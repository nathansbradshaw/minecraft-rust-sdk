//! Container-local recollection, never trusted gameplay or benchmark authority.
use crate::{Journal, Observation, Result, digest, ensure, now};
use rusqlite::{Connection, params};
use serde_json::{Value, json};

pub(crate) fn initialize(db: &Connection) -> Result<()> {
    db.execute_batch("BEGIN IMMEDIATE;
        CREATE INDEX IF NOT EXISTS events_kind_id ON events(kind,id);
        CREATE TABLE IF NOT EXISTS memory_sources(memory_id INTEGER PRIMARY KEY, origin TEXT NOT NULL, provenance TEXT NOT NULL);
        CREATE VIRTUAL TABLE IF NOT EXISTS memory_search USING fts5(text, tokenize='porter unicode61');
        CREATE TRIGGER IF NOT EXISTS memory_search_insert AFTER INSERT ON memory BEGIN INSERT INTO memory_search(rowid,text) VALUES(new.id,new.text); END;
        CREATE TRIGGER IF NOT EXISTS memory_search_update AFTER UPDATE OF text ON memory BEGIN DELETE FROM memory_search WHERE rowid=old.id; INSERT INTO memory_search(rowid,text) VALUES(new.id,new.text); END;
        INSERT INTO memory_search(rowid,text) SELECT id,text FROM memory WHERE id NOT IN (SELECT rowid FROM memory_search);
        CREATE TABLE IF NOT EXISTS observed_places(key TEXT PRIMARY KEY, memory_id INTEGER NOT NULL, signature TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS container_inspections(instance TEXT NOT NULL,actor TEXT NOT NULL,dimension TEXT NOT NULL,origin_key TEXT NOT NULL,action_id TEXT NOT NULL,game_time INTEGER NOT NULL,at INTEGER NOT NULL,snapshot TEXT NOT NULL,PRIMARY KEY(instance,actor,action_id));
        CREATE INDEX IF NOT EXISTS container_inspections_scope ON container_inspections(instance,actor,dimension,game_time DESC);
        CREATE TABLE IF NOT EXISTS search_observations(id INTEGER PRIMARY KEY,key TEXT UNIQUE NOT NULL,instance TEXT NOT NULL,actor TEXT NOT NULL,dimension TEXT NOT NULL,seen_at INTEGER NOT NULL,observations INTEGER NOT NULL,fact TEXT NOT NULL,context TEXT NOT NULL);
        CREATE INDEX IF NOT EXISTS search_observations_scope ON search_observations(instance,actor,dimension,seen_at DESC);
        COMMIT;")?;
    Ok(())
}

fn terms(query: &str) -> Vec<String> {
    let mut words = std::collections::BTreeSet::new();
    for word in query.split(|c: char| !c.is_alphanumeric()).take(256) {
        let word = word.to_lowercase();
        if word.len() >= 2
            && word.len() <= 64
            && ![
                "the", "and", "from", "with", "that", "this", "then", "please", "resume", "into",
                "both", "have", "has", "was", "are", "for", "its", "can", "get", "use", "verify",
            ]
            .contains(&word.as_str())
        {
            words.insert(word);
        }
        if words.len() == 24 {
            break;
        }
    }
    words.into_iter().collect()
}

fn preview(text: &str, bytes: usize) -> String {
    if text.len() <= bytes {
        return text.to_owned();
    }
    let mut end = bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{} [historical text truncated]", &text[..end])
}

/// Recall order is priority order. Generic history shrinking keeps the tail,
/// which would discard the executed-outcome prefix. Never shorten bindings.
pub(crate) fn fit_view(value: &mut Value, budget: usize) {
    if let Some(rows) = value.as_array_mut() {
        while rows.len() > 1 && serde_json::to_vec(rows).map_or(usize::MAX, |v| v.len()) > budget {
            rows.pop();
        }
        if serde_json::to_vec(rows).map_or(usize::MAX, |v| v.len()) > budget
            && let Some(text) = rows.first().and_then(|row| row["text"].as_str())
        {
            rows[0]["text"] = json!(preview(text, 160));
        }
    }
}

impl Journal {
    pub(crate) fn remember_from(
        &self,
        kind: &str,
        text: &str,
        origin: &str,
        provenance: &Value,
    ) -> Result<i64> {
        ensure(
            ["fact", "report", "hypothesis", "episode", "skill"].contains(&kind),
            "Memory kind required",
        )?;
        ensure(
            !text.trim().is_empty() && text.len() <= 16384,
            "Memory exceeds budget or is empty",
        )?;
        ensure(
            provenance.to_string().len() <= 3072,
            "Bounded memory provenance required",
        )?;
        let tx = self.db.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO memory(at,kind,text) VALUES(?1,?2,?3)",
            params![now(), kind, text],
        )?;
        let id = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO memory_sources(memory_id,origin,provenance) VALUES(?1,?2,?3)",
            params![id, origin, provenance.to_string()],
        )?;
        tx.commit()?;
        Ok(id)
    }

    pub fn remember(&self, kind: &str, text: &str) -> Result<()> {
        // Even a model note named 'fact' cannot manufacture SDK provenance.
        self.remember_from(
            kind,
            text,
            "agent_note",
            &json!({"authority":"agent-authored note; not native evidence"}),
        )?;
        Ok(())
    }

    pub fn recall(&self, query: &str) -> Result<Value> {
        ensure(query.len() <= 4096, "Bounded memory query required")?;
        let words = terms(query);
        let select = "SELECT m.id,m.at,m.kind,m.text,coalesce(s.origin,'legacy_agent_note'),coalesce(s.provenance,'{}') FROM memory m LEFT JOIN memory_sources s ON s.memory_id=m.id";
        let fts = words
            .iter()
            .map(|w| format!("\"{w}\""))
            .collect::<Vec<_>>()
            .join(" OR ");
        let mut result = Vec::new();
        let mut versions = std::collections::HashSet::new();
        // Separate pools prevent many exact-goal historical file writes from
        // excluding actual experiments. Two slots leave room for facts/reports;
        // receipt references describe historical effects, never reliable skills.
        for executed in [true, false] {
            let filter = if executed {
                "coalesce(s.origin,'')='program_experiment'"
            } else {
                "coalesce(s.origin,'')!='program_experiment'"
            };
            let rank = if executed {
                "CASE WHEN json_array_length(json_extract(s.provenance,'$.native_receipt_references'))>0 THEN 0 ELSE 1 END,"
            } else {
                ""
            };
            let sql = if fts.is_empty() {
                format!("{select} WHERE {filter} ORDER BY {rank}m.id DESC LIMIT 16")
            } else {
                format!(
                    "{select} JOIN memory_search ON memory_search.rowid=m.id WHERE memory_search MATCH ?1 AND {filter} ORDER BY {rank}bm25(memory_search),m.id DESC LIMIT 16"
                )
            };
            let mut stmt = self.db.prepare(&sql)?;
            let mut rows = if fts.is_empty() {
                stmt.query([])?
            } else {
                stmt.query([&fts])?
            };
            let mut selected = 0;
            while let Some(row) = rows.next()? {
                let provenance: Value = serde_json::from_str(&row.get::<_, String>(5)?)?;
                if executed
                    && let Some(version) = provenance["version"].as_str()
                    && versions.contains(version)
                {
                    continue;
                }
                let value = json!({"id":row.get::<_,i64>(0)?,"at":row.get::<_,i64>(1)?,"kind":row.get::<_,String>(2)?,"text":preview(&row.get::<_,String>(3)?,600),"origin":row.get::<_,String>(4)?,"provenance":provenance,"binding":"historical; reobserve before using any location, menu, entity or inventory claim","native_authority":false});
                if serde_json::to_vec(&result)?.len() + value.to_string().len() + 2 > 4800 {
                    continue;
                }
                if executed && let Some(version) = value["provenance"]["version"].as_str() {
                    versions.insert(version.to_owned());
                }
                result.push(value);
                selected += 1;
                if result.len() == 8 || (executed && selected == 2) {
                    break;
                }
            }
        }
        Ok(json!(result))
    }

    /// Store historical static discoveries, not live entity/menu handles. Only
    /// changed observations create a new version; repeated clock ticks do not.
    pub fn remember_observation(&self, instance: &str, o: &Observation) -> Result<()> {
        let p = &o.player;
        let provenance = json!({"instance_id":instance,"player_uuid":p["uuid"],"dimension":p["dimension"],"revision":o.revision,"sample_clock":o.world["sample_clock"],"authority":"SDK observation as seen by this client; historical, not current or external proof"});
        let mut blocks: Vec<_> = o.world["blocks"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|b| b["block"] != "minecraft:air" && b["block"].is_string())
            .collect();
        let distance = |b: &&Value| {
            (0..3)
                .map(|i| {
                    (b["position"][i].as_f64().unwrap_or(1e9)
                        - p["position"][i].as_f64().unwrap_or(0.))
                    .powi(2)
                })
                .sum::<f64>()
        };
        blocks.sort_by(|a, b| distance(a).total_cmp(&distance(b)));
        for block in blocks.into_iter().take(32) {
            let key = digest(
                json!([instance, p["dimension"], block["position"]])
                    .to_string()
                    .as_bytes(),
            );
            let signature = digest(
                json!([block["block"], block["state"]])
                    .to_string()
                    .as_bytes(),
            );
            let previous = self.db.query_row(
                "SELECT signature FROM observed_places WHERE key=?1",
                [&key],
                |r| r.get::<_, String>(0),
            );
            match previous {
                Ok(old) if old == signature => continue,
                Ok(_) | Err(rusqlite::Error::QueryReturnedNoRows) => {}
                Err(e) => return Err(e.into()),
            }
            let text = format!(
                "Observed {} at {} in dimension {}",
                block["block"].as_str().unwrap_or("unknown"),
                block["position"],
                p["dimension"]
            );
            let id = self.remember_from("fact", &text, "sdk_observation", &provenance)?;
            self.db.execute("INSERT INTO observed_places(key,memory_id,signature) VALUES(?1,?2,?3) ON CONFLICT(key) DO UPDATE SET memory_id=excluded.memory_id,signature=excluded.signature",params![key,id,signature])?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn executed_outcomes_survive_many_exact_goal_unexecuted_files_after_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("brain.sqlite");
        let j = Journal::open(&path).unwrap();
        j.remember_from("skill", "stone pickaxe: block changed, retained inventory unchanged", "program_experiment",
            &json!({"version":"executed","status":"candidate","native_receipt_references":[{"action_id":"own-finish","kind":"dig"}]})).unwrap();
        // Duplicate executions must not occupy both reserved source slots.
        j.remember_from("skill", "stone pickaxe: same program run", "program_experiment",
            &json!({"version":"executed","status":"candidate","native_receipt_references":[{"action_id":"own-finish","kind":"dig"}]})).unwrap();
        j.remember_from(
            "skill",
            "stone pickaxe: compiler failure",
            "program_experiment",
            &json!({"version":"failed","status":"candidate","native_receipt_references":[]}),
        )
        .unwrap();
        for _ in 0..40 {
            j.remember_from(
                "skill",
                "stone pickaxe stone pickaxe stone pickaxe generated file",
                "historical_program_file",
                &json!({"version":"unexecuted","status":"file only, not proof of compilation"}),
            )
            .unwrap();
        }
        j.remember("report", "stone pickaxe requested by a player")
            .unwrap();
        drop(j);
        let j = Journal::open_read_only(&path).unwrap();
        let result = j.recall("stone pickaxe").unwrap();
        let rows = result.as_array().unwrap();
        assert_eq!(rows[0]["origin"], "program_experiment");
        assert_eq!(rows[0]["provenance"]["version"], "executed");
        assert_eq!(rows[1]["provenance"]["version"], "failed");
        assert!(
            rows.iter()
                .skip(2)
                .any(|row| row["origin"] != "program_experiment")
        );
        assert!(rows.iter().all(|row| row["native_authority"] == false));
        assert!(result.to_string().len() <= 4800);
        assert!(
            j.recall("unrelated nonexistent subject")
                .unwrap()
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn context_compaction_keeps_priority_prefix_and_exact_evidence_bindings() {
        let first = json!({"origin":"program_experiment","text":"actual outcome ".repeat(30),
            "provenance":{"version":"complete-source-version","native_receipt_references":[{"action_id":"exact-own-action-id","kind":"dig"}]},"native_authority":false});
        let mut rows = vec![first.clone()];
        rows.extend(
            (0..7).map(
                |_| json!({"origin":"historical_program_file","text":"unexecuted ".repeat(60)}),
            ),
        );
        let mut value = json!(rows);
        fit_view(&mut value, 1000);
        assert_eq!(value[0], first);
        assert!(value.to_string().len() <= 1000);
        assert!(!value.as_array().unwrap().is_empty());
        fit_view(&mut value, 100);
        assert_eq!(value[0]["provenance"], first["provenance"]);
        assert_eq!(value[0]["native_authority"], false);
    }
    #[test]
    fn different_goal_phrasing_retrieves_labelled_memory_after_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("brain.sqlite");
        {
            let j = Journal::open(&path).unwrap();
            j.remember(
                "report",
                "A player reports spruce logs north of the village",
            )
            .unwrap();
            j.remember(
                "hypothesis",
                "Crafting planks could unlock a wooden pickaxe",
            )
            .unwrap();
            j.remember("fact", "A player claims entity7 carries diamonds")
                .unwrap();
        }
        let j = Journal::open(&path).unwrap();
        let rows = j.recall("Please collect a spruce log then return").unwrap();
        assert_eq!(rows[0]["kind"], "report");
        assert_eq!(rows[0]["origin"], "agent_note");
        assert_eq!(
            j.recall("Resume crafting the table and wooden pickaxe")
                .unwrap()[0]["kind"],
            "hypothesis"
        );
        let claimed = j.recall("diamonds").unwrap();
        assert_eq!(claimed[0]["origin"], "agent_note");
        assert_eq!(claimed[0]["native_authority"], false);
        assert!(j.recall("\" OR memory_search:* [ ]").is_ok());
    }
    #[test]
    fn discoveries_are_actor_scoped_historical_and_do_not_persist_live_handles() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("brain.sqlite");
        let o = Observation {
            revision: "sample-one".into(),
            player: json!({"uuid":"own","dimension":"mod:forest","position":[1,64,2],"inventory":[{"slot":1,"item":"mod:berry","count":3}]}),
            world: json!({"blocks":[{"block":"mod:tree","state":"grown","position":[1,64,2]}],"entities":[{"entity_id":7}],"sample_clock":{"epoch":"server","nanos":"100"}}),
            menu: json!({"menu_id":9}),
        };
        {
            let j = Journal::open(&path).unwrap();
            j.remember_observation("first-instance", &o).unwrap();
            j.remember_observation("first-instance", &o).unwrap();
            assert_eq!(j.recall("tree").unwrap().as_array().unwrap().len(), 1);
        }
        let j = Journal::open(&path).unwrap();
        let remembered = j.recall("tree").unwrap();
        assert_eq!(remembered[0]["origin"], "sdk_observation");
        assert_eq!(remembered[0]["provenance"]["dimension"], "mod:forest");
        assert!(!remembered.to_string().contains("entity_id"));
        assert!(!remembered.to_string().contains("menu_id"));
        j.remember_observation("second-instance", &o).unwrap();
        assert_eq!(j.recall("tree").unwrap().as_array().unwrap().len(), 2);
    }
    #[test]
    fn retrieval_is_byte_bounded_without_cutting_provenance_or_identifiers() {
        let dir = tempfile::tempdir().unwrap();
        let j = Journal::open(&dir.path().join("brain.sqlite")).unwrap();
        for _ in 0..24 {
            j.remember("report", &format!("wood {}", "🪵".repeat(3000)))
                .unwrap();
        }
        let result = j.recall("wood").unwrap();
        assert!(result.to_string().len() <= 4800);
        assert!(!result.as_array().unwrap().is_empty());
        for row in result.as_array().unwrap() {
            assert_eq!(row["origin"], "agent_note");
            assert_eq!(row["native_authority"], false);
        }
    }
}

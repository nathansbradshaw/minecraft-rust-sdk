//! Goal-local coding handoff after repeated exhausted routes. Never gameplay authority.
use crate::{Journal, Observation, Result, digest};
use serde_json::{Value, json};

fn position(value: &Value) -> Option<[f64; 3]> {
    let p = [value[0].as_f64()?, value[1].as_f64()?, value[2].as_f64()?];
    p.iter().all(|n| n.is_finite()).then_some(p)
}
fn nearby(a: [f64; 3], b: [f64; 3]) -> bool {
    (0..3).map(|i| (a[i] - b[i]).powi(2)).sum::<f64>() <= 9.
}
fn scene(observed: &Observation, anchor: [f64; 3]) -> String {
    let mut blocks: Vec<_> = observed.world["blocks"].as_array().into_iter().flatten()
        .filter(|b| position(&b["position"]).is_some_and(|p|
            (p[0]-anchor[0].floor()).abs()<=1. && (p[2]-anchor[2].floor()).abs()<=1.
                && p[1]>=anchor[1].floor()-1. && p[1]<=anchor[1].floor()+2.))
        .map(|b| json!({"position":b["position"],"block":b["block"],"state":b["state"],"collision":b["collision"],"may_interact":b["may_interact"]}))
        .collect();
    blocks.sort_by_key(Value::to_string);
    digest(json!({"dimension":observed.player["dimension"],"inventory":observed.player["inventory"],"blocks":blocks}).to_string().as_bytes())
}
impl Journal {
    pub(crate) fn record_navigation_stall(
        &self,
        result: &Value,
        source: &str,
        revision: &str,
        observed: &Observation,
    ) -> Result<()> {
        if result["navigation"]["report"]["status"] != "no_known_route"
            || !result["navigation"]["report"]
                .get("pending_action")
                .is_some_and(Value::is_null)
            || !result["navigation"]["error"].is_null()
        {
            return Ok(());
        }
        let Some(current) = position(&observed.player["position"]) else {
            return Ok(());
        };
        let previous: Value = self
            .get("navigation_stall")?
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let continued = position(&previous["anchor"]).filter(|a| {
            previous["goal_revision"] == revision
                && previous["source_sha256"] == source
                && nearby(*a, current)
                && previous["scene_sha256"] == scene(observed, *a)
        });
        let anchor = continued.unwrap_or(current);
        let count = if continued.is_some() {
            previous["count"]
                .as_u64()
                .unwrap_or_default()
                .saturating_add(1)
        } else {
            1
        };
        let value = json!({"format":"navigation-coding-checkpoint-v1","goal_revision":revision,
            "source_sha256":source,"anchor":anchor,"count":count,"scene_sha256":scene(observed,anchor),
            "scope":"Repeated exhausted routes in unchanged nearby observed terrain/inventory; not task progress or proof that no route exists. No allowance refill."});
        self.set("navigation_stall", &value.to_string())?;
        self.event("navigation_stall_recorded", value)?;
        Ok(())
    }
    pub(crate) fn navigation_coding_checkpoint(
        &self,
        source: &str,
        revision: &str,
        observed: &Observation,
    ) -> Result<Value> {
        let value: Value = self
            .get("navigation_stall")?
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let applies = (|| -> Option<()> {
            let anchor = position(&value["anchor"])?;
            (value["count"].as_u64()? >= 2
                && value["goal_revision"] == revision
                && value["source_sha256"] == source
                && nearby(anchor, position(&observed.player["position"])?)
                && value["scene_sha256"] == scene(observed, anchor))
            .then_some(())
        })()
        .is_some()
            && self.get("pending_action")?.unwrap_or_default().is_empty();
        Ok(if applies { value } else { Value::Null })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn target_jitter_cannot_refill_unchanged_navigation_but_new_conditions_can() {
        let temp = tempfile::tempdir().unwrap();
        let j = Journal::open(&temp.path().join("brain.sqlite")).unwrap();
        let mut o = Observation {
            revision: "one".into(),
            player: json!({"dimension":"minecraft:overworld","position":[0.,64.,0.],"inventory":[]}),
            world: json!({"blocks":[{"position":[0,63,0],"block":"minecraft:stone"}]}),
            menu: Value::Null,
        };
        let failure =
            json!({"navigation":{"report":{"status":"no_known_route","pending_action":null}}});
        for report in [
            json!({"status":"no_known_route"}),
            json!({"status":"needs_reconciliation","pending_action":"id"}),
        ] {
            j.record_navigation_stall(
                &json!({"navigation":{"report":report}}),
                "source",
                "goal",
                &o,
            )
            .unwrap();
            assert!(j.get("navigation_stall").unwrap().is_none());
        }
        j.record_navigation_stall(&failure, "source", "goal", &o)
            .unwrap();
        assert!(
            j.navigation_coding_checkpoint("source", "goal", &o)
                .unwrap()
                .is_null()
        );
        o.player["position"] = json!([1., 64., 0.]);
        j.record_navigation_stall(&failure, "source", "goal", &o)
            .unwrap();
        assert_eq!(
            j.navigation_coding_checkpoint("source", "goal", &o)
                .unwrap()["count"],
            2
        );
        assert!(
            j.navigation_coding_checkpoint("changed", "goal", &o)
                .unwrap()
                .is_null()
        );
        assert!(
            j.navigation_coding_checkpoint("source", "other-goal", &o)
                .unwrap()
                .is_null()
        );
        j.set("pending_action", "unsettled-id").unwrap();
        assert!(
            j.navigation_coding_checkpoint("source", "goal", &o)
                .unwrap()
                .is_null()
        );
        j.set("pending_action", "").unwrap();
        let mut changed = o.clone();
        changed.player["inventory"] = json!([{"item":"minecraft:log","count":1}]);
        assert!(
            j.navigation_coding_checkpoint("source", "goal", &changed)
                .unwrap()
                .is_null()
        );
        changed = o.clone();
        changed.world["blocks"][0]["block"] = json!("minecraft:air");
        assert!(
            j.navigation_coding_checkpoint("source", "goal", &changed)
                .unwrap()
                .is_null()
        );
        changed = o.clone();
        changed.player["position"] = json!([4., 64., 0.]);
        assert!(
            j.navigation_coding_checkpoint("source", "goal", &changed)
                .unwrap()
                .is_null()
        );
        j.record_navigation_stall(&failure, "source", "goal", &changed)
            .unwrap();
        assert!(
            j.navigation_coding_checkpoint("source", "goal", &changed)
                .unwrap()
                .is_null()
        );
    }
}

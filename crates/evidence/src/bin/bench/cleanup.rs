//! Exact-owned NPC cleanup with a bounded loaded-chunk witness, never broad removal.
use super::*;
use std::collections::BTreeSet;
use std::time::Instant;
fn marked(text: &str) -> Option<bool> {
    let lower = text.to_ascii_lowercase();
    if lower.contains("is not marked for force loading") {
        Some(false)
    } else if lower.contains("is marked for force loading") {
        Some(true)
    } else {
        None
    }
}
fn position(text: &str) -> Option<[f64; 3]> {
    let (_, suffix) = text.split_once("has the following entity data:")?;
    let values: Vec<_> = suffix
        .trim()
        .strip_prefix('[')?
        .strip_suffix(']')?
        .split(',')
        .collect();
    if values.len() != 3 {
        return None;
    }
    let mut p = [0.; 3];
    for (v, out) in values.into_iter().zip(&mut p) {
        *out = v
            .trim()
            .trim_end_matches(['d', 'D', 'f', 'F'])
            .parse::<f64>()
            .ok()?;
        if !out.is_finite() {
            return None;
        }
    }
    Some(p)
}
// Conditional execute-if-entity skips its leaf on absence and returns complete empty
// feedback. Accept that ONLY after positive location and confirmed exact kill above.
fn absent_after_kill(text: &str) -> bool {
    text.trim().is_empty() || text.trim().eq_ignore_ascii_case("No entity was found")
}
fn settle_after_kill(
    client: &mut dyn Console,
    npc: &rcon::EntityUuid,
    case: &Path,
    budget: Duration,
    interval: Duration,
) -> Result<()> {
    let started = Instant::now();
    let mut reads = 0;
    loop {
        let label = if reads == 0 {
            "cleanup-npc-after".into()
        } else {
            format!("cleanup-npc-after-{reads}")
        };
        let after = rcon_execute(
            client,
            rcon::Command::ReadNpcPosition { npc: npc.clone() },
            case,
            &label,
        )?;
        reads += 1;
        if absent_after_kill(&after.text) {
            save(
                &case.join("cleanup-npc-settlement.json"),
                &json!({
                    "absence_confirmed":true,"read_only_queries":reads,
                    "elapsed_ms":started.elapsed().as_millis(),"kill_replayed":false
                }),
            )?;
            return Ok(());
        }
        ensure(
            position(&after.text).is_some(),
            "ambiguous NPC settlement response",
        )?;
        ensure(
            started.elapsed() < budget,
            "NPC remained present after bounded read-only settlement",
        )?;
        std::thread::sleep(interval.min(budget.saturating_sub(started.elapsed())));
        ensure(
            started.elapsed() < budget,
            "NPC absence settlement deadline exceeded",
        )?;
    }
}
pub fn run(client: &mut dyn Console, f: &Fixture, case: &Path, last: [f64; 3]) -> Result<()> {
    ensure(
        last.iter().all(|n| n.is_finite() && n.abs() <= 29_999_968.),
        "bounded last-known NPC pose required",
    )?;
    let center = (
        (last[0] / 16.).floor() as i32,
        (last[2] / 16.).floor() as i32,
    );
    let npc = rcon::EntityUuid::parse(&f.npc_uuid)?;
    let mut added = Vec::new();
    let mut possibly_added = Vec::new();
    let mut restored = BTreeSet::new();
    let result = (|| -> Result<()> {
        // One3x3 ring only: no expanding search or unbounded chunk loading.
        for (i, (x, z)) in (-1..=1)
            .flat_map(|dx| (-1..=1).map(move |dz| (center.0 + dx, center.1 + dz)))
            .enumerate()
        {
            let before = rcon_execute(
                client,
                rcon::Command::QueryChunk { x, z },
                case,
                &format!("cleanup-chunk-{i}-before"),
            )?;
            let before = marked(&before.text).ok_or("ambiguous prior force-load state")?;
            if !before {
                possibly_added.push((x, z));
                let reply = rcon_execute(
                    client,
                    rcon::Command::SetChunk { x, z, loaded: true },
                    case,
                    &format!("cleanup-chunk-{i}-add"),
                )?;
                let lower = reply.text.to_ascii_lowercase();
                ensure(
                    lower.starts_with("marked chunk ") && lower.contains("to be force loaded"),
                    "chunk add not confirmed; preserve possibly-owned load",
                )?;
                added.push((x, z));
                possibly_added.pop();
                let after = rcon_execute(
                    client,
                    rcon::Command::QueryChunk { x, z },
                    case,
                    &format!("cleanup-chunk-{i}-added-query"),
                )?;
                ensure(
                    marked(&after.text) == Some(true),
                    "added force-load not confirmed",
                )?;
            }
        }
        let found = rcon_execute(
            client,
            rcon::Command::ReadNpcPosition { npc: npc.clone() },
            case,
            "cleanup-npc-before",
        )?;
        ensure(
            position(&found.text).is_some(),
            "NPC not located in bounded loaded region; absence not proven",
        )?;
        let killed = rcon_execute(
            client,
            rcon::Command::RemoveOwnedSkeleton { npc: npc.clone() },
            case,
            "cleanup-npc-remove",
        )?;
        ensure(
            killed.text.trim().starts_with("Killed "),
            "exact skeleton kill not confirmed",
        )?;
        // A confirmed kill can leave the dying entity queryable for its normal
        // death animation. Poll reads only; the kill is never sent again.
        settle_after_kill(
            client,
            &npc,
            case,
            Duration::from_secs(2),
            Duration::from_millis(100),
        )
    })();
    // Restore only loads proven absent-before and successfully added by this run.
    // Never replay an uncertain add/remove, never remove pre-existing loads.
    for (i, &(x, z)) in added.iter().enumerate() {
        let released = (|| -> Result<()> {
            let reply = rcon_execute(
                client,
                rcon::Command::SetChunk {
                    x,
                    z,
                    loaded: false,
                },
                case,
                &format!("cleanup-owned-{i}-remove"),
            )?;
            let lower = reply.text.to_ascii_lowercase();
            ensure(
                lower.starts_with("unmarked chunk ") && lower.contains("force loading"),
                "owned load removal not confirmed",
            )?;
            let after = rcon_execute(
                client,
                rcon::Command::QueryChunk { x, z },
                case,
                &format!("cleanup-owned-{i}-query"),
            )?;
            ensure(
                marked(&after.text) == Some(false),
                "owned force-load release not confirmed",
            )
        })();
        if released.is_ok() {
            restored.insert((x, z));
        }
    }
    let complete = result.is_ok() && restored.len() == added.len() && possibly_added.is_empty();
    save(
        &case.join("cleanup-result.json"),
        &json!({"verified_exact_npc_removed":result.is_ok(),"last_known_npc_position":last,"bounded_ring_center_chunk":[center.0,center.1],"ring_radius_chunks":1,"owned_added_chunks":added,"possibly_added_uncertain_chunks":possibly_added,"owned_released_chunks":restored,"complete":complete,"preexisting_loads_removed":false,"scope":"English vanilla response contract plus exact typed UUID/type commands; ambiguity is incomplete, no retry or expanding search"}),
    )?;
    ensure(
        complete,
        "NPC cleanup incomplete; preserve receipts and owned-load ledger",
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    struct Fake {
        loads: BTreeSet<(i32, i32)>,
        initial: BTreeSet<(i32, i32)>,
        removed: Vec<(i32, i32)>,
        alive: bool,
        death_frames: usize,
        kills: usize,
    }
    impl Console for Fake {
        fn execute(&mut self, c: &rcon::Command) -> std::result::Result<rcon::Reply, rcon::Error> {
            let text = match c {
                rcon::Command::QueryChunk { x, z } => if self.loads.contains(&(*x, *z)) {
                    "Chunk is marked for force loading"
                } else {
                    "Chunk is not marked for force loading"
                }
                .into(),
                rcon::Command::SetChunk { x, z, loaded: true } => {
                    self.loads.insert((*x, *z));
                    "Marked chunk [x,z] to be force loaded".into()
                }
                rcon::Command::SetChunk {
                    x,
                    z,
                    loaded: false,
                } => {
                    self.loads.remove(&(*x, *z));
                    self.removed.push((*x, *z));
                    "Unmarked chunk [x,z] for force loading".into()
                }
                rcon::Command::ReadNpcPosition { .. } => if self.alive || self.death_frames > 0 {
                    if !self.alive {
                        self.death_frames -= 1;
                    }
                    "Skeleton has the following entity data: [262.5d,64.0d,-709.5d]"
                } else {
                    "No entity was found"
                }
                .into(),
                rcon::Command::RemoveOwnedSkeleton { .. } => {
                    self.alive = false;
                    self.kills += 1;
                    "Killed Skeleton".into()
                }
                _ => panic!("unexpected console command"),
            };
            Ok(rcon::Reply {
                request_id: 1,
                text,
            })
        }
    }
    #[test]
    fn bounded_cleanup_preserves_preexisting_forceloads() {
        let s = super::super::tests::setup();
        let initial = BTreeSet::from([(16, -45)]);
        let mut c = Fake {
            loads: initial.clone(),
            initial,
            removed: vec![],
            alive: true,
            death_frames: 0,
            kills: 0,
        };
        run(&mut c, &s.f, &s.case, s.f.expected_npc_position).unwrap();
        assert_eq!(c.loads, c.initial);
        assert_eq!(c.removed.len(), 8);
        assert!(!c.removed.contains(&(16, -45)));
        assert_eq!(
            json(&s.case.join("cleanup-result.json"), 65536).unwrap()["complete"],
            true
        );
    }
    #[test]
    fn unlocated_npc_is_incomplete_and_owned_loads_are_restored() {
        let s = super::super::tests::setup();
        let mut c = Fake {
            loads: BTreeSet::new(),
            initial: BTreeSet::new(),
            removed: vec![],
            alive: false,
            death_frames: 0,
            kills: 0,
        };
        assert!(run(&mut c, &s.f, &s.case, s.f.expected_npc_position).is_err());
        assert!(c.loads.is_empty());
        assert_eq!(
            json(&s.case.join("cleanup-result.json"), 65536).unwrap()["complete"],
            false
        );
    }
    #[test]
    fn confirmed_kill_waits_for_death_animation_without_another_kill() {
        let s = super::super::tests::setup();
        let initial = BTreeSet::from([(16, -45)]);
        let mut c = Fake {
            loads: initial.clone(),
            initial,
            removed: vec![],
            alive: true,
            death_frames: 2,
            kills: 0,
        };
        run(&mut c, &s.f, &s.case, s.f.expected_npc_position).unwrap();
        assert_eq!(c.kills, 1);
        assert_eq!(c.loads, c.initial);
        let result = json(&s.case.join("cleanup-npc-settlement.json"), 65536).unwrap();
        assert_eq!(result["read_only_queries"], 3);
        assert_eq!(result["kill_replayed"], false);
    }
    #[test]
    fn settlement_deadline_keeps_presence_uncertain_without_mutation() {
        let s = super::super::tests::setup();
        let mut c = Fake {
            loads: BTreeSet::new(),
            initial: BTreeSet::new(),
            removed: vec![],
            alive: false,
            death_frames: usize::MAX,
            kills: 0,
        };
        let npc = rcon::EntityUuid::parse(&s.f.npc_uuid).unwrap();
        assert!(
            settle_after_kill(
                &mut c,
                &npc,
                &s.case,
                Duration::ZERO,
                Duration::from_millis(1)
            )
            .is_err()
        );
        assert_eq!(c.kills, 0);
    }
}

//! Optional caller-owned acquisition completion, never a positional arrival or
//! permission to replay an interrupted mutation.
use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetainedInventory {
    pub item: String,
    pub minimum: u64,
}
impl RetainedInventory {
    pub fn validate(&self) -> Result<()> {
        if !registry_id(&self.item) || !(1..=1_000_000).contains(&self.minimum) {
            return Err(error(
                ErrorCode::InvalidRequest,
                "Inventory condition requires a runtime item ID and minimum1..1000000",
            ));
        }
        Ok(())
    }
}
/// A fresh retained-count check. It does not assert a pickup event, positional
/// arrival, successful movement, or the parent task's completion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InventoryCompletion {
    pub condition: RetainedInventory,
    pub observed_count: u64,
    pub condition_satisfied: bool,
    pub owned_input_settled: bool,
    pub settled_action_id: Option<String>,
    pub input_settlement: String,
    pub observation_revision: String,
    pub actor: String,
    pub life: String,
    pub sample_clock: serde_json::Value,
}
fn text<'a>(value: &'a serde_json::Value, field: &str) -> Result<&'a str> {
    value[field]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            error(
                ErrorCode::NotReady,
                format!("Inventory condition needs {field}"),
            )
        })
}
fn validate_fresh(
    prior: &Observation,
    fresh: &Observation,
    actor: &str,
    dimension: &str,
) -> Result<()> {
    if text(&fresh.player, "uuid")? != actor
        || text(&prior.player, "uuid")? != actor
        || text(&fresh.player, "life_id")? != text(&prior.player, "life_id")?
        || text(&fresh.player, "dimension")? != dimension
        || text(&fresh.world["sample_clock"], "epoch")?
            != text(&prior.world["sample_clock"], "epoch")?
    {
        return Err(error(
            ErrorCode::Conflict,
            "Inventory completion changed actor, life, dimension or sample epoch; reobserve without certifying the condition",
        ));
    }
    let nanos = |o: &Observation| -> Result<u64> {
        text(&o.world["sample_clock"], "nanos")?
            .parse()
            .map_err(|_| {
                error(
                    ErrorCode::NotReady,
                    "Inventory completion clock unavailable",
                )
            })
    };
    if nanos(fresh)? < nanos(prior)? || fresh.revision.is_empty() {
        return Err(error(
            ErrorCode::NotReady,
            "Inventory completion sample is regressive or unbound",
        ));
    }
    Ok(())
}
fn release_verified(id: &str, receipt: &Receipt, fresh: &Observation) -> bool {
    receipt.action_id == id
        && receipt.outcome.terminal()
        && (receipt.evidence["input_released"] == true
            || (fresh.player["control"]["action_id"] == id
                && fresh.player["control"]["input_released"] == true
                && matches!(
                    fresh.player["control"]["outcome"].as_str(),
                    Some("observed" | "cancelled" | "expired" | "failed")
                )))
}
fn local_no_dispatch(last: &Attempt) -> bool {
    last.outcome == Some(Outcome::Rejected)
        && last.arrival_verification.as_ref().is_some_and(|proof| {
            proof["kind"] == "local_pre_dispatch_rejection"
                && proof["gateway_dispatch_attempted"] == false
                && proof["native_effect_verified"] == false
        })
}
pub(super) async fn check(
    client: &Client,
    j: &mut Journal,
    observed: &mut Observation,
) -> Result<Option<Report>> {
    let Some(condition) = j.state.until_inventory.clone() else {
        return Ok(None);
    };
    let at_position = j.state.goal.gap(j.state.position) <= 0.001;
    if menu::item_count(observed, &condition.item)? < condition.minimum && !at_position {
        return Ok(None);
    }
    // Cached counts only select this read. Only this typed fresh sample may
    // satisfy the opt-in condition, and it cannot cross lives or sample epochs.
    let fresh = menu::observe_fresh(client).await?;
    validate_fresh(observed, &fresh, &j.state.actor, j.state.goal.dimension())?;
    *observed = fresh;
    let fresh = &*observed;
    let pose = observed_pose(fresh)?;
    let count = menu::item_count(fresh, &condition.item)?;
    j.state.position = pose.position;
    let at_position = j.state.goal.gap(pose.position) <= 0.001;
    let mut evidence = InventoryCompletion {
        condition: condition.clone(),
        observed_count: count,
        condition_satisfied: count >= condition.minimum,
        owned_input_settled: false,
        settled_action_id: None,
        input_settlement: "not_verified".into(),
        observation_revision: fresh.revision.clone(),
        actor: text(&fresh.player, "uuid")?.into(),
        life: text(&fresh.player, "life_id")?.into(),
        sample_clock: fresh.world["sample_clock"].clone(),
    };
    // A new journal/criterion cannot silently bypass an older in-flight lane.
    // This may be a protective body's control: expose the exact ID for read-only
    // inspection, never cancel or take ownership of it here.
    if let Some(id) = fresh.player["control"]["action_id"].as_str()
        && fresh.player["control"]["input_released"] != true
    {
        j.save()?;
        let mut report = j.report("needs_reconciliation", "A current control still holds or has unknown input release. Inspect its exact ID; the inventory condition cannot bypass an earlier lane or cancel a different controller.");
        report.pending_action = Some(id.into());
        report.inventory_completion = Some(evidence);
        return Ok(Some(report));
    }
    if !evidence.condition_satisfied && !at_position {
        return Ok(None);
    }
    // run() processes the original pending intention before reaching this
    // function. Recheck the latest exact owned ID: terminal alone is not proof
    // that held input was released. No cancel, new movement or replay is sent.
    if let Some(last) = j.state.history.last() {
        let id = last.request.action_id.clone();
        let local = local_no_dispatch(last);
        let released = if local {
            true
        } else {
            match client.action_status(&id).await {
                Ok(receipt) => release_verified(&id, &receipt, fresh),
                Err(e) if e.code == ErrorCode::NotFound => false,
                Err(e) => return Err(e),
            }
        };
        if !released {
            j.save()?;
            let mut report=j.report("needs_reconciliation","Release of the exact prior movement ID is unverified. Inspect/reconcile that ID; do not replay it or treat this approach as settled.");
            report.pending_action = Some(id);
            report.inventory_completion = Some(evidence);
            return Ok(Some(report));
        }
        evidence.settled_action_id = Some(id);
        evidence.input_settlement = if local {
            "local_pre_dispatch_rejection_no_input"
        } else {
            "original_terminal_release"
        }
        .into();
    } else {
        evidence.input_settlement = "no_prior_owned_intention".into();
    }
    evidence.owned_input_settled = true;
    j.save()?;
    if !evidence.condition_satisfied {
        let mut report=j.report("inventory_missing","The approach reached its distance, but the explicit retained-inventory condition is unmet in a fresh sample and owned input is settled. Inspect the actual item/drop or choose another approach; no resource acquisition or parent-goal success is implied.");
        report.inventory_completion = Some(evidence);
        return Ok(Some(report));
    }
    let mut report=j.report("inventory_satisfied","Fresh same-actor/life retained inventory satisfies the caller condition and owned input is settled. This is not positional arrival, a new pickup event, native movement-effect credit or parent-task certification.");
    report.inventory_completion = Some(evidence);
    Ok(Some(report))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn observation(life: &str, nanos: &str) -> Observation {
        Observation {
            revision: "fresh".into(),
            player: serde_json::json!({"uuid":"actor","life_id":life,"dimension":"fixture:world"}),
            world: serde_json::json!({"sample_clock":{"epoch":"clock","nanos":nanos}}),
            menu: serde_json::Value::Null,
        }
    }
    #[test]
    fn acquisition_resume_conditions_are_explicit_bounded_and_strict() {
        for (item, minimum) in [("bad", 1), ("fixture:item", 0), ("fixture:item", 1_000_001)] {
            assert!(
                RetainedInventory {
                    item: item.into(),
                    minimum
                }
                .validate()
                .is_err()
            );
        }
        assert!(
            RetainedInventory {
                item: "fixture:item".into(),
                minimum: 1
            }
            .validate()
            .is_ok()
        );
        assert!(
            serde_json::from_str::<RetainedInventory>(
                r#"{"item":"fixture:item","minimum":1,"actor":"foreign"}"#
            )
            .is_err()
        );
        assert!(Options::default().until_inventory.is_none());
    }
    #[test]
    fn acquisition_resume_fresh_proof_never_crosses_lives_or_regresses() {
        let prior = observation("life1", "2");
        assert!(validate_fresh(&prior, &prior, "actor", "fixture:world").is_ok());
        assert!(
            validate_fresh(&prior, &observation("life2", "3"), "actor", "fixture:world").is_err()
        );
        assert!(
            validate_fresh(&prior, &observation("life1", "1"), "actor", "fixture:world").is_err()
        );
        assert!(validate_fresh(&prior, &prior, "other", "fixture:world").is_err());
        let mut missing = prior.clone();
        missing.player.as_object_mut().unwrap().remove("life_id");
        assert!(validate_fresh(&missing, &prior, "actor", "fixture:world").is_err());
    }
    #[test]
    fn acquisition_resume_terminal_receipt_requires_exact_release_not_effect_credit() {
        let mut fresh = observation("life1", "3");
        let mut receipt = Receipt {
            action_id: "owned".into(),
            outcome: Outcome::Observed,
            adapter_version: "fixture".into(),
            evidence: serde_json::json!({"native_effect_verified":true}),
            error: None,
        };
        assert!(!release_verified("owned", &receipt, &fresh));
        fresh.player["control"] =
            serde_json::json!({"action_id":"owned","outcome":"observed","input_released":true});
        assert!(release_verified("owned", &receipt, &fresh));
        receipt.action_id = "foreign".into();
        assert!(!release_verified("owned", &receipt, &fresh));
        receipt.action_id = "owned".into();
        receipt.outcome = Outcome::Running;
        receipt.evidence["input_released"] = serde_json::json!(true);
        assert!(!release_verified("owned", &receipt, &fresh));
        receipt.outcome = Outcome::Cancelled;
        receipt.evidence["native_effect_verified"] = serde_json::json!(false);
        assert!(
            release_verified("owned", &receipt, &fresh),
            "release can settle ownership without claiming movement effects"
        );
    }
    #[test]
    fn acquisition_resume_local_no_dispatch_requires_exact_retained_provenance() {
        let mut attempt = Attempt {
            request: ActionRequest {
                action_id: "local-only".into(),
                action: Action::StartMove {
                    target: Position {
                        x: 1.,
                        y: 1.,
                        z: 1.,
                    },
                    jump: false,
                    max_ticks: 20,
                },
            },
            origin: [0., 1., 0.],
            source_node: None,
            scene: "scene".into(),
            target: [1., 1., 1.],
            outcome: Some(Outcome::Rejected),
            arrival_verification: Some(
                serde_json::json!({"kind":"local_pre_dispatch_rejection","native_effect_verified":false,"gateway_dispatch_attempted":false}),
            ),
        };
        assert!(local_no_dispatch(&attempt));
        attempt.arrival_verification.as_mut().unwrap()["gateway_dispatch_attempted"] =
            serde_json::json!(true);
        assert!(!local_no_dispatch(&attempt));
        attempt.arrival_verification.as_mut().unwrap()["gateway_dispatch_attempted"] =
            serde_json::json!(false);
        attempt.outcome = Some(Outcome::Uncertain);
        assert!(!local_no_dispatch(&attempt));
        attempt.outcome = Some(Outcome::Rejected);
        attempt.arrival_verification = None;
        assert!(!local_no_dispatch(&attempt));
    }
}

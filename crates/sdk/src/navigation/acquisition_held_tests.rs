// Included in navigation::tests to reuse its real SDK HTTP transport fixtures.
#[tokio::test]
async fn acquisition_held_lost_cancel_reply_resumes_original_id_without_repost() {
    for resolve in [false, true] {
        let root = Temp::new();
        let path = root.0.join("lost-cancel.json");
        let condition = RetainedInventory {
            minimum: 3,
            ..acquisition_condition()
        };
        acquisition_prepare_held(&path, Some(condition.clone()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let c = client(format!("http://{}/", listener.local_addr().unwrap()));
        let server = tokio::spawn(async move {
            let before = acquisition_held_sample(2, "original-pickup-leg", [0.5, 1., 0.5]);
            let picked = acquisition_held_sample(3, "original-pickup-leg", [0.5, 1., 0.5]);
            respond(&listener, "GET /v1/server ", acquisition_info(), 200).await;
            respond(
                &listener,
                "GET /v1/bots/fixture/observations ",
                before.clone(),
                200,
            )
            .await;
            respond(
                &listener,
                "GET /v1/bots/fixture/actions/original-pickup-leg ",
                acquisition_receipt("original-pickup-leg", "running", before),
                200,
            )
            .await;
            respond(
                &listener,
                "GET /v1/bots/fixture/observations ",
                picked.clone(),
                200,
            )
            .await;
            respond(&listener, "POST /v1/bots/fixture/observations/query ", serde_json::json!({"format":"minecraft-fresh-observation-v1","observation":picked.clone()}), 200).await;
            let (stream, request) = request(&listener, "POST /v1/bots/fixture/actions ").await;
            assert_eq!(
                request["action_id"],
                "original-pickup-leg-acquisition-cancel"
            );
            assert_eq!(request["action"]["action_id"], "original-pickup-leg");
            reply(
                stream,
                serde_json::to_value(ApiError::new(
                    ErrorCode::Internal,
                    "Cancel reply lost; caller cannot infer dispatch outcome",
                ))
                .unwrap(),
                500,
            )
            .await;
            respond(&listener, "GET /v1/server ", acquisition_info(), 200).await;
            respond(
                &listener,
                "GET /v1/bots/fixture/observations ",
                picked.clone(),
                200,
            )
            .await;
            if resolve {
                respond(
                    &listener,
                    "GET /v1/bots/fixture/actions/original-pickup-leg-acquisition-cancel ",
                    acquisition_receipt(
                        "original-pickup-leg-acquisition-cancel",
                        "observed",
                        serde_json::Value::Null,
                    ),
                    200,
                )
                .await;
                let mut released = picked;
                released["player"]["control"] = serde_json::json!({"action_id":"original-pickup-leg","outcome":"cancelled","input_released":true});
                let original =
                    acquisition_receipt("original-pickup-leg", "cancelled", released.clone());
                respond(
                    &listener,
                    "GET /v1/bots/fixture/actions/original-pickup-leg ",
                    original.clone(),
                    200,
                )
                .await;
                respond(
                    &listener,
                    "GET /v1/bots/fixture/observations ",
                    released.clone(),
                    200,
                )
                .await;
                respond(&listener, "POST /v1/bots/fixture/observations/query ", serde_json::json!({"format":"minecraft-fresh-observation-v1","observation":released}), 200).await;
                respond(
                    &listener,
                    "GET /v1/bots/fixture/actions/original-pickup-leg ",
                    original,
                    200,
                )
                .await;
            } else {
                respond(
                    &listener,
                    "GET /v1/bots/fixture/actions/original-pickup-leg-acquisition-cancel ",
                    serde_json::to_value(ApiError::new(
                        ErrorCode::NotFound,
                        "Unknown cancellation; no retry",
                    ))
                    .unwrap(),
                    404,
                )
                .await;
            }
            assert!(
                tokio::time::timeout(Duration::from_millis(100), listener.accept())
                    .await
                    .is_err(),
                "neither original nor cancellation may be replayed"
            );
        });
        let options = Options {
            until_inventory: Some(condition.clone()),
            timeout_ms: 2000,
            ..Options::default()
        };
        assert!(navigate(&c, goal(), &path, options.clone()).await.is_err());
        {
            let j = Journal::open_with_condition(
                &path,
                goal(),
                "instance",
                "actor",
                [0.5, 1., 0.5],
                Some(condition.clone()),
            )
            .unwrap();
            let original = j.state.pending.as_ref().unwrap();
            assert_eq!(original.request.action_id, "original-pickup-leg");
            let cancel = original.cancellation.as_ref().unwrap();
            assert_eq!(
                cancel.request.action_id,
                "original-pickup-leg-acquisition-cancel"
            );
            assert!(cancel.outcome.is_none());
        }
        let report = navigate(&c, goal(), &path, options).await.unwrap();
        server.await.unwrap();
        assert_eq!(
            report.status,
            if resolve {
                "inventory_satisfied"
            } else {
                "needs_reconciliation"
            }
        );
        if !resolve {
            assert_eq!(
                report.pending_action.as_deref(),
                Some("original-pickup-leg-acquisition-cancel")
            );
            assert!(report.inventory_completion.is_none());
        }
    }
}

#[tokio::test]
async fn acquisition_held_legacy_v2_pending_upgrade_cannot_invent_a_life_basis() {
    let root = Temp::new();
    let path = root.0.join("legacy-v2.json");
    let condition = RetainedInventory {
        minimum: 3,
        ..acquisition_condition()
    };
    acquisition_prepare_held(&path, Some(condition.clone()));
    {
        let mut j = Journal::open_with_condition(
            &path,
            goal(),
            "instance",
            "actor",
            [0.5, 1., 0.5],
            Some(condition.clone()),
        )
        .unwrap();
        j.state.version = 2;
        j.state.pending.as_mut().unwrap().basis = None;
        j.save().unwrap();
    }
    let mut j = Journal::open_with_condition(
        &path,
        goal(),
        "instance",
        "actor",
        [0.5, 1., 0.5],
        Some(condition),
    )
    .unwrap();
    assert_eq!(j.state.version, 3);
    assert_eq!(
        j.state.pending.as_ref().unwrap().request.action_id,
        "original-pickup-leg"
    );
    let c = client("http://127.0.0.1:1/".into());
    acquisition::cancel_running_if_satisfied(&c, &mut j)
        .await
        .unwrap();
    assert!(
        j.state.pending.unwrap().cancellation.is_none(),
        "old pending input lacks life provenance; no network request is authorized"
    );
}

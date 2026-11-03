use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn peer(
    responses: Vec<(Outcome, &'static str)>,
) -> (Client, tokio::task::JoinHandle<Vec<String>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut client = Client::new(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    client.credential = Some(Credential {
        bot_id: "own-bot".into(),
        player_uuid: "own".into(),
        key: "test-only-scoped-key".into(),
    });
    let task = tokio::spawn(async move {
        let mut methods = Vec::new();
        for (outcome, id) in responses {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut header = Vec::new();
            while !header.ends_with(b"\r\n\r\n") {
                assert!(header.len() < 8192);
                header.push(stream.read_u8().await.unwrap());
            }
            let header = String::from_utf8(header).unwrap();
            let first = header.lines().next().unwrap().to_owned();
            methods.push(first.clone());
            if first.starts_with("POST ") {
                let size = header
                    .lines()
                    .find_map(|l| {
                        l.split_once(':')
                            .filter(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                            .map(|(_, v)| v.trim().parse::<usize>().unwrap())
                    })
                    .unwrap();
                assert!(size < 4096);
                let mut body = vec![0; size];
                stream.read_exact(&mut body).await.unwrap();
                let request: ActionRequest = serde_json::from_slice(&body).unwrap();
                assert_eq!(request.action_id, "stable-action");
            }
            let body = serde_json::to_vec(&Receipt {
                action_id: id.into(),
                outcome,
                adapter_version: "fixture".into(),
                evidence: serde_json::json!({}),
                error: None,
            })
            .unwrap();
            stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).as_bytes()).await.unwrap();
            stream.write_all(&body).await.unwrap();
        }
        methods
    });
    (client, task)
}
fn action() -> ActionRequest {
    ActionRequest {
        action_id: "stable-action".into(),
        action: Action::StartUseItem {
            hand: Hand::Main,
            max_ticks: 80,
        },
    }
}

#[tokio::test]
async fn waiting_for_delayed_completion_posts_once_and_only_reads_the_original_id() {
    let (client, task) = peer(vec![
        (Outcome::Running, "stable-action"),
        (Outcome::Running, "stable-action"),
        (Outcome::Observed, "stable-action"),
    ])
    .await;
    let r = client
        .perform(&action(), Duration::from_secs(2))
        .await
        .unwrap();
    assert_eq!(r.outcome, Outcome::Observed);
    let methods = task.await.unwrap();
    assert_eq!(methods.len(), 3);
    assert!(methods[0].starts_with("POST "));
    assert!(
        methods[1..]
            .iter()
            .all(|m| m.starts_with("GET ") && m.contains("/actions/stable-action "))
    );
}
#[tokio::test]
async fn a_wait_deadline_preserves_running_and_uncertainty_never_replays() {
    for outcome in [Outcome::Running, Outcome::Uncertain] {
        let (client, task) = peer(vec![(outcome.clone(), "stable-action")]).await;
        let r = client.perform(&action(), Duration::ZERO).await.unwrap();
        assert_eq!(r.outcome, outcome);
        assert_eq!(task.await.unwrap().len(), 1);
    }
}
#[tokio::test]
async fn crossed_initial_and_status_receipts_are_rejected() {
    for responses in [
        vec![(Outcome::Observed, "foreign")],
        vec![
            (Outcome::Running, "stable-action"),
            (Outcome::Observed, "foreign"),
        ],
    ] {
        let (client, task) = peer(responses).await;
        let error = client
            .perform(&action(), Duration::from_secs(1))
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::Conflict);
        task.await.unwrap();
    }
}

use super::*;
use std::net::TcpListener;

fn sample(uuid: &str) -> Observation {
    Observation {
        revision: "native-tick".into(),
        player: json!({"uuid":uuid,"life_id":"life-1","inventory":[]}),
        menu: json!({"menu_id":0,"state_id":21,"slots":[],"carried":{"item":"minecraft:air","count":0}}),
        world: json!({"sample_clock":{"epoch":"jvm:life-1","nanos":"21"}}),
    }
}
fn peer(
    status: u16,
    observation: Observation,
    delay: Duration,
) -> (Native, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let worker = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
        let mut first = String::new();
        reader.read_line(&mut first).unwrap();
        assert_eq!(first, "POST /v1/observe HTTP/1.1\r\n");
        let mut size = 0;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
            if let Some((key, value)) = line.split_once(':')
                && key.eq_ignore_ascii_case("content-length")
            {
                size = value.trim().parse::<usize>().unwrap();
            }
        }
        assert!(size < 16384);
        let mut body = vec![0; size];
        reader.read_exact(&mut body).unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap(),
            json!({"player_uuid":"own"})
        );
        thread::sleep(delay);
        let body = serde_json::to_vec(&observation).unwrap();
        let header = format!(
            "HTTP/1.1 {status} fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        if stream.write_all(header.as_bytes()).is_ok() {
            let _ = stream.write_all(&body);
        }
        // Exactly one accepted native request; the listener is then dropped.
    });
    (
        Native {
            endpoint,
            token: "fixture-token".into(),
            http: reqwest::blocking::Client::builder()
                .no_proxy()
                .build()
                .unwrap(),
        },
        worker,
    )
}
#[test]
fn explicit_fresh_read_uses_one_native_http_request_and_native_revision() {
    let (native, worker) = peer(200, sample("own"), Duration::ZERO);
    let o = read(&native, "own", &json!({})).unwrap();
    assert_eq!(o.menu["state_id"], 21);
    assert_eq!(o.player["life_id"], "life-1");
    worker.join().unwrap();
}
#[test]
fn nonempty_nonobject_input_is_refused_before_transport() {
    let native = Native {
        endpoint: "http://127.0.0.1:9".into(),
        token: "fixture-token".into(),
        http: reqwest::blocking::Client::new(),
    };
    for input in [Value::Null, json!([]), json!({"refresh":true})] {
        assert_eq!(
            read(&native, "own", &input).unwrap_err().code,
            ErrorCode::InvalidRequest
        );
    }
}
#[test]
fn failed_or_foreign_native_reply_has_no_cached_fallback() {
    for (status, uuid, code) in [
        (503, "own", ErrorCode::NotReady),
        (200, "foreign", ErrorCode::Conflict),
    ] {
        let (native, worker) = peer(status, sample(uuid), Duration::ZERO);
        assert_eq!(read(&native, "own", &json!({})).unwrap_err().code, code);
        worker.join().unwrap();
    }
}
#[test]
fn missing_life_or_native_clock_is_refused_without_fallback() {
    let mut no_life = sample("own");
    no_life.player["life_id"] = Value::Null;
    let mut no_clock = sample("own");
    no_clock.world["sample_clock"]["nanos"] = json!(21);
    for o in [no_life, no_clock] {
        let (native, worker) = peer(200, o, Duration::ZERO);
        assert_eq!(
            read(&native, "own", &json!({})).unwrap_err().code,
            ErrorCode::Conflict
        );
        worker.join().unwrap();
    }
}

#[test]
fn slow_native_request_is_bounded_without_polling_or_retry() {
    let (native, worker) = peer(200, sample("own"), Duration::from_millis(650));
    let start = Instant::now();
    assert_eq!(
        read(&native, "own", &json!({})).unwrap_err().code,
        ErrorCode::NotReady
    );
    assert!(start.elapsed() < Duration::from_secs(2));
    worker.join().unwrap();
}

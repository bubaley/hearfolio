//! The upload worker against an in-process stand-in for Hearfolio Cloud + S3.
use super::*;
use crate::org::{Connection, Organization, UserBrief};
use std::sync::atomic::AtomicUsize;

/// Multipart plan with 3 parts of 8 bytes; part 2 fails once with 500;
/// `complete` checks every ETag.
fn fake_cloud(
    fail_part_once: Arc<AtomicBool>,
    creates: Arc<AtomicUsize>,
) -> (String, Arc<Mutex<Vec<String>>>) {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let base = format!("http://{}", server.server_addr().to_ip().unwrap());
    let log = Arc::new(Mutex::new(Vec::new()));
    let seen = log.clone();
    let part_base = base.clone();
    std::thread::spawn(move || {
        for mut request in server.incoming_requests() {
            let url = request.url().to_owned();
            let method = request.method().to_string();
            let mut body = String::new();
            if method != "PUT" {
                let _ = request.as_reader().read_to_string(&mut body);
            } else {
                let mut bytes = Vec::new();
                let _ = request.as_reader().read_to_end(&mut bytes);
                body = format!("{} bytes", bytes.len());
            }
            let header = |name: &'static str| {
                request
                    .headers()
                    .iter()
                    .find(|h| h.field.equiv(name))
                    .map(|h| h.value.as_str().to_owned())
            };
            let authorized = header("Authorization").as_deref() == Some("Token secret")
                && header("Organization-Key").as_deref() == Some("acme");
            seen.lock().unwrap().push(format!("{method} {url} {body}"));
            let plan = |numbers: Vec<u32>| {
                json!({"method":"multipart","upload_id":"up-1","part_size":8,"parts_count":3,
                "parts": numbers.iter().map(|n| json!({"part_number":n,"url":format!("{part_base}/s3/part/{n}")})).collect::<Vec<_>>(),
                "expires_at":"2030-01-01T00:00:00Z"})
            };
            let recording = json!({"id":"rec-1","client_id":"c1","updated_at":"2026-01-01T00:00:00Z","recorded_at":"2026-01-01T00:00:00Z","status":"awaiting_upload","deleted_at":null});
            let respond = |status: u16, value: Value| {
                tiny_http::Response::from_string(value.to_string())
                    .with_status_code(status)
                    .with_header(
                        "Content-Type: application/json"
                            .parse::<tiny_http::Header>()
                            .unwrap(),
                    )
            };
            let response = if url.starts_with("/s3/part/") {
                let number = url.rsplit('/').next().unwrap().to_owned();
                if number == "2" && fail_part_once.swap(false, Ordering::SeqCst) {
                    respond(500, json!({}))
                } else {
                    respond(200, json!({})).with_header(
                        format!("ETag: \"etag-{number}\"")
                            .parse::<tiny_http::Header>()
                            .unwrap(),
                    )
                }
            } else if !authorized {
                respond(401, json!({}))
            } else if method == "POST" && url == "/api/v1/recordings" {
                creates.fetch_add(1, Ordering::SeqCst);
                let sent: Value = serde_json::from_str(&body).unwrap();
                let ok = sent["client_id"] == "c1"
                    && sent["audio_size"] == 20
                    && sent["visibility"] == "personal"
                    && sent["transcript_text"] == "hello";
                if ok {
                    respond(
                        201,
                        json!({"recording": recording, "upload": plan(vec![1, 2, 3])}),
                    )
                } else {
                    respond(422, sent)
                }
            } else if method == "POST" && url == "/api/v1/recordings/rec-1/upload/parts" {
                let sent: Value = serde_json::from_str(&body).unwrap();
                let numbers = sent["part_numbers"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|n| n.as_u64().unwrap() as u32)
                    .collect();
                respond(200, plan(numbers))
            } else if method == "POST" && url == "/api/v1/recordings/rec-1/upload/complete" {
                let sent: Value = serde_json::from_str(&body).unwrap();
                let etags: Vec<String> = sent["parts"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|p| p["etag"].as_str().unwrap().to_owned())
                    .collect();
                if etags == ["\"etag-1\"", "\"etag-2\"", "\"etag-3\""] {
                    let mut done = recording.clone();
                    done["status"] = json!("queued");
                    done["updated_at"] = json!("2026-01-01T00:01:00Z");
                    respond(200, done)
                } else {
                    respond(400, json!({}))
                }
            } else {
                respond(404, json!({}))
            };
            let _ = request.respond(response);
        }
    });
    (base, log)
}

#[test]
fn multipart_upload_resumes_after_a_failed_part_and_lands_in_the_cache() {
    let creates = Arc::new(AtomicUsize::new(0));
    let (base, log) = fake_cloud(Arc::new(AtomicBool::new(true)), creates.clone());
    let pending = super::super::pending_dir().unwrap().join("c1.m4a");
    fs::write(&pending, b"0123456789abcdefghij").unwrap();
    {
        let _lock = super::super::lock();
        super::super::write_connections(&[Connection {
            id: "conn".into(),
            server: base.clone(),
            server_name: "Test".into(),
            server_version: String::new(),
            revoke_url: format!("{base}/api/v1/oauth/revoke"),
            user: UserBrief::default(),
            organizations: vec![Organization {
                id: "org".into(),
                name: "Acme".into(),
                key: "acme".into(),
                role: "member".into(),
            }],
            created_at: 0,
            secure: true,
            needs_reauth: false,
        }])
        .unwrap();
        let mut job = upload::sample(20);
        job.file = pending.clone();
        job.visibility = "personal".into();
        job.transcript_text = Some("hello".into());
        super::super::write_queue(&[job]).unwrap();
    }
    super::super::secrets::store("conn", "secret").unwrap();

    process(super::super::read_queue().unwrap().remove(0));
    let job = super::super::read_queue().unwrap().remove(0);
    assert_eq!(
        job.phase,
        Phase::Uploading,
        "part 2 failed: waits for a retry"
    );
    assert_eq!(job.parts.keys().copied().collect::<Vec<_>>(), vec![1]);
    assert!(job.next_attempt_at > super::super::now_ms());
    assert_eq!(job.remote_id.as_deref(), Some("rec-1"));

    // "Restart": the in-memory plan is gone and the backoff has passed.
    forget_plan("c1");
    {
        let _lock = super::super::lock();
        let mut jobs = super::super::read_queue().unwrap();
        jobs[0].next_attempt_at = 0;
        super::super::write_queue(&jobs).unwrap();
    }
    process(super::super::read_queue().unwrap().remove(0));
    let job = super::super::read_queue().unwrap().remove(0);
    let requests = log.lock().unwrap().clone();
    assert_eq!(job.phase, Phase::Done, "{requests:#?} {:?}", job.error);
    assert_eq!(
        creates.load(Ordering::SeqCst),
        1,
        "never a second recording"
    );
    assert!(
        requests
            .iter()
            .any(|r| r == "POST /api/v1/recordings/rec-1/upload/parts {\"part_numbers\":[2,3]}"),
        "{requests:#?}"
    );
    assert_eq!(
        requests
            .iter()
            .filter(|r| r.starts_with("PUT /s3/part/1 "))
            .count(),
        1,
        "part 1 is not sent again"
    );
    assert!(requests.iter().any(|r| r == "PUT /s3/part/3 4 bytes"));
    assert!(!pending.exists());
    let cached = super::super::cache::find(&super::super::cache_dir().unwrap(), "rec-1").unwrap();
    assert_eq!(fs::read(cached).unwrap(), b"0123456789abcdefghij");
    let library = super::super::library::read(&super::super::library_path("conn", "org").unwrap());
    assert_eq!(library.items["rec-1"]["status"], "queued");
}

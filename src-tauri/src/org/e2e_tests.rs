//! Live run against a Hearfolio Cloud server (ignored by default):
//!
//! ```sh
//! HEARFOLIO_E2E_SERVER=http://127.0.0.1:9100 HEARFOLIO_E2E_EMAIL=… HEARFOLIO_E2E_PASSWORD=… \
//!   cargo test --manifest-path src-tauri/Cargo.toml --lib org_e2e -- --ignored --nocapture
//! ```
//!
//! It signs in like the consent page would (cookie session → `POST /oauth/authorize`),
//! exchanges the code with this app's PKCE verifier, uploads a 17 MiB WAV through
//! the real queue (multipart), syncs the organization, reads the detail, results
//! and audio URL, then revokes the token. Needs S3 configured on the server.
use super::*;
use serde_json::{json, Value};

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn silent_wav(path: &std::path::Path, bytes: usize) {
    let data = bytes - 44;
    let mut wav = Vec::with_capacity(bytes);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&((data + 36) as u32).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&16_000u32.to_le_bytes());
    wav.extend_from_slice(&32_000u32.to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(data as u32).to_le_bytes());
    wav.resize(bytes, 0);
    fs::write(path, wav).unwrap();
}

#[test]
#[ignore]
fn org_e2e() {
    let Some(input) = env("HEARFOLIO_E2E_SERVER") else {
        eprintln!("set HEARFOLIO_E2E_SERVER (and _EMAIL / _PASSWORD for the full run)");
        return;
    };
    let server_url = server::normalize_server_url(&input).unwrap();
    let discovery = api::discovery(&server_url).unwrap();
    server::check_compatibility(&discovery, "99.0.0").unwrap();
    assert!(
        server::check_compatibility(&discovery, "0.0.1").is_err(),
        "min_app_version is enforced"
    );
    let base = server::api_base(&server_url, &discovery);
    eprintln!(
        "discovery ok: {} {} at {base}",
        discovery.name, discovery.server_version
    );
    let (Some(email), Some(password)) = (env("HEARFOLIO_E2E_EMAIL"), env("HEARFOLIO_E2E_PASSWORD"))
    else {
        return;
    };

    // What the consent page does in the browser.
    let login = api::client()
        .post(format!("{base}/api/v1/auth/login"))
        .json(&json!({"email": email, "password": password}))
        .send()
        .unwrap();
    assert!(login.status().is_success(), "login: {}", login.status());
    let cookies: Vec<String> = login
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok()?.split(';').next().map(str::to_owned))
        .collect();
    let verifier = pkce::generate_verifier();
    let state = pkce::generate_state();
    let redirect = pkce::loopback_redirect(53682);
    let authorized: Value = api::client()
        .post(format!("{base}/api/v1/oauth/authorize"))
        .header("Cookie", cookies.join("; "))
        // The consent page is same-origin; the server rejects cookie POSTs without a trusted Origin (CSRF).
        .header("Origin", base.trim_end_matches('/'))
        .json(&json!({"client_id": pkce::CLIENT_ID, "redirect_uri": redirect, "code_challenge": pkce::challenge(&verifier), "code_challenge_method": "S256", "state": state, "device_name": "Hearfolio e2e"}))
        .send()
        .unwrap()
        .json()
        .unwrap();
    let code = authorized["code"]
        .as_str()
        .expect("authorize code")
        .to_owned();
    assert_eq!(authorized["state"], state.as_str());
    let token_body =
        api::exchange_code(&discovery.oauth.token_url, &code, &verifier, &redirect).unwrap();
    let token = token_body["access_token"].as_str().unwrap().to_owned();
    let organizations = auth::organizations_from(&token_body["organizations"]);
    let organization = organizations
        .first()
        .expect("the user needs an organization")
        .clone();
    eprintln!("token ok, organization {}", organization.name);

    let connection_id = uuid::Uuid::new_v4().to_string();
    secrets::store(&connection_id, &token).unwrap();
    {
        let _lock = lock();
        let mut connections = read_connections().unwrap();
        connections.push(Connection {
            id: connection_id.clone(),
            server: base.clone(),
            server_name: discovery.name.clone(),
            server_version: discovery.server_version.clone(),
            revoke_url: discovery.oauth.revoke_url.clone(),
            user: UserBrief::default(),
            organizations: organizations.clone(),
            created_at: now_ms(),
            secure: true,
            needs_reauth: false,
        });
        write_connections(&connections).unwrap();
    }

    let client_id = uuid::Uuid::new_v4().to_string();
    let file = pending_dir().unwrap().join(format!("{client_id}.wav"));
    let size = 17 * 1024 * 1024;
    silent_wav(&file, size);
    let mut job = upload::sample(size as u64);
    job.client_id = client_id.clone();
    job.connection_id = connection_id.clone();
    job.organization_id = organization.id.clone();
    job.file = file;
    job.mime = "audio/wav".into();
    job.original_filename = "e2e.wav".into();
    job.recorded_at = now_ms();
    job.duration_ms = Some(((size - 44) / 32) as u64);
    job.title = Some("Hearfolio e2e".into());
    job.transcript_text = Some("Hearfolio end-to-end check".into());
    job.transcription_model = Some("e2e".into());
    {
        let _lock = lock();
        let mut jobs = read_queue().unwrap();
        jobs.push(job);
        write_queue(&jobs).unwrap();
    }
    for _ in 0..5 {
        let job = read_queue()
            .unwrap()
            .into_iter()
            .find(|j| j.client_id == client_id)
            .unwrap();
        if !job.active() {
            break;
        }
        worker::process_for_test(job);
        let _lock = lock();
        let mut jobs = read_queue().unwrap();
        if let Some(job) = jobs.iter_mut().find(|j| j.client_id == client_id) {
            job.next_attempt_at = 0;
        }
        write_queue(&jobs).unwrap();
    }
    let job = read_queue()
        .unwrap()
        .into_iter()
        .find(|j| j.client_id == client_id)
        .unwrap();
    assert_eq!(job.phase, upload::Phase::Done, "upload: {:?}", job.error);
    assert_eq!(job.multipart.as_ref().map(|m| m.parts_count), Some(3));
    let remote = job.remote_id.clone().unwrap();
    eprintln!("uploaded {remote}");

    let rt = |f: std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Value, String>> + Send>,
    >| { tauri::async_runtime::block_on(f) };
    let (c, o) = (connection_id.clone(), organization.id.clone());
    let synced = rt(Box::pin(async move {
        commands::org_sync(c, o)
            .await
            .map(|v| serde_json::to_value(v).unwrap())
    }))
    .unwrap();
    assert!(
        synced["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["id"] == remote.as_str()),
        "sync lists the upload"
    );
    let (c, o, r) = (
        connection_id.clone(),
        organization.id.clone(),
        remote.clone(),
    );
    let detail = rt(Box::pin(async move {
        commands::org_recording(c, o, r, true).await
    }))
    .unwrap();
    assert_eq!(
        detail["recording"]["status"], "transcribed",
        "the transcript came along"
    );
    assert!(detail["results"].is_array(), "results endpoint answered");
    let (c, o, r) = (
        connection_id.clone(),
        organization.id.clone(),
        remote.clone(),
    );
    let audio = rt(Box::pin(async move {
        commands::org_audio(c, o, r)
            .await
            .map(|v| serde_json::to_value(v).unwrap())
    }))
    .unwrap();
    assert!(
        audio["path"].is_string(),
        "uploaded audio is cached locally"
    );

    // Clean up: delete the recording on the server and revoke the token.
    let api = api::Api {
        base: &base,
        token: &token,
        organization_key: Some(&organization.key),
    };
    api.delete(&format!("/recordings/{remote}")).unwrap();
    let (c, o) = (connection_id.clone(), organization.id.clone());
    let synced = rt(Box::pin(async move {
        commands::org_sync(c, o)
            .await
            .map(|v| serde_json::to_value(v).unwrap())
    }))
    .unwrap();
    assert!(
        !synced["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["id"] == remote.as_str()),
        "tombstone removes it"
    );
    api::revoke(&discovery.oauth.revoke_url, &token).unwrap();
    let after = api::Api {
        base: &base,
        token: &token,
        organization_key: None,
    }
    .get::<Value>("/me/organizations");
    assert_eq!(
        after.err(),
        Some(api::ApiError::Status(401)),
        "revoked token is rejected"
    );
    // Last on purpose: replaying a used code also revokes the token issued for it (RFC 6749 §4.1.2).
    assert_eq!(
        api::exchange_code(&discovery.oauth.token_url, &code, &verifier, &redirect),
        Err("errors.orgAuthExpired".into()),
        "a code works once"
    );
    eprintln!("e2e ok");
}

//! Upload worker: one background thread drains the persisted queue. Each
//! network step is followed by a saved state transition, so a crash or restart
//! resumes from the last confirmed part. Presigned URLs are kept in memory only;
//! a fresh plan is requested after a restart or when they expire.
use super::{
    api::{self, Api, ApiError},
    upload::{self, Failure, Job, Multipart, Phase, Step},
};
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    io::{Read, Seek, SeekFrom},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Condvar, Mutex, OnceLock,
    },
    time::{Duration, Instant},
};

static WAKE: (Mutex<bool>, Condvar) = (Mutex::new(false), Condvar::new());
static STARTED: AtomicBool = AtomicBool::new(false);
/// Bytes sent of the part/file currently in flight, per client id.
static INFLIGHT: Mutex<BTreeMap<String, u64>> = Mutex::new(BTreeMap::new());
/// Client ids the user cancelled while a request was running.
static CANCELLED: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn plans() -> &'static Mutex<HashMap<String, Plan>> {
    static PLANS: OnceLock<Mutex<HashMap<String, Plan>>> = OnceLock::new();
    PLANS.get_or_init(Default::default)
}

#[derive(Clone, Default)]
struct Plan {
    single: Option<(String, Vec<(String, String)>)>,
    parts: HashMap<u32, String>,
}

pub fn start() {
    if STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("org-uploads".into())
        .spawn(run);
}

/// Wake the worker: new job, retry, network or app focus.
pub fn kick() {
    let (lock, condvar) = &WAKE;
    if let Ok(mut flag) = lock.lock() {
        *flag = true;
        condvar.notify_all();
    }
}

pub fn cancel(client_id: &str) {
    if let Ok(mut cancelled) = CANCELLED.lock() {
        cancelled.push(client_id.to_owned());
    }
    if let Ok(mut plans) = plans().lock() {
        plans.remove(client_id);
    }
}
fn cancelled(client_id: &str) -> bool {
    CANCELLED
        .lock()
        .map(|list| list.iter().any(|id| id == client_id))
        .unwrap_or(false)
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct JobView {
    pub client_id: String,
    pub connection_id: String,
    pub organization_id: String,
    pub remote_id: Option<String>,
    pub display_name: String,
    pub recorded_at: u64,
    pub duration_ms: Option<u64>,
    pub size: u64,
    pub uploaded: u64,
    pub phase: Phase,
    pub error: Option<String>,
    pub waiting_network: bool,
    pub next_attempt_at: u64,
    pub project_id: Option<String>,
    pub visibility: String,
    pub local_record_id: Option<String>,
    pub has_transcript: bool,
    pub created_at: u64,
}
pub fn view(job: &Job) -> JobView {
    let inflight = if job.phase == Phase::Uploading {
        INFLIGHT
            .lock()
            .ok()
            .and_then(|map| map.get(&job.client_id).copied())
            .unwrap_or(0)
    } else {
        0
    };
    JobView {
        client_id: job.client_id.clone(),
        connection_id: job.connection_id.clone(),
        organization_id: job.organization_id.clone(),
        remote_id: job.remote_id.clone(),
        display_name: job.display_name.clone(),
        recorded_at: job.recorded_at,
        duration_ms: job.duration_ms,
        size: job.size,
        uploaded: (job.confirmed_bytes() + inflight).min(job.size),
        phase: job.phase,
        error: job.error.clone(),
        waiting_network: job.waiting_network,
        next_attempt_at: job.next_attempt_at,
        project_id: job.project_id.clone(),
        visibility: job.visibility.clone(),
        local_record_id: job.local_record_id.clone(),
        has_transcript: job.transcript_text.is_some(),
        created_at: job.created_at,
    }
}

/// Apply a transition to the persisted job and announce it.
pub fn transition(client_id: &str, step: Step) -> Option<Job> {
    let _lock = super::lock();
    let mut jobs = super::read_queue().ok()?;
    let job = jobs.iter_mut().find(|job| job.client_id == client_id)?;
    upload::apply(job, step, super::now_ms());
    let updated = job.clone();
    if let Err(error) = super::write_queue(&jobs) {
        eprintln!("Очередь загрузки не сохранена: {error}");
    }
    drop(_lock);
    super::emit("org-upload", view(&updated));
    android_service(&jobs);
    Some(updated)
}

enum Next {
    Job(Box<Job>),
    Wait(Option<u64>),
}

fn run() {
    let mut last_eviction = Instant::now();
    loop {
        let now = super::now_ms();
        let next = {
            let _lock = super::lock();
            match super::read_queue() {
                Ok(jobs) => match upload::next_runnable(&jobs, now) {
                    Some(index) => Next::Job(Box::new(jobs[index].clone())),
                    None => Next::Wait(upload::next_wake(&jobs)),
                },
                Err(_) => Next::Wait(None),
            }
        };
        match next {
            Next::Job(job) => {
                process(*job);
                // Guard against a tight loop if the queue file cannot be written.
                std::thread::sleep(Duration::from_millis(50));
            }
            Next::Wait(at) => {
                let wake = match at {
                    Some(at) => Duration::from_millis(at.saturating_sub(now).clamp(250, 60_000)),
                    None => Duration::from_secs(60),
                };
                let (lock, condvar) = &WAKE;
                if let Ok(mut flag) = lock.lock() {
                    if !*flag {
                        flag = condvar
                            .wait_timeout(flag, wake)
                            .map(|(flag, _)| flag)
                            .unwrap_or_else(|poisoned| poisoned.into_inner().0);
                    }
                    *flag = false;
                }
            }
        }
        if last_eviction.elapsed() > Duration::from_secs(3600) {
            last_eviction = Instant::now();
            if let (Ok(dir), Ok(settings)) = (super::cache_dir(), super::read_settings()) {
                super::cache::evict(&dir, settings.retention_days, std::time::SystemTime::now());
            }
        }
    }
}

struct Context {
    base: String,
    token: String,
    key: String,
}
fn context(job: &Job) -> Result<Context, Failure> {
    let connections = super::read_connections().map_err(Failure::Transient)?;
    let connection = connections
        .iter()
        .find(|connection| connection.id == job.connection_id)
        .ok_or_else(|| Failure::Permanent("errors.orgDisconnected".into()))?;
    if connection.needs_reauth {
        return Err(Failure::Permanent("errors.orgUnauthorized".into()));
    }
    let organization = connection
        .organizations
        .iter()
        .find(|organization| organization.id == job.organization_id)
        .ok_or_else(|| Failure::Permanent("errors.orgMissing".into()))?;
    let token = super::secrets::load(&connection.id).map_err(Failure::Permanent)?;
    Ok(Context {
        base: connection.server.clone(),
        token,
        key: organization.key.clone(),
    })
}

fn process(job: Job) {
    let ctx = match context(&job) {
        Ok(ctx) => ctx,
        Err(failure) => {
            transition(&job.client_id, Step::Failed(failure));
            return;
        }
    };
    if !job.file.is_file() {
        transition(
            &job.client_id,
            Step::Failed(Failure::Permanent("errors.orgAudioMissing".into())),
        );
        return;
    }
    let mut job = job;
    let mut replans = 0;
    // Work until the job finishes, fails, waits or is cancelled.
    while job.runnable(super::now_ms()) && !cancelled(&job.client_id) {
        let step = match next_step(&job, &ctx) {
            Ok(Some(step)) => step,
            Ok(None) => {
                // Expired URLs: ask for a fresh plan, at most twice in a row.
                replans += 1;
                if replans > 2 {
                    Step::Failed(Failure::Transient("errors.orgUploadExpired".into()))
                } else {
                    continue;
                }
            }
            Err(failure) => {
                if failure == Failure::Permanent("errors.orgUnauthorized".into()) {
                    mark_reauth(&job.connection_id);
                }
                Step::Failed(failure)
            }
        };
        let completed = step == Step::Completed;
        match transition(&job.client_id, step) {
            Some(updated) => job = updated,
            None => break,
        }
        if completed {
            finish(&job);
        }
    }
    if let Ok(mut map) = INFLIGHT.lock() {
        map.remove(&job.client_id);
    }
    if let Ok(mut list) = CANCELLED.lock() {
        list.retain(|id| id != &job.client_id);
    }
}

pub fn mark_reauth(connection_id: &str) {
    let _lock = super::lock();
    if let Ok(mut connections) = super::read_connections() {
        if let Some(connection) = connections.iter_mut().find(|c| c.id == connection_id) {
            if !connection.needs_reauth {
                connection.needs_reauth = true;
                let _ = super::write_connections(&connections);
                super::emit(
                    "org-connection",
                    json!({"state":"reauth","connectionId":connection_id}),
                );
            }
        }
    }
}

fn create_body(job: &Job) -> Value {
    let mut body = json!({
        "client_id": job.client_id,
        "recorded_at": super::iso8601(job.recorded_at),
        "source": job.source,
        "original_filename": job.original_filename,
        "audio_mime": job.mime,
        "audio_size": job.size,
        "visibility": job.visibility,
        "project_id": job.project_id,
    });
    if let Some(duration) = job.duration_ms {
        body["duration_ms"] = json!(duration);
    }
    if let Some(title) = job.title.as_ref().filter(|title| !title.trim().is_empty()) {
        body["title"] = json!(title.trim());
    }
    if let Some(text) = job
        .transcript_text
        .as_ref()
        .filter(|text| !text.trim().is_empty())
    {
        body["transcript_text"] = json!(text);
        body["transcription_model"] = json!(job.transcription_model);
    }
    body
}

fn plan_step(job: &Job, remote_id: String, plan: &Value) -> Step {
    if plan.is_null() {
        return Step::Planned {
            remote_id,
            multipart: None,
            uploaded: true,
        };
    }
    let mut stored = Plan::default();
    let multipart = if plan["method"] == "multipart" {
        for part in plan["parts"].as_array().into_iter().flatten() {
            if let (Some(number), Some(url)) = (part["part_number"].as_u64(), part["url"].as_str())
            {
                stored.parts.insert(number as u32, url.to_owned());
            }
        }
        Some(Multipart {
            upload_id: plan["upload_id"].as_str().unwrap_or_default().to_owned(),
            part_size: plan["part_size"].as_u64().unwrap_or(job.size.max(1)),
            parts_count: plan["parts_count"].as_u64().unwrap_or(1) as u32,
        })
    } else {
        let headers = plan["headers"]
            .as_object()
            .map(|headers| {
                headers
                    .iter()
                    .filter_map(|(name, value)| Some((name.clone(), value.as_str()?.to_owned())))
                    .collect()
            })
            .unwrap_or_default();
        stored.single = plan["url"].as_str().map(|url| (url.to_owned(), headers));
        None
    };
    if let Ok(mut plans) = plans().lock() {
        plans.insert(job.client_id.clone(), stored);
    }
    Step::Planned {
        remote_id,
        multipart,
        uploaded: false,
    }
}

fn store_recording(job: &Job, recording: &Value) {
    let _ = super::commands::store_recording(
        &job.connection_id,
        &job.organization_id,
        recording.clone(),
    );
}

/// One network step. `Ok(None)`: the presigned URLs expired, plan again.
fn next_step(job: &Job, ctx: &Context) -> Result<Option<Step>, Failure> {
    let api = Api {
        base: &ctx.base,
        token: &ctx.token,
        organization_key: Some(&ctx.key),
    };
    let plan = plans()
        .lock()
        .ok()
        .and_then(|plans| plans.get(&job.client_id).cloned());
    let needs_plan = job.phase == Phase::Queued
        || job.phase == Phase::Uploading
            && (job.remote_id.is_none()
                || plan.as_ref().is_none_or(|plan| {
                    if job.multipart.is_some() {
                        job.missing_parts()
                            .iter()
                            .any(|n| !plan.parts.contains_key(n))
                    } else {
                        plan.single.is_none()
                    }
                }));
    if needs_plan {
        return match &job.remote_id {
            None => {
                let created: Value = api
                    .post("/recordings", &create_body(job))
                    .map_err(|e| e.failure())?;
                let remote = created["recording"]["id"]
                    .as_str()
                    .ok_or(Failure::Transient("errors.orgResponse".into()))?;
                store_recording(job, &created["recording"]);
                Ok(Some(plan_step(job, remote.to_owned(), &created["upload"])))
            }
            Some(remote) => {
                let numbers = if job.multipart.is_some() {
                    json!(job.missing_parts())
                } else {
                    Value::Null
                };
                match api.post::<Value>(
                    &format!("/recordings/{remote}/upload/parts"),
                    &json!({"part_numbers": numbers}),
                ) {
                    Ok(plan) => Ok(Some(plan_step(job, remote.clone(), &plan))),
                    Err(ApiError::Status(409)) => Ok(Some(Step::Planned {
                        remote_id: remote.clone(),
                        multipart: None,
                        uploaded: true,
                    })),
                    Err(ApiError::Status(404)) => {
                        Err(Failure::Permanent("errors.orgRecordingDeleted".into()))
                    }
                    // An expired multipart upload must start over with a new one.
                    Err(ApiError::Status(400))
                        if job.multipart.is_some() && !job.parts.is_empty() =>
                    {
                        Err(Failure::Rejected("errors.orgUploadRejected".into()))
                    }
                    Err(error) => Err(error.failure()),
                }
            }
        };
    }
    let remote = job.remote_id.clone().unwrap_or_default();
    match job.phase {
        Phase::Uploading => {
            let plan = plan.unwrap_or_default();
            if job.multipart.is_some() {
                let Some(number) = job.missing_parts().first().copied() else {
                    return Ok(Some(Step::Planned {
                        remote_id: remote,
                        multipart: job.multipart.clone(),
                        uploaded: false,
                    }));
                };
                let url = plan.parts.get(&number).cloned().unwrap_or_default();
                let (start, end) = job
                    .part_range(number)
                    .ok_or(Failure::Permanent("errors.orgInvalid".into()))?;
                match put(job, &url, start, end, &[]) {
                    Ok(etag) => Ok(Some(Step::PartUploaded { number, etag })),
                    Err(PutError::Expired) => {
                        forget_plan(&job.client_id);
                        Ok(None)
                    }
                    Err(PutError::Failed(failure)) => Err(failure),
                }
            } else {
                let (url, headers) = plan.single.unwrap_or_default();
                match put(job, &url, 0, job.size, &headers) {
                    Ok(_) => Ok(Some(Step::SingleUploaded)),
                    Err(PutError::Expired) => {
                        forget_plan(&job.client_id);
                        Ok(None)
                    }
                    Err(PutError::Failed(failure)) => Err(failure),
                }
            }
        }
        Phase::Completing => {
            let body = if job.multipart.is_some() && !job.parts.is_empty() {
                json!({"parts": job.parts.iter().map(|(number, etag)| json!({"part_number": number, "etag": etag})).collect::<Vec<_>>()})
            } else {
                json!({})
            };
            match api.post::<Value>(&format!("/recordings/{remote}/upload/complete"), &body) {
                Ok(recording) => {
                    store_recording(job, &recording);
                    Ok(Some(Step::Completed))
                }
                Err(ApiError::Status(400)) => {
                    Err(Failure::Rejected("errors.orgUploadRejected".into()))
                }
                Err(ApiError::Status(404)) => {
                    Err(Failure::Permanent("errors.orgRecordingDeleted".into()))
                }
                Err(error) => Err(error.failure()),
            }
        }
        _ => Ok(None),
    }
}

fn forget_plan(client_id: &str) {
    if let Ok(mut plans) = plans().lock() {
        plans.remove(client_id);
    }
}

enum PutError {
    Expired,
    Failed(Failure),
}

/// Reads a byte range of the file, counting bytes as reqwest sends them.
struct Progress {
    file: fs::File,
    left: u64,
    sent: u64,
    client_id: String,
    last_emit: Instant,
    abort: Arc<AtomicBool>,
}
impl Read for Progress {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if self.abort.load(Ordering::Relaxed) || cancelled(&self.client_id) {
            return Err(std::io::Error::other("cancelled"));
        }
        let limit = buffer.len().min(self.left as usize);
        if limit == 0 {
            return Ok(0);
        }
        let read = self.file.read(&mut buffer[..limit])?;
        self.left -= read as u64;
        self.sent += read as u64;
        if let Ok(mut map) = INFLIGHT.lock() {
            map.insert(self.client_id.clone(), self.sent);
        }
        if self.last_emit.elapsed() >= Duration::from_millis(300) {
            self.last_emit = Instant::now();
            announce(&self.client_id);
        }
        Ok(read)
    }
}

fn announce(client_id: &str) {
    let job = {
        let _lock = super::lock();
        super::read_queue()
            .ok()
            .and_then(|jobs| jobs.into_iter().find(|job| job.client_id == client_id))
    };
    if let Some(job) = job {
        super::emit("org-upload", view(&job));
    }
}

fn put(
    job: &Job,
    url: &str,
    start: u64,
    end: u64,
    headers: &[(String, String)],
) -> Result<String, PutError> {
    if url.is_empty() {
        return Err(PutError::Expired);
    }
    let mut file = fs::File::open(&job.file)
        .map_err(|_| PutError::Failed(Failure::Permanent("errors.orgAudioMissing".into())))?;
    if file.metadata().map(|m| m.len()).unwrap_or(0) != job.size {
        return Err(PutError::Failed(Failure::Permanent(
            "errors.orgAudioChanged".into(),
        )));
    }
    file.seek(SeekFrom::Start(start))
        .map_err(|_| PutError::Failed(Failure::Permanent("errors.orgAudioMissing".into())))?;
    let length = end - start;
    let reader = Progress {
        file,
        left: length,
        sent: 0,
        client_id: job.client_id.clone(),
        last_emit: Instant::now(),
        abort: Arc::new(AtomicBool::new(false)),
    };
    let mut request = api::client()
        .put(url)
        // Slow mobile uplinks: allow ~16 KB/s for a whole part before giving up.
        .timeout(Duration::from_secs(120 + length / 16_384))
        .body(reqwest::blocking::Body::sized(reader, length));
    for (name, value) in headers {
        request = request.header(name.as_str(), value.as_str());
    }
    let response = request
        .send()
        .map_err(|error| PutError::Failed(api::from_reqwest(&error).failure()))?;
    if let Ok(mut map) = INFLIGHT.lock() {
        map.remove(&job.client_id);
    }
    match response.status().as_u16() {
        200..=299 => Ok(response
            .headers()
            .get("etag")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned()),
        // S3 answers an expired or invalid signature with 403 (sometimes 400).
        400 | 403 => Err(PutError::Expired),
        status => Err(PutError::Failed(ApiError::Status(status).failure())),
    }
}

/// Uploaded audio becomes cached audio of the cloud recording.
fn finish(job: &Job) {
    let Some(remote) = &job.remote_id else { return };
    let extension = job
        .file
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("m4a");
    if let (Ok(dir), Some(name)) = (
        super::cache_dir(),
        super::cache::file_name(remote, extension),
    ) {
        let target = dir.join(name);
        if fs::rename(&job.file, &target).is_ok() || fs::copy(&job.file, &target).is_ok() {
            let _ = fs::remove_file(&job.file);
            super::cache::touch(&target);
        }
    }
    super::emit(
        "org-library",
        json!({"connectionId": job.connection_id, "organizationId": job.organization_id}),
    );
}

/// Android keeps the process alive with a foreground service while uploads run.
fn android_service(_jobs: &[Job]) {
    #[cfg(target_os = "android")]
    {
        use tauri::Manager;
        let active: Vec<&Job> = _jobs
            .iter()
            .filter(|job| job.active() && !job.waiting_network)
            .collect();
        let (sent, total) = active.iter().fold((0, 0), |(sent, total), job| {
            (sent + job.confirmed_bytes(), total + job.size)
        });
        if let Some(app) = super::APP.get() {
            let _ = app
                .state::<tauri_plugin_recording::Recording<tauri::Wry>>()
                .transfer::<Value>(
                    "uploadService",
                    json!({"count": active.len(), "current": sent, "total": total}),
                );
        }
    }
}

#[cfg(test)]
#[path = "worker_tests.rs"]
mod tests;

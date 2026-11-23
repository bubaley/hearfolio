//! Sequential transcription queue. Records wait here in the order they were
//! added; one worker thread transcribes them one at a time, so two local
//! engines never run together and the interface is never held by a run.
//!
//! The queue is saved next to the history after every change. A restart keeps
//! the records that were still waiting. A record that was running is run again
//! only when its engine is local (free and repeatable); a cloud run is never
//! repeated on its own, because it may have been paid for. It is reported
//! through the operation snapshot and can be continued by the user.
use crate::cloud::RecognitionConfig;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Condvar, Mutex, MutexGuard,
    },
    time::Duration,
};
use tauri::Emitter;

const FILE: &str = "transcription-queue.json";
/// How long a record waits before it is offered another chance to start.
const RETRY: Duration = Duration::from_millis(500);

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub record_id: String,
    #[serde(default)]
    pub configuration: Option<RecognitionConfig>,
    #[serde(default)]
    pub running: bool,
}
impl Item {
    fn local(&self) -> bool {
        self.configuration
            .as_ref()
            .is_some_and(|configuration| configuration.provider == "local")
    }
}

/// An ordered list of records, saved after every change when it has a path.
#[derive(Default)]
pub struct Queue {
    path: Option<PathBuf>,
    items: Vec<Item>,
}
impl Queue {
    /// Reads the saved queue. Returns it with the records whose run was cut
    /// short and must not be repeated automatically.
    pub fn load(path: &Path) -> (Self, Vec<String>) {
        let mut items: Vec<Item> = fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        let mut interrupted = Vec::new();
        items.retain_mut(|item| {
            if !item.running {
                return true;
            }
            item.running = false;
            if item.local() {
                return true;
            }
            interrupted.push(item.record_id.clone());
            false
        });
        let mut seen = Vec::new();
        items.retain(|item| {
            let new = !seen.contains(&item.record_id);
            seen.push(item.record_id.clone());
            new
        });
        let queue = Self {
            path: Some(path.to_owned()),
            items,
        };
        let _ = queue.save();
        (queue, interrupted)
    }
    fn save(&self) -> Result<(), String> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let temp = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec(&self.items).map_err(|_| "errors.operationFailed")?;
        let mut file = fs::File::create(&temp).map_err(|_| "errors.operationFailed")?;
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(|_| "errors.operationFailed")?;
        fs::rename(temp, path).map_err(|_| "errors.operationFailed".to_owned())
    }
    pub fn contains(&self, record: &str) -> bool {
        self.items.iter().any(|item| item.record_id == record)
    }
    /// Adds a record at the end. False when it is already waiting or running.
    pub fn enqueue(
        &mut self,
        record: &str,
        configuration: Option<RecognitionConfig>,
    ) -> Result<bool, String> {
        if self.contains(record) {
            return Ok(false);
        }
        self.items.push(Item {
            record_id: record.to_owned(),
            configuration,
            running: false,
        });
        if let Err(error) = self.save() {
            self.items.pop();
            return Err(error);
        }
        Ok(true)
    }
    /// The record that is next to run, when nothing is running.
    pub fn head(&self) -> Option<Item> {
        if self.items.iter().any(|item| item.running) {
            return None;
        }
        self.items.first().cloned()
    }
    /// Marks the head as running; false when it is gone or already running.
    pub fn start(&mut self, record: &str) -> bool {
        if self.items.iter().any(|item| item.running) {
            return false;
        }
        match self
            .items
            .first_mut()
            .filter(|item| item.record_id == record)
        {
            Some(item) => {
                item.running = true;
                let _ = self.save();
                true
            }
            None => false,
        }
    }
    /// Removes a record that has not started. False when running or absent.
    pub fn cancel_waiting(&mut self, record: &str) -> bool {
        let Some(position) = self
            .items
            .iter()
            .position(|item| item.record_id == record && !item.running)
        else {
            return false;
        };
        self.items.remove(position);
        let _ = self.save();
        true
    }
    /// Removes a record after its run, whatever the outcome.
    pub fn finish(&mut self, record: &str) {
        self.items.retain(|item| item.record_id != record);
        let _ = self.save();
    }
    pub fn items(&self) -> &[Item] {
        &self.items
    }
}

/// What one worker step did.
#[derive(Debug, PartialEq)]
pub enum Step<T> {
    /// Nothing is waiting.
    Idle,
    /// The head could not start now (a heavy job runs, the microphone is busy).
    Deferred,
    Ran(String, T),
}

/// Starts the head of `queue` when `claim` allows it, runs it with the claim,
/// and removes it afterwards. Only one run exists at a time, because the head
/// is marked running before `run` and nothing else can start until `finish`.
pub fn step<C, T>(
    queue: &Mutex<Queue>,
    claim: impl FnOnce(&Item) -> Option<C>,
    run: impl FnOnce(&Item, C) -> T,
) -> Step<T> {
    let Some(head) = lock(queue).head() else {
        return Step::Idle;
    };
    let Some(claimed) = claim(&head) else {
        return Step::Deferred;
    };
    if !lock(queue).start(&head.record_id) {
        // Cancelled while the claim was taken.
        return Step::Deferred;
    }
    let outcome = run(&head, claimed);
    lock(queue).finish(&head.record_id);
    Step::Ran(head.record_id, outcome)
}
fn lock(queue: &Mutex<Queue>) -> MutexGuard<'_, Queue> {
    queue
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

static QUEUE: Mutex<Queue> = Mutex::new(Queue {
    path: None,
    items: Vec::new(),
});
static WAKE: (Mutex<bool>, Condvar) = (Mutex::new(false), Condvar::new());
static STARTED: AtomicBool = AtomicBool::new(false);

fn wake() {
    let (flag, condvar) = &WAKE;
    *flag
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
    condvar.notify_all();
}
/// Waiting or running records exist: the archive must not move and the app
/// must not restart.
pub fn is_busy() -> bool {
    !lock(&QUEUE).items.is_empty()
}
pub fn contains(record: &str) -> bool {
    lock(&QUEUE).contains(record)
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Event {
    record_id: String,
    /// `queued`, `running`, `finished`, `failed` or `cancelled`.
    state: &'static str,
    error: Option<String>,
}
fn announce(app: &tauri::AppHandle, record: &str, state: &'static str, error: Option<String>) {
    let _ = app.emit(
        "transcription-queue",
        Event {
            record_id: record.to_owned(),
            state,
            error,
        },
    );
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Waiting {
    record_id: String,
    state: &'static str,
}
fn waiting() -> Vec<Waiting> {
    lock(&QUEUE)
        .items()
        .iter()
        .map(|item| Waiting {
            record_id: item.record_id.clone(),
            state: if item.running { "running" } else { "queued" },
        })
        .collect()
}

/// Loads the saved queue and starts the worker. Safe to call again (Android
/// can run setup twice in one process).
pub fn start(app: &tauri::AppHandle) {
    if STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    if let Ok(root) = crate::base() {
        let (queue, _interrupted) = Queue::load(&root.join(FILE));
        *lock(&QUEUE) = queue;
    }
    let app = app.clone();
    let _ = std::thread::Builder::new()
        .name("transcription-queue".into())
        .spawn(move || work(app));
}
fn work(app: tauri::AppHandle) {
    loop {
        let outcome = step(
            &QUEUE,
            |head| {
                // A local engine keeps every core busy; it must not start
                // while the microphone is being captured.
                if head.local() && crate::recording_active() {
                    return None;
                }
                crate::JobGuard::acquire("transcribe").ok()
            },
            |head, job| {
                announce(&app, &head.record_id, "running", None);
                crate::run_transcription(&app, &head.record_id, head.configuration.clone(), job)
            },
        );
        match outcome {
            Step::Ran(record, result) => {
                match result {
                    Ok(_) => announce(&app, &record, "finished", None),
                    Err(error) if error == "errors.operationCancelled" => {
                        announce(&app, &record, "cancelled", Some(error))
                    }
                    Err(error) => announce(&app, &record, "failed", Some(error)),
                }
                continue;
            }
            Step::Deferred => std::thread::sleep(RETRY),
            Step::Idle => {
                let (flag, condvar) = &WAKE;
                let mut woken = flag
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if !*woken {
                    woken = condvar
                        .wait_timeout(woken, Duration::from_secs(5))
                        .map(|(guard, _)| guard)
                        .unwrap_or_else(|poisoned| poisoned.into_inner().0);
                }
                *woken = false;
            }
        }
    }
}

/// Puts a record in the queue. Returns the queue as the interface should show it.
#[tauri::command]
pub fn enqueue_transcription(
    app: tauri::AppHandle,
    id: String,
    configuration: Option<RecognitionConfig>,
) -> Result<Vec<Waiting>, String> {
    // The record must exist and have its audio; the run itself checks again.
    let entry = crate::read_history()?
        .into_iter()
        .find(|entry| entry.id == id)
        .ok_or("errors.recordMissing")?;
    if !Path::new(&entry.input_path).is_file() {
        return Err("errors.audioMissing".into());
    }
    let mut configuration = configuration;
    if let Some(configuration) = configuration.as_mut() {
        configuration.validate()?;
        crate::platform::validate_configuration(configuration)?;
    }
    let added = {
        let mut queue = lock(&QUEUE);
        if queue.path.is_none() {
            // Setup could not read the archive then; it can now.
            queue.path = Some(crate::base()?.join(FILE));
        }
        queue.enqueue(&id, configuration)?
    };
    if added {
        announce(&app, &id, "queued", None);
        wake();
    }
    Ok(waiting())
}
#[tauri::command]
pub fn transcription_queue() -> Vec<Waiting> {
    waiting()
}
/// Removes a waiting record, or stops the running one. The queue goes on with
/// the next record either way.
#[tauri::command]
pub fn cancel_transcription(app: tauri::AppHandle, id: String) -> Result<(), String> {
    let removed = lock(&QUEUE).cancel_waiting(&id);
    if removed {
        announce(
            &app,
            &id,
            "cancelled",
            Some("errors.operationCancelled".into()),
        );
        return Ok(());
    }
    crate::background::request_stop_record(&id)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    fn configuration(provider: &str) -> Option<RecognitionConfig> {
        Some(RecognitionConfig {
            provider: provider.into(),
            model: if provider == "local" {
                "whisper-base".into()
            } else {
                "google/gemini-2.5-flash".into()
            },
            mode: if provider == "local" {
                "local".into()
            } else {
                "streaming".into()
            },
        })
    }
    fn file(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "hearfolio-queue-{name}-{}.json",
            uuid::Uuid::new_v4()
        ))
    }
    fn ids(queue: &Queue) -> Vec<&str> {
        queue
            .items()
            .iter()
            .map(|item| item.record_id.as_str())
            .collect()
    }

    #[test]
    fn records_run_in_the_order_they_were_added_and_only_once() {
        let queue = Mutex::new(Queue::default());
        for id in ["a", "b", "c"] {
            assert!(lock(&queue).enqueue(id, configuration("local")).unwrap());
        }
        // Adding a record that waits already changes nothing.
        assert!(!lock(&queue).enqueue("b", configuration("local")).unwrap());
        let mut order = Vec::new();
        loop {
            match step(&queue, |_| Some(()), |item, ()| item.record_id.clone()) {
                Step::Ran(id, ran) => {
                    assert_eq!(id, ran);
                    order.push(id);
                }
                Step::Idle => break,
                Step::Deferred => panic!("nothing defers here"),
            }
        }
        assert_eq!(order, ["a", "b", "c"]);
        assert!(lock(&queue).items().is_empty());
    }

    #[test]
    fn a_record_added_during_a_run_waits_behind_it() {
        let queue = Mutex::new(Queue::default());
        lock(&queue).enqueue("a", configuration("local")).unwrap();
        let Step::Ran(_, during) = step(
            &queue,
            |_| Some(()),
            |_, ()| {
                // A new record arrives, and a second worker tries to start it.
                lock(&queue).enqueue("b", configuration("local")).unwrap();
                let second = lock(&queue).head();
                (second, lock(&queue).start("b"))
            },
        ) else {
            panic!("the head runs");
        };
        assert_eq!(during, (None, false));
        assert_eq!(ids(&lock(&queue)), ["b"]);
    }

    #[test]
    fn never_two_runs_at_once_even_with_competing_workers() {
        let queue = Arc::new(Mutex::new(Queue::default()));
        for number in 0..12 {
            lock(&queue)
                .enqueue(&format!("record-{number}"), configuration("local"))
                .unwrap();
        }
        let running = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let finished = Arc::new(Mutex::new(Vec::new()));
        let workers: Vec<_> = (0..4)
            .map(|_| {
                let (queue, running, peak, finished) = (
                    queue.clone(),
                    running.clone(),
                    peak.clone(),
                    finished.clone(),
                );
                std::thread::spawn(move || loop {
                    match step(
                        &queue,
                        |_| Some(()),
                        |item, ()| {
                            let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                            peak.fetch_max(now, Ordering::SeqCst);
                            std::thread::sleep(Duration::from_millis(3));
                            running.fetch_sub(1, Ordering::SeqCst);
                            finished.lock().unwrap().push(item.record_id.clone());
                        },
                    ) {
                        Step::Idle => break,
                        Step::Deferred => std::thread::yield_now(),
                        Step::Ran(..) => {}
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        assert_eq!(peak.load(Ordering::SeqCst), 1);
        let finished = finished.lock().unwrap();
        let expected: Vec<String> = (0..12).map(|number| format!("record-{number}")).collect();
        assert_eq!(*finished, expected);
    }

    #[test]
    fn a_blocked_head_stays_in_place_until_it_can_start() {
        let queue = Mutex::new(Queue::default());
        lock(&queue).enqueue("a", configuration("local")).unwrap();
        lock(&queue)
            .enqueue("b", configuration("openrouter"))
            .unwrap();
        assert_eq!(
            step(&queue, |_| None::<()>, |_, ()| ()),
            Step::<()>::Deferred
        );
        assert_eq!(ids(&lock(&queue)), ["a", "b"]);
        assert!(!lock(&queue).head().unwrap().running);
        assert_eq!(
            step(&queue, |_| Some(()), |_, ()| ()),
            Step::Ran("a".into(), ())
        );
    }

    #[test]
    fn waiting_records_can_be_cancelled_but_not_the_running_one() {
        let queue = Mutex::new(Queue::default());
        for id in ["a", "b", "c"] {
            lock(&queue).enqueue(id, None).unwrap();
        }
        assert!(lock(&queue).cancel_waiting("b"));
        assert!(!lock(&queue).cancel_waiting("b"));
        let Step::Ran(_, cancelled_running) = step(
            &queue,
            |_| Some(()),
            |item, ()| lock(&queue).cancel_waiting(&item.record_id),
        ) else {
            panic!("the head runs");
        };
        assert!(!cancelled_running);
        assert_eq!(ids(&lock(&queue)), ["c"]);
    }

    #[test]
    fn waiting_records_survive_a_restart_in_order() {
        let path = file("restart");
        let mut queue = Queue::load(&path).0;
        queue.enqueue("a", configuration("openrouter")).unwrap();
        queue.enqueue("b", configuration("local")).unwrap();
        queue.enqueue("c", configuration("openrouter")).unwrap();
        drop(queue);
        let (restored, interrupted) = Queue::load(&path);
        assert_eq!(ids(&restored), ["a", "b", "c"]);
        assert!(interrupted.is_empty());
        assert!(restored.items().iter().all(|item| !item.running));
        assert_eq!(restored.items()[1].configuration, configuration("local"));
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn a_run_cut_short_is_repeated_only_when_it_is_local() {
        let path = file("interrupted");
        let mut queue = Queue::load(&path).0;
        queue.enqueue("cloud", configuration("openrouter")).unwrap();
        queue.enqueue("next", configuration("openrouter")).unwrap();
        assert!(queue.start("cloud"));
        drop(queue);
        // The process died with the cloud run in flight: it may have been
        // paid for, so it is reported and not repeated; the rest waits.
        let (restored, interrupted) = Queue::load(&path);
        assert_eq!(interrupted, ["cloud"]);
        assert_eq!(ids(&restored), ["next"]);
        drop(restored);
        let mut queue = Queue::load(&path).0;
        queue.finish("next");
        queue.enqueue("local", configuration("local")).unwrap();
        queue.enqueue("after", configuration("local")).unwrap();
        assert!(queue.start("local"));
        drop(queue);
        // A local run costs nothing: it goes first again.
        let (restored, interrupted) = Queue::load(&path);
        assert!(interrupted.is_empty());
        assert_eq!(ids(&restored), ["local", "after"]);
        assert!(!restored.head().unwrap().running);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn a_damaged_or_duplicated_file_never_blocks_the_queue() {
        let path = file("damaged");
        fs::write(&path, b"{not json").unwrap();
        let (queue, interrupted) = Queue::load(&path);
        assert!(queue.items().is_empty() && interrupted.is_empty());
        drop(queue);
        fs::write(
            &path,
            br#"[{"recordId":"a"},{"recordId":"a","running":true},{"recordId":"b"}]"#,
        )
        .unwrap();
        let (queue, _) = Queue::load(&path);
        assert_eq!(ids(&queue), ["a", "b"]);
        fs::remove_file(path).unwrap();
    }
}

//! Persistent upload queue: the state machine only. Network I/O lives in
//! `worker.rs`; every transition is saved before the next request so a restart
//! continues from the last confirmed part. `client_id` makes `POST /recordings`
//! idempotent, so a lost response never creates a duplicate.
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};

/// Only re-uploads of a rejected file give up; server and network errors retry forever.
pub const MAX_ATTEMPTS: u32 = 8;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Queued,
    Uploading,
    Completing,
    Done,
    Failed,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Multipart {
    pub upload_id: String,
    pub part_size: u64,
    pub parts_count: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    pub client_id: String,
    pub connection_id: String,
    pub organization_id: String,
    #[serde(default)]
    pub title: Option<String>,
    /// Display name while the server has no title yet.
    #[serde(default)]
    pub display_name: String,
    pub recorded_at: u64,
    #[serde(default)]
    pub duration_ms: Option<u64>,
    pub source: String,
    pub original_filename: String,
    pub mime: String,
    pub size: u64,
    pub file: PathBuf,
    #[serde(default)]
    pub project_id: Option<String>,
    pub visibility: String,
    #[serde(default)]
    pub transcript_text: Option<String>,
    #[serde(default)]
    pub transcription_model: Option<String>,
    #[serde(default)]
    pub local_record_id: Option<String>,
    pub phase: Phase,
    #[serde(default)]
    pub remote_id: Option<String>,
    #[serde(default)]
    pub multipart: Option<Multipart>,
    #[serde(default)]
    pub single_done: bool,
    /// Confirmed parts: number → ETag, sent to `upload/complete`.
    #[serde(default)]
    pub parts: BTreeMap<u32, String>,
    #[serde(default)]
    pub attempts: u32,
    #[serde(default)]
    pub next_attempt_at: u64,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub waiting_network: bool,
    pub created_at: u64,
    #[serde(default)]
    pub finished_at: Option<u64>,
    /// The server could not match our ETags: let it list the parts itself.
    #[serde(default)]
    pub omit_parts: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Failure {
    /// No connection: wait without limit, retrying soon.
    Offline,
    /// 5xx, 429, timeouts mid-upload, an incomplete object: retry with
    /// backoff (capped at 5 minutes) without giving up.
    Transient(String),
    /// The server rejected the uploaded object (size mismatch): upload again.
    Rejected(String),
    /// Bad request, revoked access, deleted recording: needs the user.
    Permanent(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// `POST /recordings` or `upload/parts` answered with a plan (or none: uploaded).
    Planned {
        remote_id: String,
        multipart: Option<Multipart>,
        uploaded: bool,
    },
    PartUploaded {
        number: u32,
        etag: String,
    },
    SingleUploaded,
    Completed,
    /// `complete` said these parts never arrived: upload only them again.
    PartsMissing(Vec<u32>),
    /// `complete` could not use our part list: retry letting the server list them.
    CompleteUnmatched(String),
    Failed(Failure),
    Retry,
}

/// Exponential backoff: 5 s, 10 s, 20 s … capped at 5 minutes (1 minute offline).
pub fn backoff_ms(attempts: u32, offline: bool) -> u64 {
    let cap = if offline { 60_000 } else { 300_000 };
    5_000u64
        .saturating_mul(1u64 << attempts.saturating_sub(1).min(16))
        .min(cap)
}

impl Job {
    pub fn runnable(&self, now: u64) -> bool {
        matches!(
            self.phase,
            Phase::Queued | Phase::Uploading | Phase::Completing
        ) && self.next_attempt_at <= now
    }
    pub fn active(&self) -> bool {
        !matches!(self.phase, Phase::Done | Phase::Failed)
    }
    pub fn missing_parts(&self) -> Vec<u32> {
        self.multipart
            .as_ref()
            .map(|plan| {
                (1..=plan.parts_count)
                    .filter(|number| !self.parts.contains_key(number))
                    .collect()
            })
            .unwrap_or_default()
    }
    pub fn part_range(&self, number: u32) -> Option<(u64, u64)> {
        let plan = self.multipart.as_ref()?;
        if number == 0 || number > plan.parts_count {
            return None;
        }
        let start = u64::from(number - 1) * plan.part_size;
        Some((start, (start + plan.part_size).min(self.size)))
    }
    /// Bytes confirmed by the server (in-flight bytes are tracked separately).
    pub fn confirmed_bytes(&self) -> u64 {
        match self.phase {
            Phase::Completing | Phase::Done => return self.size,
            _ => {}
        }
        if self.single_done {
            return self.size;
        }
        self.parts
            .keys()
            .filter_map(|number| self.part_range(*number))
            .map(|(start, end)| end - start)
            .sum()
    }
    fn progressed(&mut self) {
        self.attempts = 0;
        self.error = None;
        self.waiting_network = false;
        self.next_attempt_at = 0;
    }
}

pub fn apply(job: &mut Job, step: Step, now: u64) {
    match step {
        Step::Planned {
            remote_id,
            multipart,
            uploaded,
        } => {
            job.progressed();
            job.remote_id = Some(remote_id);
            if uploaded {
                job.phase = Phase::Completing;
                return;
            }
            // A new multipart upload (the server restarted a rejected one)
            // invalidates every ETag of the previous one.
            if job.multipart.as_ref().map(|plan| &plan.upload_id)
                != multipart.as_ref().map(|plan| &plan.upload_id)
            {
                job.parts.clear();
                job.omit_parts = false;
            }
            job.multipart = multipart;
            job.phase = if job.multipart.is_some() && job.missing_parts().is_empty()
                || job.multipart.is_none() && job.single_done
            {
                Phase::Completing
            } else {
                Phase::Uploading
            };
        }
        Step::PartUploaded { number, etag } => {
            if job.part_range(number).is_none() {
                return;
            }
            job.progressed();
            job.parts.insert(number, etag);
            if job.missing_parts().is_empty() {
                job.phase = Phase::Completing;
            }
        }
        Step::SingleUploaded => {
            job.omit_parts = false;
            job.progressed();
            job.single_done = true;
            job.phase = Phase::Completing;
        }
        Step::Completed => {
            job.progressed();
            job.phase = Phase::Done;
            job.finished_at = Some(now);
        }
        Step::Failed(Failure::Offline) => {
            job.attempts = job.attempts.saturating_add(1);
            job.waiting_network = true;
            job.next_attempt_at = now + backoff_ms(job.attempts, true);
        }
        Step::Failed(Failure::Transient(error)) => {
            job.waiting_network = false;
            job.attempts = job.attempts.saturating_add(1);
            job.error = Some(error);
            job.next_attempt_at = now + backoff_ms(job.attempts, false);
        }
        Step::CompleteUnmatched(error) => {
            job.waiting_network = false;
            job.attempts = job.attempts.saturating_add(1);
            job.error = Some(error);
            job.omit_parts = true;
            job.next_attempt_at = now + backoff_ms(job.attempts, false);
        }
        Step::PartsMissing(numbers) => {
            job.waiting_network = false;
            job.attempts = job.attempts.saturating_add(1);
            job.next_attempt_at = now + backoff_ms(job.attempts, false);
            if job.multipart.is_some() {
                for number in numbers {
                    job.parts.remove(&number);
                }
            } else {
                job.single_done = false;
            }
            job.phase = Phase::Uploading;
        }
        Step::Failed(Failure::Rejected(error)) => {
            job.waiting_network = false;
            job.attempts += 1;
            job.error = Some(error);
            job.parts.clear();
            job.single_done = false;
            job.phase = if job.attempts >= MAX_ATTEMPTS {
                Phase::Failed
            } else {
                Phase::Queued
            };
            job.next_attempt_at = now + backoff_ms(job.attempts, false);
        }
        Step::Failed(Failure::Permanent(error)) => {
            job.waiting_network = false;
            job.error = Some(error);
            job.phase = Phase::Failed;
        }
        Step::Retry => {
            if job.phase == Phase::Failed {
                job.phase = if job.remote_id.is_some() {
                    Phase::Uploading
                } else {
                    Phase::Queued
                };
            }
            if job.active() {
                job.progressed();
            }
        }
    }
}

/// The next job to work on: oldest first, so a queue drains in recording order.
pub fn next_runnable(jobs: &[Job], now: u64) -> Option<usize> {
    jobs.iter()
        .enumerate()
        .filter(|(_, job)| job.runnable(now))
        .min_by_key(|(_, job)| job.created_at)
        .map(|(index, _)| index)
}

/// When the worker should wake up next for a waiting job.
pub fn next_wake(jobs: &[Job]) -> Option<u64> {
    jobs.iter()
        .filter(|job| job.active())
        .map(|job| job.next_attempt_at)
        .min()
}

#[cfg(test)]
pub(crate) fn sample(size: u64) -> Job {
    Job {
        client_id: "c1".into(),
        connection_id: "conn".into(),
        organization_id: "org".into(),
        title: None,
        display_name: "Recording".into(),
        recorded_at: 1,
        duration_ms: Some(1000),
        source: "desktop".into(),
        original_filename: "a.m4a".into(),
        mime: "audio/mp4".into(),
        size,
        file: PathBuf::from("/tmp/a.m4a"),
        project_id: None,
        visibility: "organization".into(),
        transcript_text: None,
        transcription_model: None,
        local_record_id: None,
        phase: Phase::Queued,
        remote_id: None,
        multipart: None,
        single_done: false,
        parts: BTreeMap::new(),
        attempts: 0,
        next_attempt_at: 0,
        error: None,
        waiting_network: false,
        created_at: 1,
        finished_at: None,
        omit_parts: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn multipart(id: &str) -> Option<Multipart> {
        Some(Multipart {
            upload_id: id.into(),
            part_size: 8,
            parts_count: 3,
        })
    }

    #[test]
    fn multipart_upload_runs_to_done_and_survives_a_restart() {
        let mut job = sample(20);
        assert!(job.runnable(0));
        apply(
            &mut job,
            Step::Planned {
                remote_id: "r1".into(),
                multipart: multipart("u1"),
                uploaded: false,
            },
            10,
        );
        assert_eq!(job.phase, Phase::Uploading);
        assert_eq!(job.missing_parts(), vec![1, 2, 3]);
        assert_eq!(job.part_range(3), Some((16, 20)));
        assert_eq!(job.part_range(4), None);
        apply(
            &mut job,
            Step::PartUploaded {
                number: 2,
                etag: "e2".into(),
            },
            11,
        );
        assert_eq!(job.confirmed_bytes(), 8);
        // Persisted and reloaded: the same upload continues with part 1 and 3.
        let mut job: Job = serde_json::from_str(&serde_json::to_string(&job).unwrap()).unwrap();
        apply(
            &mut job,
            Step::Planned {
                remote_id: "r1".into(),
                multipart: multipart("u1"),
                uploaded: false,
            },
            12,
        );
        assert_eq!(job.missing_parts(), vec![1, 3]);
        apply(
            &mut job,
            Step::PartUploaded {
                number: 1,
                etag: "e1".into(),
            },
            13,
        );
        apply(
            &mut job,
            Step::PartUploaded {
                number: 3,
                etag: "e3".into(),
            },
            14,
        );
        assert_eq!(job.phase, Phase::Completing);
        assert_eq!(job.confirmed_bytes(), 20);
        apply(&mut job, Step::Completed, 15);
        assert_eq!(job.phase, Phase::Done);
        assert!(!job.runnable(100));
        assert_eq!(job.finished_at, Some(15));
    }

    #[test]
    fn a_restarted_multipart_upload_discards_old_etags() {
        let mut job = sample(20);
        apply(
            &mut job,
            Step::Planned {
                remote_id: "r".into(),
                multipart: multipart("u1"),
                uploaded: false,
            },
            0,
        );
        apply(
            &mut job,
            Step::PartUploaded {
                number: 1,
                etag: "e1".into(),
            },
            0,
        );
        apply(
            &mut job,
            Step::Planned {
                remote_id: "r".into(),
                multipart: multipart("u2"),
                uploaded: false,
            },
            0,
        );
        assert_eq!(job.missing_parts(), vec![1, 2, 3]);
        apply(
            &mut job,
            Step::Failed(Failure::Rejected("size".into())),
            100,
        );
        assert_eq!(job.phase, Phase::Queued);
        assert!(job.parts.is_empty());
        assert_eq!(job.next_attempt_at, 100 + 5_000);
    }

    #[test]
    fn single_upload_and_already_uploaded_plans_complete() {
        let mut job = sample(5);
        apply(
            &mut job,
            Step::Planned {
                remote_id: "r".into(),
                multipart: None,
                uploaded: false,
            },
            0,
        );
        assert_eq!(job.phase, Phase::Uploading);
        apply(&mut job, Step::SingleUploaded, 0);
        assert_eq!(job.phase, Phase::Completing);
        let mut again = sample(5);
        apply(
            &mut again,
            Step::Planned {
                remote_id: "r".into(),
                multipart: None,
                uploaded: true,
            },
            0,
        );
        assert_eq!(again.phase, Phase::Completing);
        assert_eq!(again.confirmed_bytes(), 5);
    }

    #[test]
    fn failures_back_off_wait_for_network_and_need_retry_when_permanent() {
        let mut job = sample(5);
        for attempt in 1..=20 {
            apply(&mut job, Step::Failed(Failure::Offline), 0);
            assert!(job.active(), "offline never gives up (attempt {attempt})");
        }
        assert!(job.waiting_network);
        assert_eq!(job.next_attempt_at, 60_000);
        apply(
            &mut job,
            Step::Planned {
                remote_id: "r".into(),
                multipart: None,
                uploaded: false,
            },
            0,
        );
        assert!(!job.waiting_network && job.attempts == 0);
        for attempt in 1..=30 {
            apply(
                &mut job,
                Step::Failed(Failure::Transient("errors.orgServer".into())),
                1_000,
            );
            assert_eq!(
                job.phase,
                Phase::Uploading,
                "server errors never give up ({attempt})"
            );
            assert_eq!(job.next_attempt_at, 1_000 + backoff_ms(attempt, false));
        }
        assert_eq!(job.next_attempt_at, 1_000 + 300_000, "capped at 5 minutes");
        for _ in 0..MAX_ATTEMPTS {
            apply(
                &mut job,
                Step::Failed(Failure::Rejected("errors.orgUploadRejected".into())),
                1_000,
            );
        }
        assert_eq!(
            job.phase,
            Phase::Failed,
            "a file the server keeps rejecting stops"
        );
        assert!(!job.runnable(u64::MAX));
        apply(&mut job, Step::Retry, 2_000);
        assert_eq!(job.phase, Phase::Uploading);
        assert!(job.runnable(2_000) && job.error.is_none() && job.attempts == 0);
        apply(
            &mut job,
            Step::Failed(Failure::Permanent("errors.orgForbidden".into())),
            0,
        );
        assert_eq!(job.phase, Phase::Failed);
        assert_eq!(job.error.as_deref(), Some("errors.orgForbidden"));
        let mut fresh = sample(1);
        apply(&mut fresh, Step::Failed(Failure::Permanent("x".into())), 0);
        apply(&mut fresh, Step::Retry, 0);
        assert_eq!(fresh.phase, Phase::Queued);
    }

    #[test]
    fn missing_parts_are_uploaded_again_without_touching_the_rest() {
        let mut job = sample(20);
        apply(
            &mut job,
            Step::Planned {
                remote_id: "r".into(),
                multipart: multipart("u1"),
                uploaded: false,
            },
            0,
        );
        for number in 1..=3 {
            apply(
                &mut job,
                Step::PartUploaded {
                    number,
                    etag: format!("e{number}"),
                },
                0,
            );
        }
        assert_eq!(job.phase, Phase::Completing);
        apply(&mut job, Step::PartsMissing(vec![2]), 100);
        assert_eq!(job.phase, Phase::Uploading);
        assert_eq!(job.missing_parts(), vec![2]);
        assert_eq!(job.parts.get(&1).map(String::as_str), Some("e1"));
        assert_eq!(job.next_attempt_at, 100 + 5_000);
        // The same upload id after a re-plan keeps the confirmed ETags.
        apply(
            &mut job,
            Step::Planned {
                remote_id: "r".into(),
                multipart: multipart("u1"),
                uploaded: false,
            },
            0,
        );
        assert_eq!(job.missing_parts(), vec![2]);
    }

    #[test]
    fn backoff_grows_and_caps() {
        assert_eq!(backoff_ms(1, false), 5_000);
        assert_eq!(backoff_ms(2, false), 10_000);
        assert_eq!(backoff_ms(4, false), 40_000);
        assert_eq!(backoff_ms(30, false), 300_000);
        assert_eq!(backoff_ms(30, true), 60_000);
    }

    #[test]
    fn the_oldest_runnable_job_goes_first() {
        let mut first = sample(1);
        first.created_at = 5;
        first.next_attempt_at = 1_000;
        let mut second = sample(1);
        second.client_id = "c2".into();
        second.created_at = 10;
        let mut done = sample(1);
        done.created_at = 1;
        done.phase = Phase::Done;
        let jobs = vec![second, first, done];
        assert_eq!(next_runnable(&jobs, 0), Some(0));
        assert_eq!(next_runnable(&jobs, 1_000), Some(1));
        assert_eq!(next_wake(&jobs), Some(0));
    }
}

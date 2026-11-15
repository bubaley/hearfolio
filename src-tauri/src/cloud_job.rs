//! One prepared piece of audio becomes text, whatever the request does on the
//! way: a dropped connection is retried with a pause, a model that runs into
//! its output limit gets the same audio in two shorter pieces, and measured
//! silence is simply empty. An empty answer for audible audio is not silence:
//! it is kept as a gap the user is told about. Only a failure that repeating cannot fix reaches the user, and
//! it never costs the pieces that are already done.
use crate::cloud::{transient, Recognized};
use crate::phrases::{coarse, Phrase};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

/// Requests for one piece before a passing failure is reported.
pub const ATTEMPTS: u32 = 5;
/// Pause before repeating a request: 2 s, 5 s, 12 s, 30 s.
pub fn backoff(failures: u32) -> Duration {
    Duration::from_secs(match failures {
        0 | 1 => 2,
        2 => 5,
        3 => 12,
        _ => 30,
    })
}

/// What the job needs from the outside world; tests replace it with a script.
pub trait Backend {
    /// One request for `audio`, which starts `offset_ms` into the recording.
    /// `live` receives the text of this request as it grows.
    fn recognize(
        &mut self,
        audio: &Path,
        offset_ms: u64,
        live: &mut dyn FnMut(&str),
    ) -> Result<Recognized, String>;
    /// Length of a piece in milliseconds, when it can be read.
    fn duration_ms(&mut self, audio: &Path) -> Option<u64>;
    /// Longer than the user's segment length (a model that tells speakers
    /// apart gets long pieces): a request that times out is divided instead
    /// of repeated.
    fn oversized(&mut self, _audio: &Path) -> bool {
        false
    }
    /// A pause that ends early with an error when the user cancels.
    fn wait(&mut self, delay: Duration) -> Result<(), String>;
    /// Two halves of `audio`, or `None` when it is too short to divide.
    fn split(&mut self, audio: &Path) -> Result<Option<(PathBuf, PathBuf)>, String>;
    /// Whether the audio itself is silent, measured on its samples. An empty
    /// answer is believed only for such a piece.
    fn silent(&mut self, audio: &Path) -> bool;
    /// The job repeats a request or divides a piece; shown as the current stage.
    fn stage(&mut self, _stage: &'static str) {}
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Outcome {
    pub text: String,
    /// The model could not finish even the shortest piece, or returned nothing
    /// for audible audio; `text` is what it did write. The transcript
    /// continues, the passage is reported.
    pub incomplete: bool,
    /// Timed phrases of the piece, in milliseconds from the start of the
    /// recording; one phrase for the piece when the model gave no times.
    pub phrases: Vec<Phrase>,
}

/// Failures that can mean the request took too long.
fn timed_out(error: &str) -> bool {
    let code = error.split('|').next().unwrap_or(error);
    matches!(
        code,
        "errors.cloudConnect" | "errors.cloudResponseRead" | "errors.cloudInterrupted"
    ) || ["HTTP 408", "HTTP 504", "HTTP 524"]
        .iter()
        .any(|status| error.contains(status))
}

fn join(first: &str, second: &str) -> String {
    match (first.is_empty(), second.is_empty()) {
        (true, _) => second.to_owned(),
        (_, true) => first.to_owned(),
        _ => format!("{first} {second}"),
    }
}

/// Recognizes one piece. `live` receives everything this piece has produced so
/// far, across retries and halves.
pub fn recognize(
    backend: &mut dyn Backend,
    audio: &Path,
    offset_ms: u64,
    live: &mut dyn FnMut(&str),
) -> Result<Outcome, String> {
    piece(backend, audio, offset_ms, "", live)
}

/// Both halves in order; the second starts where the first ends.
fn halves(
    backend: &mut dyn Backend,
    (first, second): (PathBuf, PathBuf),
    offset_ms: u64,
    before: &str,
    live: &mut dyn FnMut(&str),
) -> Result<Outcome, String> {
    backend.stage("progress.splitCloudChunk");
    let middle = offset_ms + backend.duration_ms(&first).unwrap_or(0);
    let head = piece(backend, &first, offset_ms, before, live)?;
    let tail = piece(backend, &second, middle, &join(before, &head.text), live)?;
    let mut phrases = head.phrases;
    phrases.extend(tail.phrases);
    Ok(Outcome {
        text: join(&head.text, &tail.text),
        incomplete: head.incomplete || tail.incomplete,
        phrases,
    })
}

fn piece(
    backend: &mut dyn Backend,
    audio: &Path,
    offset_ms: u64,
    before: &str,
    live: &mut dyn FnMut(&str),
) -> Result<Outcome, String> {
    let mut failures = 0;
    let mut unanswered = false;
    loop {
        let result = backend.recognize(audio, offset_ms, &mut |text| live(&join(before, text)));
        match result {
            Ok(recognized) if !recognized.truncated => {
                let phrases = if recognized.phrases.is_empty() {
                    coarse(offset_ms, backend.duration_ms(audio), &recognized.text)
                } else {
                    recognized.phrases
                };
                return Ok(Outcome {
                    text: recognized.text,
                    incomplete: false,
                    phrases,
                });
            }
            // The same audio would hit the same limit again. Shorter audio
            // needs less text and breaks a model out of repeating itself.
            Ok(recognized) => {
                let Some(parts) = backend.split(audio)? else {
                    let text = collapse_repetition(&recognized.text);
                    live(&join(before, &text));
                    return Ok(Outcome {
                        phrases: coarse(offset_ms, backend.duration_ms(audio), &text),
                        text,
                        incomplete: true,
                    });
                };
                return halves(backend, parts, offset_ms, before, live);
            }
            // Nothing came back. For a pause in a meeting or a recorder left
            // running that is the right answer, and the samples prove it. For
            // audible audio it is the model failing (a refusal, an empty
            // body): one more request, then a reported gap — never a silent one.
            Err(error) if error == "errors.emptyTranscript" => {
                if backend.silent(audio) {
                    return Ok(Outcome::default());
                }
                if unanswered {
                    return Ok(Outcome {
                        incomplete: true,
                        ..Outcome::default()
                    });
                }
                unanswered = true;
            }
            // Too much audio for one request body: two shorter requests.
            Err(error) if error.starts_with("errors.audioTooLarge") => {
                let Some(parts) = backend.split(audio)? else {
                    return Err(error);
                };
                live(before);
                return halves(backend, parts, offset_ms, before, live);
            }
            // A long piece that timed out would time out again: two shorter
            // requests instead (normal chunking).
            Err(error) if transient(&error) && timed_out(&error) && backend.oversized(audio) => {
                if let Some(parts) = backend.split(audio)? {
                    live(before);
                    return halves(backend, parts, offset_ms, before, live);
                }
                if failures + 1 >= ATTEMPTS {
                    return Err(error);
                }
                failures += 1;
                backend.stage("progress.retryCloudChunk");
                backend.wait(backoff(failures))?;
                live(before);
            }
            Err(error) if transient(&error) && failures + 1 < ATTEMPTS => {
                failures += 1;
                backend.stage("progress.retryCloudChunk");
                backend.wait(backoff(failures))?;
                // Text streamed by the failed request is withdrawn.
                live(before);
            }
            Err(error) => return Err(error),
        }
    }
}

/// Unanswered pieces after which a run that has not heard a word gives up.
pub const UNANSWERED_LIMIT: usize = 2;
/// Judges the pieces finished so far (`incomplete` holds the indexes of
/// reported gaps). A model that cannot take audio answers every piece with
/// nothing: that must end after a couple of requests, not after two per piece
/// of a long recording. Returns the error to stop with.
pub fn verdict(parts: &[String], incomplete: &[u64], finished: bool) -> Result<(), &'static str> {
    if parts.iter().any(|part| !part.trim().is_empty()) {
        return Ok(());
    }
    let unanswered = incomplete.len();
    if unanswered >= UNANSWERED_LIMIT || (finished && unanswered > 0) {
        return Err("errors.cloudNoText");
    }
    if finished {
        // Every piece was measured silence.
        return Err("errors.emptyTranscript");
    }
    Ok(())
}
/// Passages to check by ear, in whole seconds of the recording, from where
/// every piece ends (milliseconds; each starts where the one before ends).
/// Neighbouring gaps become one passage.
pub fn passages(incomplete: &[u64], ends_ms: &[u64]) -> Vec<[u64; 2]> {
    let mut passages: Vec<[u64; 2]> = Vec::new();
    let mut indexes: Vec<usize> = incomplete
        .iter()
        .map(|index| *index as usize)
        .filter(|index| *index < ends_ms.len())
        .collect();
    indexes.sort_unstable();
    indexes.dedup();
    for index in indexes {
        let start = if index == 0 { 0 } else { ends_ms[index - 1] };
        let range = [start / 1000, ends_ms[index].max(start).div_ceil(1000)];
        match passages.last_mut() {
            Some(last) if last[1] >= range[0] => last[1] = range[1],
            _ => passages.push(range),
        }
    }
    passages
}

/// A model that reached its output limit by repeating itself leaves the same
/// phrase many times at the end. Keep one copy; text without such a loop is
/// returned unchanged.
pub fn collapse_repetition(text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    let count = words.len();
    let mut keep = count;
    for period in 1..=60.min(count / 3) {
        // The last repetition is usually cut mid-phrase: allow a ragged end.
        for ragged in 0..period {
            let end = count - ragged;
            if end < period * 3 {
                break;
            }
            let phrase = &words[end - period..end];
            let mut repeats = 1;
            while end >= period * (repeats + 1)
                && &words[end - period * (repeats + 1)..end - period * repeats] == phrase
            {
                repeats += 1;
            }
            // People do repeat themselves ("да, да, да", a refrain). Only a
            // run no speaker produces counts: one word a dozen times, a
            // short phrase half a dozen, a long one from three copies.
            let looped = match period {
                1 => repeats >= 12,
                2 | 3 => repeats >= 6,
                _ => repeats >= 3 && repeats * period >= 16,
            };
            if looped {
                keep = keep.min(end - period * (repeats - 1));
            }
        }
    }
    if keep == count {
        return text.trim().to_owned();
    }
    words[..keep].join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    #[derive(Default)]
    struct Script {
        replies: VecDeque<Result<Recognized, String>>,
        requests: Vec<String>,
        offsets: Vec<u64>,
        waits: Vec<Duration>,
        stages: Vec<&'static str>,
        cancel_during_wait: bool,
        splittable: bool,
        quiet: bool,
        oversized: bool,
    }
    impl Script {
        fn new(replies: Vec<Result<Recognized, String>>) -> Self {
            Self {
                replies: replies.into(),
                splittable: true,
                ..Self::default()
            }
        }
    }
    impl Backend for Script {
        fn recognize(
            &mut self,
            audio: &Path,
            offset_ms: u64,
            live: &mut dyn FnMut(&str),
        ) -> Result<Recognized, String> {
            self.requests.push(audio.to_string_lossy().into_owned());
            self.offsets.push(offset_ms);
            let reply = self.replies.pop_front().expect("unexpected request");
            if let Ok(recognized) = &reply {
                live(&recognized.text);
            }
            reply
        }
        fn wait(&mut self, delay: Duration) -> Result<(), String> {
            self.waits.push(delay);
            if self.cancel_during_wait {
                return Err("errors.operationCancelled".into());
            }
            Ok(())
        }
        fn split(&mut self, audio: &Path) -> Result<Option<(PathBuf, PathBuf)>, String> {
            // "chunk" -> "chunk-a"/"chunk-b"; a quarter is too short to divide.
            let name = audio.to_string_lossy();
            if !self.splittable || name.matches('-').count() >= 2 {
                return Ok(None);
            }
            Ok(Some((
                PathBuf::from(format!("{name}-a")),
                PathBuf::from(format!("{name}-b")),
            )))
        }
        fn silent(&mut self, _audio: &Path) -> bool {
            self.quiet
        }
        // "chunk" lasts 120 s; every halving halves it.
        fn duration_ms(&mut self, audio: &Path) -> Option<u64> {
            Some(120_000 >> audio.to_string_lossy().matches('-').count())
        }
        fn oversized(&mut self, audio: &Path) -> bool {
            self.oversized && !audio.to_string_lossy().contains('-')
        }
        fn stage(&mut self, stage: &'static str) {
            self.stages.push(stage);
        }
    }
    fn full(text: &str) -> Result<Recognized, String> {
        Ok(Recognized {
            text: text.into(),
            ..Recognized::default()
        })
    }
    fn cut(text: &str) -> Result<Recognized, String> {
        Ok(Recognized {
            text: text.into(),
            truncated: true,
            ..Recognized::default()
        })
    }
    fn phrase(start_ms: u64, end_ms: u64, text: &str) -> Phrase {
        Phrase {
            start_ms,
            end_ms,
            text: text.into(),
            speaker: None,
        }
    }
    fn fail(error: &str) -> Result<Recognized, String> {
        Err(error.into())
    }

    #[test]
    fn a_dropped_connection_repeats_only_this_piece_with_growing_pauses() {
        let mut script = Script::new(vec![
            fail("errors.cloudConnect"),
            fail("errors.cloudHttp|HTTP 503: overloaded"),
            fail("errors.cloudInterrupted"),
            full("the whole piece"),
        ]);
        let mut seen = Vec::new();
        let outcome = recognize(&mut script, Path::new("chunk"), 0, &mut |text| {
            seen.push(text.to_owned())
        })
        .unwrap();
        assert_eq!(outcome.text, "the whole piece");
        assert!(!outcome.incomplete);
        assert_eq!(script.requests, ["chunk", "chunk", "chunk", "chunk"]);
        assert_eq!(
            script.waits,
            [2, 5, 12].map(Duration::from_secs),
            "exponential pauses"
        );
        assert_eq!(seen.last().unwrap(), "the whole piece");
    }

    #[test]
    fn a_passing_failure_is_reported_after_the_last_attempt_and_a_lasting_one_at_once() {
        let mut script = Script::new((0..ATTEMPTS).map(|_| fail("errors.cloudConnect")).collect());
        assert_eq!(
            recognize(&mut script, Path::new("chunk"), 0, &mut |_| {}).unwrap_err(),
            "errors.cloudConnect"
        );
        assert_eq!(script.requests.len(), ATTEMPTS as usize);
        assert_eq!(script.waits.len(), ATTEMPTS as usize - 1);
        for error in [
            "errors.cloudHttp|HTTP 401: token",
            "errors.cloudHttp|HTTP 402: credits",
            "errors.tokenRequired",
            "errors.operationCancelled",
        ] {
            let mut script = Script::new(vec![fail(error)]);
            assert_eq!(
                recognize(&mut script, Path::new("chunk"), 0, &mut |_| {}).unwrap_err(),
                error
            );
            assert!(script.waits.is_empty(), "{error} is never retried");
        }
    }

    #[test]
    fn cancelling_during_a_pause_ends_the_job_without_another_request() {
        let mut script = Script::new(vec![fail("errors.cloudConnect")]);
        script.cancel_during_wait = true;
        assert_eq!(
            recognize(&mut script, Path::new("chunk"), 0, &mut |_| {}).unwrap_err(),
            "errors.operationCancelled"
        );
        assert_eq!(script.requests.len(), 1);
    }

    #[test]
    fn a_piece_cut_by_the_output_limit_is_divided_and_fully_recovered() {
        let mut script = Script::new(vec![
            cut("first half and then it broke"),
            full("first half"),
            fail("errors.cloudConnect"),
            full("second half"),
        ]);
        let mut seen = Vec::new();
        let outcome = recognize(&mut script, Path::new("chunk"), 0, &mut |text| {
            seen.push(text.to_owned())
        })
        .unwrap();
        assert_eq!(outcome.text, "first half second half");
        assert!(!outcome.incomplete, "nothing was lost");
        assert_eq!(script.requests, ["chunk", "chunk-a", "chunk-b", "chunk-b"]);
        assert_eq!(
            script.stages,
            ["progress.splitCloudChunk", "progress.retryCloudChunk"]
        );
        // The first half stays on screen while the second is requested.
        assert!(seen.contains(&"first half".to_owned()));
        assert_eq!(seen.last().unwrap(), "first half second half");
    }

    #[test]
    fn a_piece_too_short_to_divide_keeps_its_text_and_is_marked_incomplete() {
        let looped = format!("we agreed on friday {}", "thank you very much ".repeat(40));
        let mut script = Script::new(vec![
            cut("everything"),
            full("opening words"),
            cut("more"),
            full("middle"),
            cut(&looped),
        ]);
        let outcome = recognize(&mut script, Path::new("chunk"), 0, &mut |_| {}).unwrap();
        assert_eq!(
            script.requests,
            ["chunk", "chunk-a", "chunk-b", "chunk-b-a", "chunk-b-b"]
        );
        assert_eq!(
            outcome.text,
            "opening words middle we agreed on friday thank you very much"
        );
        assert!(outcome.incomplete);
        // Without anything to divide the text is kept as it came.
        let mut script = Script::new(vec![cut("only the beginning of")]);
        script.splittable = false;
        let outcome = recognize(&mut script, Path::new("chunk"), 0, &mut |_| {}).unwrap();
        assert_eq!(outcome.text, "only the beginning of");
        assert!(outcome.incomplete);
    }

    #[test]
    fn measured_silence_is_empty_at_once_but_an_empty_answer_for_speech_is_a_reported_gap() {
        // The samples are silent: the empty answer is believed, nothing is repeated.
        let mut script = Script::new(vec![fail("errors.emptyTranscript")]);
        script.quiet = true;
        let outcome = recognize(&mut script, Path::new("chunk"), 0, &mut |_| {}).unwrap();
        assert_eq!(outcome, Outcome::default());
        assert_eq!(script.requests.len(), 1);
        // Audible audio and two empty answers (a refusal, a null body): the
        // piece is kept as an explicit gap.
        let mut script = Script::new(vec![
            fail("errors.emptyTranscript"),
            fail("errors.emptyTranscript"),
        ]);
        let outcome = recognize(&mut script, Path::new("chunk"), 0, &mut |_| {}).unwrap();
        assert_eq!(
            outcome,
            Outcome {
                incomplete: true,
                ..Outcome::default()
            }
        );
        assert!(script.waits.is_empty());
        // A hiccup: the second request does hear speech.
        let mut script = Script::new(vec![fail("errors.emptyTranscript"), full("speech")]);
        let outcome = recognize(&mut script, Path::new("chunk"), 0, &mut |_| {}).unwrap();
        assert_eq!(outcome.text, "speech");
        assert!(!outcome.incomplete);
    }

    #[test]
    fn repetition_at_the_end_is_collapsed_and_ordinary_text_is_untouched() {
        assert_eq!(
            collapse_repetition(&format!(
                "Начали встречу. {}Спасибо за",
                "Спасибо за внимание. ".repeat(30)
            )),
            "Начали встречу. Спасибо за внимание."
        );
        assert_eq!(collapse_repetition(&"да ".repeat(200)), "да");
        let ordinary = "Да, да, да. Мы это уже обсуждали, и обсуждали, и ещё раз обсуждали вчера.";
        assert_eq!(collapse_repetition(ordinary), ordinary);
        let refrain = "one two three one two three one two";
        assert_eq!(
            collapse_repetition(refrain),
            refrain,
            "two copies are not a loop"
        );
        for spoken in [
            "ну да да да да да да да да",
            "так и было, нет нет нет нет нет",
            "раз два раз два раз два раз два",
        ] {
            assert_eq!(
                collapse_repetition(spoken),
                spoken,
                "people repeat themselves"
            );
        }
        assert_eq!(collapse_repetition("  kept\nas is  "), "kept\nas is");
        assert_eq!(collapse_repetition(""), "");
    }

    #[test]
    fn a_model_that_hears_nothing_fails_fast_and_silence_is_told_apart() {
        let empty = String::new;
        // Two audible pieces answered with nothing, not a word so far: stop.
        assert_eq!(verdict(&[empty()], &[0], false), Ok(()));
        assert_eq!(
            verdict(&[empty(), empty()], &[0, 1], false),
            Err("errors.cloudNoText")
        );
        // Measured silence at the start of a recording is not the model's fault.
        assert_eq!(verdict(&[empty(), empty(), empty()], &[], false), Ok(()));
        assert_eq!(verdict(&[empty(), empty()], &[1], false), Ok(()));
        // Once anything was heard, gaps are reported, never fatal.
        let heard = ["speech".to_owned(), empty(), empty(), empty()];
        assert_eq!(verdict(&heard, &[1, 2, 3], true), Ok(()));
        // At the end: nothing but silence, or a single unanswered piece.
        assert_eq!(
            verdict(&[empty(), empty()], &[], true),
            Err("errors.emptyTranscript")
        );
        assert_eq!(verdict(&[empty()], &[0], true), Err("errors.cloudNoText"));
    }

    #[test]
    fn passages_use_the_real_end_of_each_piece_and_join_neighbours() {
        // Three pieces of 120 s and a last one of 127.4 s (a short remainder joined to it).
        let ends = [120_000, 240_000, 360_000, 487_400];
        assert_eq!(passages(&[3], &ends), [[360, 488]]);
        assert_eq!(passages(&[0, 2], &ends), [[0, 120], [240, 360]]);
        assert_eq!(passages(&[2, 1, 1], &ends), [[120, 360]]);
        assert!(passages(&[9], &ends).is_empty());
        assert!(passages(&[], &ends).is_empty());
        // Pieces of different lengths (a run resumed with another segment length).
        assert_eq!(passages(&[1], &[120_000, 600_000]), [[120, 600]]);
    }

    #[test]
    fn halves_are_timed_from_where_they_start_in_the_recording() {
        let mut script = Script::new(vec![
            cut("too much"),
            full("first half"),
            Ok(Recognized {
                text: "second half".into(),
                phrases: vec![phrase(250_000, 251_000, "second half")],
                ..Recognized::default()
            }),
        ]);
        let outcome = recognize(&mut script, Path::new("chunk"), 180_000, &mut |_| {}).unwrap();
        assert_eq!(script.offsets, [180_000, 180_000, 240_000]);
        // A model without times gets one phrase for its half; timed phrases are kept.
        assert_eq!(
            outcome.phrases,
            [
                phrase(180_000, 240_000, "first half"),
                phrase(250_000, 251_000, "second half")
            ]
        );
        // Without a split: one phrase for the piece; silence has none.
        let mut script = Script::new(vec![full("all of it")]);
        let outcome = recognize(&mut script, Path::new("chunk"), 0, &mut |_| {}).unwrap();
        assert_eq!(outcome.phrases, [phrase(0, 120_000, "all of it")]);
        let mut script = Script::new(vec![fail("errors.emptyTranscript")]);
        script.quiet = true;
        let outcome = recognize(&mut script, Path::new("chunk"), 0, &mut |_| {}).unwrap();
        assert!(outcome.phrases.is_empty());
    }

    #[test]
    fn a_long_piece_that_times_out_is_divided_instead_of_repeated() {
        let mut script = Script::new(vec![
            fail("errors.cloudHttp|HTTP 504: upstream timeout"),
            full("head"),
            full("tail"),
        ]);
        script.oversized = true;
        let outcome = recognize(&mut script, Path::new("chunk"), 0, &mut |_| {}).unwrap();
        assert_eq!(outcome.text, "head tail");
        assert_eq!(script.requests, ["chunk", "chunk-a", "chunk-b"]);
        assert!(
            script.waits.is_empty(),
            "no pause before the shorter requests"
        );
        // A body over the provider's limit is divided before anything is sent again.
        let mut script = Script::new(vec![
            fail("errors.audioTooLarge"),
            full("head"),
            full("tail"),
        ]);
        let outcome = recognize(&mut script, Path::new("chunk"), 0, &mut |_| {}).unwrap();
        assert_eq!(outcome.text, "head tail");
        assert!(script.waits.is_empty());
        let mut script = Script::new(vec![fail("errors.audioTooLarge")]);
        script.splittable = false;
        assert_eq!(
            recognize(&mut script, Path::new("chunk"), 0, &mut |_| {}).unwrap_err(),
            "errors.audioTooLarge"
        );
        // Rate limiting is not about length: the same piece is repeated.
        let mut script = Script::new(vec![fail("errors.cloudHttp|HTTP 429: slow"), full("all")]);
        script.oversized = true;
        recognize(&mut script, Path::new("chunk"), 0, &mut |_| {}).unwrap();
        assert_eq!(script.requests, ["chunk", "chunk"]);
        // An ordinary piece that times out is repeated as before.
        let mut script = Script::new(vec![fail("errors.cloudConnect"), full("all")]);
        recognize(&mut script, Path::new("chunk"), 0, &mut |_| {}).unwrap();
        assert_eq!(script.requests, ["chunk", "chunk"]);
    }
}

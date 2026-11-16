//! Timed phrases from one transcription response. Pure and shared in spirit
//! with Hearfolio Cloud: the same response gives the same phrases on the server
//! and in the app.
//!
//! The richest answer wins: words with times (grouped into phrases at speaker
//! changes and pauses), else timed segments, else one phrase for the whole
//! piece. Anything malformed falls to the next level instead of failing the
//! piece, and the provider's own text is never lost to missing words.
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One timed phrase of a transcript, in milliseconds from the start of the
/// recording. Same shape as Hearfolio Cloud `transcript_segments`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Phrase {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
    /// Who speaks, scoped to the request that heard it (`"<request start
    /// ms>:<provider label>"`): two requests' "speaker 0" are not claimed to be
    /// the same person.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker: Option<String>,
}

// Rules and thresholds mirror Hearfolio Cloud
// (`backend/app/domains/ai/utils/transcript_phrases.py`); keep both identical.
// Phrases are paragraphs: one speaker's continuous speech reads as one text.
/// A pause of at least this long after the end of a sentence starts a phrase.
pub const SENTENCE_PAUSE_MS: u64 = 2000;
/// A pause of at least this long starts a phrase anyway.
pub const HARD_PAUSE_MS: u64 = 4000;
/// A longer phrase is divided at its last sentence end, else at its largest gap,
/// so a monologue still has timestamps to jump to.
pub const MAX_PHRASE_MS: u64 = 90_000;
/// Share of the provider text's words the phrases must carry to be trusted.
pub const MIN_COVERAGE: f64 = 0.9;
/// Bound on parsed entries: a 3-hour piece has about 40 000 words.
const MAX_ENTRIES: usize = 200_000;

/// A word or segment, times in ms relative to the start of the audio sent.
#[derive(Debug, Clone, PartialEq)]
struct Timed {
    text: String,
    start: u64,
    end: u64,
    speaker: Option<String>,
}

/// What a response says, already checked. `words` is `None` when absent or
/// when any word has unusable times; `segments` keeps the valid ones.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Parsed {
    /// The provider's text with whitespace collapsed.
    pub text: String,
    words: Option<Vec<Timed>>,
    segments: Option<Vec<Timed>>,
    /// Audio length reported by the provider, ms.
    pub duration_ms: Option<u64>,
}

fn label(value: &Value) -> Option<String> {
    match value {
        Value::Number(number) => {
            if let Some(integer) = number.as_i64() {
                Some(integer.to_string())
            } else {
                // 1.0 is speaker "1"; 1.5 is not a label.
                number
                    .as_f64()
                    .filter(|value| value.is_finite() && value.fract() == 0.0)
                    .map(|value| format!("{value:.0}"))
            }
        }
        Value::String(text) => {
            Some(text.trim().chars().take(64).collect::<String>()).filter(|text| !text.is_empty())
        }
        _ => None,
    }
}

fn milliseconds(value: &Value) -> Option<u64> {
    value
        .as_f64()
        .filter(|value| value.is_finite() && *value >= 0.0)
        .map(|seconds| (seconds * 1000.0).round() as u64)
}

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn entry(item: &Value, key: &str) -> Option<Timed> {
    let text = collapse(item.as_object()?.get(key)?.as_str()?);
    let (start, end) = (milliseconds(&item["start"])?, milliseconds(&item["end"])?);
    (!text.is_empty() && end >= start).then(|| Timed {
        text,
        start,
        end,
        speaker: label(&item["speaker"]),
    })
}

/// Words in provider order. A word with empty text is skipped; one with a
/// missing, non-finite, negative or reversed time (or a non-object entry)
/// rejects the whole list, since its timing can no longer be trusted.
fn parse_words(list: &Value) -> Option<Vec<Timed>> {
    let mut words = Vec::new();
    for item in list.as_array()?.iter().take(MAX_ENTRIES) {
        if item.is_object()
            && item["word"]
                .as_str()
                .is_none_or(|word| word.trim().is_empty())
        {
            continue;
        }
        words.push(entry(item, "word")?);
    }
    (!words.is_empty()).then_some(words)
}

/// Segments in provider order; invalid entries are dropped.
fn parse_segments(list: &Value) -> Option<Vec<Timed>> {
    let segments: Vec<Timed> = list
        .as_array()?
        .iter()
        .take(MAX_ENTRIES)
        .filter_map(|item| entry(item, "text"))
        .collect();
    (!segments.is_empty()).then_some(segments)
}

impl Parsed {
    /// Whether the response carried any usable times at all.
    pub fn timed(&self) -> bool {
        self.words.is_some() || self.segments.is_some()
    }
}

/// Some models return bare words (no punctuation, other casing) next to a
/// punctuated `text`. When the text splits into exactly the same words
/// (letters and digits compared, case-insensitively), its tokens replace the
/// word texts, so sentence ends are known.
fn repunctuate(words: &mut [Timed], text: &str) {
    let tokens: Vec<&str> = text.split_whitespace().collect();
    if tokens.len() != words.len()
        || tokens
            .iter()
            .zip(words.iter())
            .any(|(token, word)| normalized(token) != normalized(&word.text))
    {
        return;
    }
    for (word, token) in words.iter_mut().zip(tokens) {
        word.text = token.to_owned();
    }
}

pub fn parse(value: &Value) -> Parsed {
    let text = collapse(value["text"].as_str().unwrap_or(""));
    let segments = parse_segments(&value["segments"]);
    let mut words = parse_words(&value["words"]);
    if let Some(words) = &mut words {
        repunctuate(words, &text);
        // Some providers label speakers on segments only: a word takes the
        // label of the labelled segment its middle falls in.
        for word in words.iter_mut().filter(|word| word.speaker.is_none()) {
            let middle = (word.start + word.end) as f64 / 2.0;
            word.speaker = segments.iter().flatten().find_map(|segment| {
                (segment.speaker.is_some()
                    && segment.start as f64 <= middle
                    && middle <= segment.end as f64)
                    .then(|| segment.speaker.clone())
                    .flatten()
            });
        }
    }
    Parsed {
        text,
        words,
        segments,
        duration_ms: milliseconds(&value["duration"]),
    }
}

fn ends_sentence(word: &str) -> bool {
    word.trim_end_matches(['"', '\'', '»', '”', ')', ']'])
        .ends_with(['.', '?', '!', '…'])
}

/// Times never go back: a word that starts before the previous one starts
/// where that one starts.
fn monotonic(words: &[Timed]) -> Vec<Timed> {
    let mut ordered: Vec<Timed> = Vec::with_capacity(words.len());
    for word in words {
        let mut word = word.clone();
        if let Some(previous) = ordered.last() {
            if word.start < previous.start {
                word.start = previous.start;
                word.end = word.end.max(previous.start);
            }
        }
        ordered.push(word);
    }
    ordered
}

fn group_words(words: &[Timed]) -> Vec<Vec<Timed>> {
    let mut groups: Vec<Vec<Timed>> = Vec::new();
    for word in monotonic(words) {
        if let Some(current) = groups.last_mut() {
            let previous = current.last().expect("groups are never empty");
            let pause = word.start.saturating_sub(previous.end);
            let starts = word.speaker != previous.speaker
                || pause >= HARD_PAUSE_MS
                || (pause >= SENTENCE_PAUSE_MS && ends_sentence(&previous.text));
            if !starts {
                current.push(word);
                continue;
            }
        }
        groups.push(vec![word]);
    }
    groups.into_iter().flat_map(cap).collect()
}

/// Divides a phrase longer than `MAX_PHRASE_MS`. A cut before word `i` is a
/// candidate when the part before it fits the limit (the first word alone
/// always counts). The cut goes after the last sentence end among them, else
/// at the largest gap (the later one on a tie).
fn cap(mut words: Vec<Timed>) -> Vec<Vec<Timed>> {
    let mut parts = Vec::new();
    while words.len() > 1 && words[words.len() - 1].end - words[0].start > MAX_PHRASE_MS {
        let limit = words[0].start + MAX_PHRASE_MS;
        let mut candidates: Vec<usize> = (1..words.len())
            .filter(|&cut| words[cut - 1].end <= limit)
            .collect();
        if candidates.is_empty() {
            candidates.push(1);
        }
        let gap = |cut: usize| words[cut].start.saturating_sub(words[cut - 1].end);
        let cut = candidates
            .iter()
            .rev()
            .copied()
            .find(|&cut| ends_sentence(&words[cut - 1].text))
            .unwrap_or_else(|| {
                candidates
                    .iter()
                    .copied()
                    .max_by_key(|&cut| (gap(cut), cut))
                    .expect("never empty")
            });
        let tail = words.split_off(cut);
        parts.push(words);
        words = tail;
    }
    parts.push(words);
    parts
}

/// Letters and digits of a word, lowercase: how words are compared.
fn normalized(word: &str) -> String {
    word.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn tokens(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(normalized)
        .filter(|word| !word.is_empty())
        .collect()
}

/// Whether `phrases` carry at least `MIN_COVERAGE` of the words of the
/// provider's `text` (as a multiset, ignoring case and punctuation).
fn covers(phrases: &[Phrase], text: &str) -> bool {
    let expected = tokens(text);
    if expected.is_empty() {
        return true;
    }
    let mut available: std::collections::HashMap<String, usize> = Default::default();
    for phrase in phrases {
        for token in tokens(&phrase.text) {
            *available.entry(token).or_default() += 1;
        }
    }
    let matched = expected
        .iter()
        .filter(|token| match available.get_mut(*token) {
            Some(count) if *count > 0 => {
                *count -= 1;
                true
            }
            _ => false,
        })
        .count();
    matched as f64 >= expected.len() as f64 * MIN_COVERAGE
}

/// Times relative to the piece become absolute; they stay inside the piece,
/// in order and without overlap. Empty text is dropped.
fn place(offset_ms: u64, limit_ms: Option<u64>, timed: Vec<Timed>) -> Vec<Phrase> {
    let clamp = |ms: u64| offset_ms + limit_ms.map_or(ms, |limit| ms.min(limit));
    let mut phrases: Vec<Phrase> = Vec::with_capacity(timed.len());
    for item in timed {
        let text = item.text.trim();
        if text.is_empty() {
            continue;
        }
        let floor = phrases.last().map_or(offset_ms, |last| last.end_ms);
        let start = clamp(item.start).max(floor);
        let end = clamp(item.end).max(start);
        phrases.push(Phrase {
            start_ms: start,
            end_ms: end,
            text: text.to_owned(),
            speaker: item.speaker,
        });
    }
    phrases
}

/// Provider speaker ids mean something only inside one request, so the label
/// names the request by its start: `0:1` is speaker 1 of the request at 0 ms.
fn scoped(offset_ms: u64, phrases: Vec<Phrase>) -> Vec<Phrase> {
    phrases
        .into_iter()
        .map(|phrase| Phrase {
            speaker: phrase
                .speaker
                .map(|speaker| format!("{offset_ms}:{speaker}")),
            ..phrase
        })
        .collect()
}

/// One phrase for the whole piece: what a model without timestamps gives.
pub fn coarse(offset_ms: u64, duration_ms: Option<u64>, text: &str) -> Vec<Phrase> {
    let text = collapse(text);
    if text.is_empty() {
        return Vec::new();
    }
    vec![Phrase {
        start_ms: offset_ms,
        end_ms: offset_ms + duration_ms.unwrap_or(0),
        text,
        speaker: None,
    }]
}

/// Words or segments grouped into paragraphs (speaker change, pauses, cap).
fn paragraphs(units: &[Timed]) -> Vec<Timed> {
    group_words(units)
        .into_iter()
        .map(|group| Timed {
            text: group
                .iter()
                .map(|unit| unit.text.as_str())
                .collect::<Vec<_>>()
                .join(" "),
            start: group[0].start,
            end: group.iter().map(|unit| unit.end).max().unwrap_or(0),
            speaker: group[0].speaker.clone(),
        })
        .collect()
}

/// Phrases of one piece that starts `offset_ms` into the recording and lasts
/// `duration_ms` (when known; otherwise the provider's `duration` bounds it).
pub fn build(offset_ms: u64, duration_ms: Option<u64>, parsed: &Parsed) -> Vec<Phrase> {
    let limit = duration_ms.or(parsed.duration_ms);
    if let Some(words) = &parsed.words {
        let phrases = place(offset_ms, limit, paragraphs(words));
        if !phrases.is_empty() && covers(&phrases, &parsed.text) {
            return scoped(offset_ms, phrases);
        }
    }
    // A provider's segments are often single sentences: grouped by the same rules.
    if let Some(segments) = &parsed.segments {
        let phrases = place(offset_ms, limit, paragraphs(segments));
        if !phrases.is_empty() && covers(&phrases, &parsed.text) {
            return scoped(offset_ms, phrases);
        }
    }
    coarse(offset_ms, limit, &parsed.text)
}

/// The text of a response whose `text` field is empty but whose words or
/// segments are not.
pub fn fallback_text(parsed: &Parsed) -> String {
    for list in [&parsed.words, &parsed.segments].into_iter().flatten() {
        let text = list
            .iter()
            .map(|item| item.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        if !text.trim().is_empty() {
            return text;
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn word(text: &str, start: f64, end: f64) -> Value {
        json!({"word": text, "start": start, "end": end})
    }
    fn spoken(text: &str, start: f64, end: f64, speaker: i64) -> Value {
        json!({"word": text, "start": start, "end": end, "speaker": speaker})
    }
    fn phrases(value: Value) -> Vec<Phrase> {
        build(0, None, &parse(&value))
    }
    fn texts(phrases: &[Phrase]) -> Vec<&str> {
        phrases.iter().map(|phrase| phrase.text.as_str()).collect()
    }

    // Live Grok STT answer: one segment for the whole piece, per-word times, no speakers.
    const GROK_WORDS: &str = include_str!("../tests/fixtures/grok-words.json");
    // The same audio with `provider.options.xai.diarize`: per-word speaker ints.
    const GROK_SPEAKERS: &str = include_str!("../tests/fixtures/grok-speakers.json");

    #[test]
    fn grok_words_of_one_voice_read_as_one_paragraph() {
        // ~1.5 s pauses between sentences of the same voice do not break the text apart.
        let value: Value = serde_json::from_str(GROK_WORDS).unwrap();
        let result = build(0, Some(19_188), &parse(&value));
        assert_eq!(result.len(), 1);
        assert_eq!(
            (result[0].start_ms, result[0].end_ms),
            (60, 18_888),
            "first word start to last word end"
        );
        assert!(result[0].text.starts_with("Привет, давай"));
        assert!(result[0].text.ends_with("первые результаты."));
        assert!(result[0].speaker.is_none());
    }

    #[test]
    fn grok_speakers_are_scoped_to_the_request_that_heard_them() {
        let value: Value = serde_json::from_str(GROK_SPEAKERS).unwrap();
        let result = build(120_000, Some(19_188), &parse(&value));
        assert_eq!(
            result
                .iter()
                .map(|phrase| (phrase.start_ms, phrase.speaker.as_deref().unwrap()))
                .collect::<Vec<_>>(),
            [
                (120_060, "120000:0"),
                (127_323, "120000:1"),
                (134_866, "120000:0")
            ]
        );
        assert_eq!(
            result[1].text,
            "Согласен, я возьму выгрузку заказов, а остатки сделаем дельтами каждые пять минут."
        );
        // A second request with the same provider ids is a different scope.
        let later = build(240_000, Some(19_188), &parse(&value));
        assert_ne!(result[0].speaker, later[0].speaker);
    }

    #[test]
    fn a_new_phrase_starts_on_speaker_change_and_on_long_pauses() {
        // Speaker change without any pause.
        let result =
            phrases(json!({"text":"a b","words":[spoken("a",0.0,1.0,0),spoken("b",1.0,2.0,1)]}));
        assert_eq!(texts(&result), ["a", "b"]);
        // 2 s after a sentence end splits; 1.99 s does not.
        let result = phrases(json!({"text":"One. Two. three","words":[
            word("One.",0.0,1.0), word("Two.",3.0,3.2), word("three",5.19,5.4)]}));
        assert_eq!(texts(&result), ["One.", "Two. three"]);
        // Without a sentence end only 4 s splits; quotes/brackets after the stop still count.
        let result = phrases(json!({"text":"a b c «d.»","words":[
            word("a",0.0,1.0), word("b",4.9,5.0), word("c",9.0,9.1), word("«d.»",9.2,9.3)]}));
        assert_eq!(texts(&result), ["a b", "c «d.»"]);
        let result = phrases(json!({"text":"Ask? Yes… «Ok.» (fine.) end","words":[
            word("Ask?",0.0,0.5), word("Yes…",2.5,2.75), word("«Ok.»",4.75,5.0), word("(fine.)",7.0,7.25), word("end",9.25,9.5)]}));
        assert_eq!(texts(&result), ["Ask?", "Yes…", "«Ok.»", "(fine.)", "end"]);
    }

    #[test]
    fn a_long_phrase_is_cut_at_its_last_sentence_end_else_at_its_largest_gap() {
        // 120 words, 1 s each, no pauses; a sentence ends at word 9 and word 74.
        let words: Vec<Value> = (0..120)
            .map(|i| {
                let text = if i == 9 || i == 74 {
                    format!("w{i}.")
                } else {
                    format!("w{i}")
                };
                word(&text, i as f64, i as f64 + 1.0)
            })
            .collect();
        let text = words
            .iter()
            .map(|w| w["word"].as_str().unwrap())
            .collect::<Vec<_>>()
            .join(" ");
        let result = phrases(json!({"text": text, "words": words}));
        // The last sentence end within 90 s is w74 (ends at 75 s).
        assert_eq!(result.len(), 2);
        assert!(result[0].text.ends_with("w74."));
        assert_eq!((result[0].start_ms, result[0].end_ms), (0, 75_000));
        assert_eq!((result[1].start_ms, result[1].end_ms), (75_000, 120_000));
        // No sentence end: the largest gap within the limit (after w40), later on a tie.
        let words: Vec<Value> = (0..120)
            .map(|i| {
                let start = i as f64 * 1.0 + if i > 40 { 0.5 } else { 0.0 };
                word(&format!("w{i}"), start, start + 0.9)
            })
            .collect();
        let text = words
            .iter()
            .map(|w| w["word"].as_str().unwrap())
            .collect::<Vec<_>>()
            .join(" ");
        let result = phrases(json!({"text": text, "words": words}));
        assert!(result[0].text.ends_with("w40"), "{:?}", texts(&result));
        assert!(result
            .iter()
            .all(|p| p.end_ms - p.start_ms <= MAX_PHRASE_MS));
        // Even gaps everywhere: the latest gap inside the limit wins (w179 ends at 89.75 s).
        let words: Vec<Value> = (0..200)
            .map(|i| word(&format!("w{i}"), i as f64 * 0.5, i as f64 * 0.5 + 0.25))
            .collect();
        let text = words
            .iter()
            .map(|w| w["word"].as_str().unwrap())
            .collect::<Vec<_>>()
            .join(" ");
        let result = phrases(json!({"text": text, "words": words}));
        assert_eq!(result.len(), 2);
        assert!(result[0].text.ends_with("w179"));
        // One endless word stays whole.
        let result = phrases(json!({"text":"long","words":[word("long",0.0,120.0)]}));
        assert_eq!((result[0].start_ms, result[0].end_ms), (0, 120_000));
    }

    #[test]
    fn segments_of_one_speaker_close_together_become_one_paragraph() {
        let segments = json!([
            {"start":0.0,"end":1.0,"text":"Первый.","speaker":0},
            {"start":1.5,"end":2.0,"text":"Ещё.","speaker":0},
            {"start":2.1,"end":3.0,"text":"Второй.","speaker":1},
            {"start":6.0,"end":7.0,"text":"После паузы.","speaker":1}
        ]);
        let value = json!({"text":"Первый. Ещё. Второй. После паузы.","segments":segments});
        let result = build(0, None, &parse(&value));
        assert_eq!(
            result
                .iter()
                .map(|p| (p.text.as_str(), p.speaker.as_deref().unwrap()))
                .collect::<Vec<_>>(),
            [
                ("Первый. Ещё.", "0:0"),
                ("Второй.", "0:1"),
                ("После паузы.", "0:1")
            ]
        );
    }

    #[test]
    fn segments_are_used_when_words_are_missing_or_broken() {
        let segments = json!([
            {"start":0.0,"end":2.0,"text":" First part. ","speaker":"A"},
            {"start":2.5,"end":4.0,"text":"Second part."}
        ]);
        let result = build(
            1000,
            None,
            &parse(&json!({"text":"First part. Second part.","segments":segments})),
        );
        assert_eq!(
            result,
            [
                Phrase {
                    start_ms: 1000,
                    end_ms: 3000,
                    text: "First part.".into(),
                    speaker: Some("1000:A".into())
                },
                Phrase {
                    start_ms: 3500,
                    end_ms: 5000,
                    text: "Second part.".into(),
                    speaker: None
                },
            ]
        );
        // One word without a time: the words are not trusted, segments are.
        let broken = json!({"text":"First part. Second part.","segments":segments,
            "words":[word("First",0.0,0.5),{"word":"part.","start":null,"end":1.0}]});
        assert_eq!(build(1000, None, &parse(&broken)), result);
        let nan = json!({"text":"a b","segments":segments, "words":[word("a",0.0,0.5),{"word":"b","start":"x","end":1.0}]});
        assert!(parse(&nan).words.is_none());
    }

    #[test]
    fn segment_speakers_reach_the_words_inside_them() {
        let value = json!({"text":"hi there yes","segments":[
            {"start":0.0,"end":1.0,"text":"hi there","speaker":0},
            {"start":1.0,"end":2.0,"text":"yes","speaker":1}],
            "words":[word("hi",0.0,0.4),word("there",0.5,0.9),word("yes",1.1,1.5)]});
        let result = phrases(value);
        assert_eq!(texts(&result), ["hi there", "yes"]);
        assert_eq!(result[1].speaker.as_deref(), Some("0:1"));
    }

    #[test]
    fn missing_words_fall_back_to_the_provider_text_for_the_piece() {
        // The words carry only half of the text: keep every word of the text, coarsely.
        let value = json!({"text":"one two three four five six","duration":12.0,
            "segments":[{"start":0.0,"end":3.0,"text":"one two three"}],
            "words":[word("one",0.0,1.0),word("two",1.0,2.0),word("three",2.0,3.0)]});
        assert_eq!(
            build(5000, Some(10_000), &parse(&value)),
            [Phrase {
                start_ms: 5000,
                end_ms: 15_000,
                text: "one two three four five six".into(),
                speaker: None
            }]
        );
        // Punctuation and case differences are not missing words.
        let value = json!({"text":"Hello, World!","words":[
            word("hello",0.0,1.0), word("world",1.0,2.0), word("uh",2.0,2.5)]});
        assert_eq!(texts(&phrases(value)), ["hello world uh"]);
    }

    #[test]
    fn plain_text_is_one_phrase_for_the_piece_and_garbage_never_fails() {
        assert_eq!(
            build(60_000, Some(30_000), &parse(&json!({"text":" just text "}))),
            [Phrase {
                start_ms: 60_000,
                end_ms: 90_000,
                text: "just text".into(),
                speaker: None
            }]
        );
        // Without any known length the phrase is a point in time.
        assert_eq!(build(7, None, &parse(&json!({"text":"x"})))[0].end_ms, 7);
        for garbage in [
            json!(null),
            json!([]),
            json!({"text":5}),
            json!({"words":"no","segments":{}}),
            json!({"text":"","words":[{"word":"","start":0,"end":1}]}),
            json!({"text":"a","words":[{"word":"a","start":-1,"end":1}]}),
            json!({"text":"a","words":[{"word":"a","start":2,"end":1}]}),
        ] {
            let result = build(0, Some(1000), &parse(&garbage));
            assert!(
                result
                    .iter()
                    .all(|p| !p.text.is_empty() && p.start_ms <= p.end_ms),
                "{garbage}"
            );
        }
        assert!(build(0, None, &parse(&json!({}))).is_empty());
    }

    #[test]
    fn times_stay_inside_the_piece_in_order_without_overlap() {
        let value = json!({"text":"a b c","words":[
            spoken("a",0.0,3.0,0), spoken("b",1.0,2.0,1), spoken("c",8.0,99.0,0)]});
        let result = build(10_000, Some(5000), &parse(&value));
        assert_eq!(
            result
                .iter()
                .map(|p| (p.start_ms, p.end_ms))
                .collect::<Vec<_>>(),
            [(10_000, 13_000), (13_000, 13_000), (15_000, 15_000)]
        );
        // The provider's duration bounds the piece when its real length is unknown.
        let value = json!({"text":"a","duration":2.0,"words":[word("a",1.0,9.0)]});
        assert_eq!(build(0, None, &parse(&value))[0].end_ms, 2000);
    }

    #[test]
    fn bare_words_take_their_punctuation_from_the_text() {
        // Qwen3-ASR: words without punctuation or case next to a punctuated text.
        let value = json!({"text":"Привет, как дела? Хорошо. Спасибо!","words":[
            word("привет",0.0,0.5), word("как",0.6,0.8), word("дела",0.9,1.2),
            word("хорошо",3.2,3.6), word("спасибо",5.6,6.0)]});
        assert_eq!(
            texts(&phrases(value)),
            ["Привет, как дела?", "Хорошо.", "Спасибо!"]
        );
        // A different word sequence (here an extra word) keeps the words as they came.
        let value = json!({"text":"Привет, как дела? Хорошо.","words":[
            word("привет",0.0,0.5), word("как",0.6,0.8), word("твои",0.85,0.88), word("дела",0.9,1.2),
            word("хорошо",2.2,2.6)]});
        assert_eq!(texts(&phrases(value)), ["привет как твои дела хорошо"]);
        // Same count, one different word: no replacement either.
        let value =
            json!({"text":"One. Two.","words":[word("one",0.0,0.5), word("three",1.5,2.0)]});
        let kept: Vec<String> = parse(&value)
            .words
            .unwrap()
            .into_iter()
            .map(|word| word.text)
            .collect();
        assert_eq!(kept, ["one", "three"]);
    }

    #[test]
    fn segments_are_kept_one_by_one_and_odd_speaker_labels_are_normalized() {
        let value = json!({"text":"a b c","segments":[
            {"start":0.0,"end":1.0,"text":"a","speaker":1.0},
            {"start":"bad","end":2.0,"text":"lost"},
            {"start":2.0,"end":3.0,"text":"b c","speaker":true}]});
        let result = build(0, None, &parse(&value));
        assert_eq!(texts(&result), ["a", "b c"]);
        assert_eq!(result[0].speaker.as_deref(), Some("0:1"));
        assert_eq!(result[1].speaker, None);
        // Words that go back in time are kept in order, never overlapping.
        let value = json!({"text":"x y","words":[word("x",1.0,2.0), word("y",0.5,0.8)]});
        let result = phrases(value);
        assert_eq!(texts(&result), ["x y"]);
        assert_eq!((result[0].start_ms, result[0].end_ms), (1000, 2000));
        // A non-object word cannot be placed: the words are not trusted.
        assert!(parse(&json!({"text":"a","words":[word("a",0.0,1.0), 5]}))
            .words
            .is_none());
    }

    #[test]
    fn an_empty_text_field_is_rebuilt_from_words() {
        let parsed = parse(&json!({"words":[word("a",0.0,1.0),word("b",1.0,2.0)]}));
        assert_eq!(fallback_text(&parsed), "a b");
        assert_eq!(texts(&build(0, None, &parsed)), ["a b"]);
    }
}

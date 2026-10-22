// Finished recordings are captured as PCM WAV (crash-recoverable, append-only)
// and compacted once before they enter the archive. Compaction is strictly
// best-effort: any failure or failed verification keeps the original WAV.
use std::{
    fs,
    io::{BufWriter, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

/// AAC-LC bitrate for mono speech. 48 kbps is transparent for speech at
/// 16 kHz (higher rates are rejected by some encoders at that sample rate).
#[cfg_attr(
    not(any(target_os = "macos", target_os = "android", test)),
    allow(dead_code)
)]
pub fn aac_bitrate(sample_rate: u32) -> u32 {
    if sample_rate <= 16000 {
        48000
    } else {
        64000
    }
}

pub struct PcmInfo {
    pub sample_rate: u32,
    pub frames: u64,
}
pub fn pcm_info(path: &Path) -> Option<PcmInfo> {
    let reader = hound::WavReader::open(path).ok()?;
    let spec = reader.spec();
    if spec.bits_per_sample != 16
        || spec.sample_format != hound::SampleFormat::Int
        || !(1..=2).contains(&spec.channels)
        || !(8000..=192000).contains(&spec.sample_rate)
    {
        return None;
    }
    // Compact only a finalized file: a header that disagrees with the actual
    // PCM length could silently drop audio that the WAV still holds.
    let block = spec.channels as u64 * 2;
    let frames = reader.duration() as u64;
    let data = fs::metadata(path).ok()?.len().checked_sub(44)?;
    if data / block != frames {
        return None;
    }
    Some(PcmInfo {
        sample_rate: spec.sample_rate,
        frames,
    })
}

fn close_enough(actual: f64, expected: f64) -> bool {
    // AAC adds encoder priming/padding (about 0.15 s at 16 kHz).
    actual.is_finite() && (actual - expected).abs() <= 0.25 + expected * 0.005
}

// The archive commit renames this file; it must be on disk first.
fn durable(path: &Path) -> bool {
    fs::OpenOptions::new()
        .write(true)
        .open(path)
        .and_then(|file| file.sync_all())
        .is_ok()
}

fn verify_lossy(path: &Path, info: &PcmInfo) -> bool {
    let expected = info.frames as f64 / info.sample_rate as f64;
    fs::metadata(path).is_ok_and(|metadata| metadata.len() > 0)
        && crate::audio::duration(path).is_some_and(|seconds| close_enough(seconds, expected))
        // Decode the start: proves the codec configuration is readable by the
        // transcription decoder, not just that the container is well formed.
        && crate::audio::decoded_frames(path, Some(64)).is_ok_and(|frames| frames > 0)
}

/// Produces `<stem>.m4a` (AAC) or `<stem>.flac` from a finished PCM WAV and
/// verifies it. Returns None when the caller must keep the original file.
pub fn compact_recording(
    app: &tauri::AppHandle,
    source: &Path,
    stem: &Path,
) -> Option<(PathBuf, &'static str)> {
    let info = pcm_info(source)?;
    if info.frames == 0 {
        return None;
    }
    let aac = stem.with_extension("m4a");
    match encode_aac(app, source, &aac, &info) {
        Ok(()) if verify_lossy(&aac, &info) && durable(&aac) => return Some((aac, "m4a")),
        Ok(()) => eprintln!("Compacted AAC failed verification; trying FLAC"),
        Err(error) => eprintln!("AAC compaction unavailable: {error}"),
    }
    let _ = fs::remove_file(&aac);
    let flac = stem.with_extension("flac");
    match encode_flac(source, &flac) {
        // FLAC is lossless; require the full stream to decode to every frame.
        Ok(frames)
            if frames == info.frames
                && crate::audio::decoded_frames(&flac, None).ok() == Some(info.frames) =>
        {
            return Some((flac, "flac"))
        }
        Ok(_) => eprintln!("FLAC compaction failed verification; keeping WAV"),
        Err(error) => eprintln!("FLAC compaction failed: {error}"),
    }
    let _ = fs::remove_file(&flac);
    None
}

#[cfg(target_os = "macos")]
fn encode_aac(
    _app: &tauri::AppHandle,
    source: &Path,
    output: &Path,
    info: &PcmInfo,
) -> Result<(), String> {
    afconvert(source, output, aac_bitrate(info.sample_rate))
}
#[cfg(any(target_os = "macos", test))]
#[allow(dead_code)]
fn afconvert(source: &Path, output: &Path, bitrate: u32) -> Result<(), String> {
    // Built into every macOS install; uses the system AudioToolbox encoder.
    let tool = Path::new("/usr/bin/afconvert");
    if !tool.is_file() {
        return Err("afconvert is unavailable".into());
    }
    let status = std::process::Command::new(tool)
        .args(["-f", "m4af", "-d", "aac", "-b", &bitrate.to_string()])
        .arg(source)
        .arg(output)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|error| error.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("afconvert exited with {status}"))
    }
}

#[cfg(target_os = "android")]
fn encode_aac(
    app: &tauri::AppHandle,
    source: &Path,
    output: &Path,
    info: &PcmInfo,
) -> Result<(), String> {
    use tauri::Manager;
    // MediaCodec runs on a Kotlin worker. Bound the wait so a lost Activity
    // callback degrades to FLAC instead of blocking the archive forever.
    let seconds = info.frames / info.sample_rate as u64;
    let timeout = std::time::Duration::from_secs(60 + seconds / 4);
    let payload = serde_json::json!({
        "input": source,
        "output": output,
        "bitrate": aac_bitrate(info.sample_rate),
    });
    let app = app.clone();
    let (send, receive) = std::sync::mpsc::channel();
    tauri::async_runtime::spawn(async move {
        let result = app
            .state::<tauri_plugin_recording::Recording<tauri::Wry>>()
            .run_async::<serde_json::Value>("compactAudio", payload)
            .await;
        let _ = send.send(result);
    });
    receive
        .recv_timeout(timeout)
        .map_err(|_| "native AAC encoder timed out".to_string())?
        .map(|_| ())
}

#[cfg(not(any(target_os = "macos", target_os = "android")))]
fn encode_aac(
    _app: &tauri::AppHandle,
    _source: &Path,
    _output: &Path,
    _info: &PcmInfo,
) -> Result<(), String> {
    // Windows/Linux have no encoder that is guaranteed to be installed; FLAC
    // (pure Rust, lossless, playable by WebView2 and GStreamer) is used instead.
    Err("no system AAC encoder".into())
}

/// Streams a 16-bit PCM WAV into a FLAC file frame by frame (bounded memory).
/// Returns the number of encoded frames per channel.
pub fn encode_flac(source: &Path, output: &Path) -> Result<u64, String> {
    use flacenc::{
        bitsink::ByteSink,
        component::{BitRepr, Stream, StreamInfo},
        error::Verify,
        source::{Context, Fill, FrameBuf},
    };
    let mut reader = hound::WavReader::open(source).map_err(|error| error.to_string())?;
    let spec = reader.spec();
    if spec.bits_per_sample != 16 || spec.sample_format != hound::SampleFormat::Int {
        return Err("unsupported PCM format".into());
    }
    let channels = spec.channels as usize;
    let config = flacenc::config::Encoder::default()
        .into_verified()
        .map_err(|(_, error)| error.to_string())?;
    let block_size = config.block_size;
    let mut info = StreamInfo::new(spec.sample_rate as usize, channels, 16)
        .map_err(|error| error.to_string())?;
    let header = |info: &StreamInfo| -> Result<Vec<u8>, String> {
        let mut sink = ByteSink::new();
        Stream::with_stream_info(info.clone())
            .write(&mut sink)
            .map_err(|error| format!("{error:?}"))?;
        Ok(sink.as_slice().to_vec())
    };
    let placeholder = header(&info)?;
    let mut options = fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = BufWriter::new(options.open(output).map_err(|error| error.to_string())?);
    let result = (|| -> Result<u64, String> {
        file.write_all(&placeholder)
            .map_err(|error| error.to_string())?;
        let mut buffer = (
            FrameBuf::with_size(channels, block_size).map_err(|error| error.to_string())?,
            Context::new(16, channels),
        );
        let mut samples = reader.samples::<i16>();
        let mut interleaved = Vec::with_capacity(block_size * channels);
        let mut sink = ByteSink::new();
        loop {
            interleaved.clear();
            for sample in samples.by_ref().take(block_size * channels) {
                interleaved.push(sample.map_err(|error| error.to_string())? as i32);
            }
            // A truncated trailing frame (odd sample count) is dropped.
            interleaved.truncate(interleaved.len() / channels * channels);
            if interleaved.is_empty() {
                break;
            }
            buffer
                .fill_interleaved(&interleaved)
                .map_err(|error| error.to_string())?;
            let number = buffer.1.current_frame_number().ok_or("frame number")?;
            let frame = flacenc::encode_fixed_size_frame(&config, &buffer.0, number, &info)
                .map_err(|error| error.to_string())?;
            info.update_frame_info(&frame);
            sink.clear();
            frame
                .write(&mut sink)
                .map_err(|error| format!("{error:?}"))?;
            file.write_all(sink.as_slice())
                .map_err(|error| error.to_string())?;
        }
        info.set_md5_digest(&buffer.1.md5_digest());
        let total = info.total_samples() as u64;
        // STREAMINFO block sizes exclude the shorter last block; otherwise
        // decoders treat the stream as variable-blocksize.
        let fixed = block_size.min(total as usize).max(16);
        info.set_block_sizes(fixed, fixed)
            .map_err(|error| error.to_string())?;
        let finished = header(&info)?;
        if finished.len() != placeholder.len() {
            return Err("FLAC header size changed".into());
        }
        let mut inner = file.into_inner().map_err(|error| error.to_string())?;
        inner
            .seek(SeekFrom::Start(0))
            .and_then(|_| inner.write_all(&finished))
            .and_then(|_| inner.sync_all())
            .map_err(|error| error.to_string())?;
        Ok(total)
    })();
    if result.is_err() {
        let _ = fs::remove_file(output);
    }
    result
}

/// Whether an archived input is the same recording as a retained source. A
/// compacted archive cannot match by hash, so equal decoded duration is used.
pub fn archived_matches(source: &Path, archived: &Path) -> Result<bool, String> {
    if crate::transfer::hash_file(source)? == crate::transfer::hash_file(archived)? {
        return Ok(true);
    }
    let same_format = source.extension() == archived.extension();
    Ok(!same_format
        && pcm_info(source).is_some_and(|info| {
            info.frames > 0
                && crate::audio::duration(archived).is_some_and(|seconds| {
                    close_enough(seconds, info.frames as f64 / info.sample_rate as f64)
                })
        }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn speech_like_wav(path: &Path, rate: u32, seconds: u32) {
        let mut writer = hound::WavWriter::create(
            path,
            hound::WavSpec {
                channels: 1,
                sample_rate: rate,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .unwrap();
        let mut noise = 12345u32;
        for index in 0..rate * seconds {
            let t = index as f64 / rate as f64;
            noise = noise.wrapping_mul(1103515245).wrapping_add(12345);
            let jitter = ((noise >> 16) as f64 / 65536.0 - 0.5) * 0.02;
            let voice = (2.0 * std::f64::consts::PI * 180.0 * t).sin() * 0.3
                + (2.0 * std::f64::consts::PI * 720.0 * t).sin() * 0.1;
            let envelope = 0.5 + 0.5 * (2.0 * std::f64::consts::PI * 3.0 * t).sin();
            writer
                .write_sample(((voice * envelope + jitter) * 32767.0) as i16)
                .unwrap();
        }
        writer.finalize().unwrap();
    }
    fn directory() -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("hearfolio-compact-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&directory).unwrap();
        directory
    }

    #[test]
    fn flac_compaction_is_lossless_smaller_and_chunks_at_16_khz() {
        let dir = directory();
        let source = dir.join("hearfolio-recording-1.wav");
        speech_like_wav(&source, 16000, 35);
        let output = dir.join("compact.flac");
        let frames = encode_flac(&source, &output).unwrap();
        assert_eq!(frames, 16000 * 35);
        assert_eq!(crate::audio::decoded_frames(&output, None).unwrap(), frames);
        assert!((crate::audio::duration(&output).unwrap() - 35.0).abs() < 1e-6);
        assert_eq!(crate::audio::detect_extension(&output).unwrap(), "flac");
        assert!(fs::metadata(&output).unwrap().len() < fs::metadata(&source).unwrap().len());
        assert!(archived_matches(&source, &output).unwrap());
        // Lossless: decoded chunks equal the source PCM sample for sample.
        let chunks = crate::audio::prepare_cloud_chunks(&output, &dir, 30, |_, _| {}).unwrap();
        assert_eq!(chunks.len(), 2);
        let mut first = hound::WavReader::open(&chunks[0]).unwrap();
        assert_eq!(first.spec().sample_rate, 16000);
        assert_eq!(first.duration(), 16000 * 30);
        assert_eq!(
            hound::WavReader::open(&chunks[1]).unwrap().duration(),
            16000 * 5
        );
        let original: Vec<i16> = hound::WavReader::open(&source)
            .unwrap()
            .samples::<i16>()
            .take(1000)
            .map(Result::unwrap)
            .collect();
        let decoded: Vec<i16> = first
            .samples::<i16>()
            .take(1000)
            .map(Result::unwrap)
            .collect();
        assert_eq!(original, decoded);
        // A different recording of another length never matches.
        let other = dir.join("other.wav");
        speech_like_wav(&other, 16000, 3);
        assert!(!archived_matches(&other, &output).unwrap());
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn afconvert_aac_is_small_decodable_and_chunks_at_16_khz() {
        let dir = directory();
        let source = dir.join("hearfolio-recording-2.wav");
        speech_like_wav(&source, 16000, 40);
        let info = pcm_info(&source).unwrap();
        let output = dir.join("compact.m4a");
        afconvert(&source, &output, aac_bitrate(16000)).unwrap();
        assert!(verify_lossy(&output, &info));
        assert_eq!(crate::audio::detect_extension(&output).unwrap(), "m4a");
        let size = fs::metadata(&output).unwrap().len();
        // About 48 kbps: 40 s is roughly 240 KB instead of 1.28 MB of PCM.
        assert!(size < 300_000, "unexpected AAC size {size}");
        assert!(archived_matches(&source, &output).unwrap());
        let chunks = crate::audio::prepare_cloud_chunks(&output, &dir, 30, |_, _| {}).unwrap();
        assert_eq!(chunks.len(), 2);
        let first = hound::WavReader::open(&chunks[0]).unwrap();
        assert_eq!(first.spec().sample_rate, 16000);
        assert_eq!(first.duration(), 16000 * 30);
        fs::remove_dir_all(dir).unwrap();
    }
}

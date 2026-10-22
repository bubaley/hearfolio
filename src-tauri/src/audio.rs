use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};
use symphonia::core::{
    audio::SampleBuffer, codecs::DecoderOptions, errors::Error, formats::FormatOptions,
    io::MediaSourceStream, meta::MetadataOptions, probe::Hint,
};

fn probe(path: &Path) -> Result<symphonia::core::probe::ProbeResult, String> {
    let file = fs::File::open(path).map_err(|error| format!("errors.audioRead|{error}"))?;
    let mut hint = Hint::new();
    if let Some(extension) = path.extension().and_then(|extension| extension.to_str()) {
        hint.with_extension(extension);
    }
    symphonia::default::get_probe()
        .format(
            &hint,
            MediaSourceStream::new(Box::new(file), Default::default()),
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|error| format!("errors.audioDecodeUnsupported|{error}"))
}

pub fn duration(path: &Path) -> Option<f64> {
    let probed = probe(path).ok()?;
    let track = probed.format.default_track()?;
    let track_id = track.id;
    let parameters = track.codec_params.clone();
    duration_from_packets(probed, parameters, track_id)
}
fn duration_from_packets(
    mut probed: symphonia::core::probe::ProbeResult,
    parameters: symphonia::core::codecs::CodecParameters,
    track_id: u32,
) -> Option<f64> {
    let seconds_for_frames = |frames| {
        if let Some(time_base) = parameters.time_base {
            let time = time_base.calc_time(frames);
            Some(time.seconds as f64 + time.frac)
        } else {
            parameters
                .sample_rate
                .filter(|rate| *rate > 0)
                .map(|rate| frames as f64 / rate as f64)
        }
    };
    if let Some(seconds) = parameters
        .n_frames
        .and_then(seconds_for_frames)
        .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
    {
        return Some(seconds);
    }
    // Raw AAC and some recorders omit n_frames. Scan packet timestamps without
    // decoding PCM or buffering the recording, then use the track's time base.
    let mut first = None;
    let mut end = 0;
    loop {
        match probed.format.next_packet() {
            Ok(packet) if packet.track_id() == track_id => {
                first.get_or_insert(packet.ts());
                end = end.max(packet.ts().checked_add(packet.dur())?);
            }
            Ok(_) => {}
            Err(Error::IoError(error)) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                break
            }
            Err(_) => return None,
        }
    }
    seconds_for_frames(end.checked_sub(first?)?)
        .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
}

/// Decodes up to `packet_limit` packets of the default track and returns the
/// decoded frame count. Any decode error fails verification.
pub fn decoded_frames(path: &Path, packet_limit: Option<usize>) -> Result<u64, String> {
    let mut probed = probe(path)?;
    let track = probed.format.default_track().ok_or("errors.audioEmpty")?;
    let track_id = track.id;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|error| format!("errors.audioDecodeUnsupported|{error}"))?;
    let (mut frames, mut packets) = (0u64, 0usize);
    while packet_limit.is_none_or(|limit| packets < limit) {
        let packet = match probed.format.next_packet() {
            Ok(packet) => packet,
            Err(Error::IoError(error)) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                break
            }
            Err(error) => return Err(format!("errors.audioDecode|{error}")),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = decoder
            .decode(&packet)
            .map_err(|error| format!("errors.audioDecode|{error}"))?;
        frames += decoded.frames() as u64;
        packets += 1;
    }
    Ok(frames)
}

// SAF providers may return an opaque URI with no extension. Inspect the copied
// bytes, never use a URI suffix as a filesystem name.
pub fn detect_extension(path: &Path) -> Result<&'static str, String> {
    let mut bytes = [0u8; 16];
    let count = fs::File::open(path)
        .and_then(|mut file| file.read(&mut bytes))
        .map_err(|error| format!("errors.audioRead|{error}"))?;
    let bytes = &bytes[..count];
    let extension = if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WAVE") {
        "wav"
    } else if bytes.starts_with(b"fLaC") {
        "flac"
    } else if bytes.starts_with(b"OggS") {
        "ogg"
    } else if bytes.get(4..8) == Some(b"ftyp") {
        "m4a"
    } else if bytes.starts_with(b"ID3") {
        "mp3"
    } else if bytes.len() >= 2 && bytes[0] == 0xff && bytes[1] & 0xf6 == 0xf0 {
        "aac"
    } else if bytes.len() >= 2 && bytes[0] == 0xff && bytes[1] & 0xe0 == 0xe0 {
        "mp3"
    } else {
        return Err("errors.audioDecodeUnsupported".into());
    };
    // Detection alone does not guarantee that an audio track is present.
    let probed = probe(path)?;
    if probed.format.default_track().is_none() {
        return Err("errors.audioEmpty".into());
    }
    Ok(extension)
}

/// Speech transcription gains nothing above 8 kHz of audio bandwidth. Cloud
/// chunks and new recordings are stored at this rate to keep uploads small.
pub const SPEECH_SAMPLE_RATE: u32 = 16000;

/// Streaming windowed-sinc (Kaiser) decimator. Memory is bounded by the filter
/// length regardless of input length; it never upsamples (higher output rates
/// pass audio through unchanged).
pub struct Resampler {
    passthrough: bool,
    input_rate: u64,
    output_rate: u64,
    half: i64,
    table: Vec<f32>,
    buffer: Vec<f32>,
    // Absolute input index of buffer[0]; output n is centred exactly at
    // n * input_rate / output_rate, so results never depend on chunking.
    start: i64,
    consumed: u64,
    produced: u64,
}
const RESAMPLER_TABLE_RESOLUTION: f64 = 128.0;
impl Resampler {
    pub fn new(input_rate: u32, output_rate: u32) -> Self {
        let passthrough = output_rate == 0 || output_rate >= input_rate;
        let step = if passthrough {
            1.0
        } else {
            input_rate as f64 / output_rate as f64
        };
        // Cut off just below the output Nyquist frequency. Kaiser beta 8.6 gives
        // roughly 85 dB of stop-band attenuation with 16 zero crossings a side.
        let cutoff = 0.9 / step;
        let half = if passthrough {
            0.0
        } else {
            (16.0 / cutoff).ceil()
        };
        let beta = 8.6;
        let bessel = |x: f64| {
            let (mut sum, mut term, mut k) = (1.0f64, 1.0f64, 1.0f64);
            while term > 1e-12 * sum {
                term *= (x / (2.0 * k)).powi(2);
                sum += term;
                k += 1.0;
            }
            sum
        };
        let size = (2.0 * half * RESAMPLER_TABLE_RESOLUTION) as usize + 2;
        let table = (0..size)
            .map(|index| {
                let x = index as f64 / RESAMPLER_TABLE_RESOLUTION - half;
                if passthrough || x.abs() > half {
                    return 0.0;
                }
                let argument = std::f64::consts::PI * cutoff * x;
                let sinc = if argument.abs() < 1e-9 {
                    1.0
                } else {
                    argument.sin() / argument
                };
                let ratio = x / half;
                let window = bessel(beta * (1.0 - ratio * ratio).max(0.0).sqrt()) / bessel(beta);
                (cutoff * sinc * window) as f32
            })
            .collect();
        Self {
            passthrough,
            input_rate: input_rate as u64,
            output_rate: output_rate.max(1) as u64,
            half: half as i64,
            table,
            // Zero history before input 0 (no filter delay in the output).
            buffer: vec![0.0; half as usize],
            start: -(half as i64),
            consumed: 0,
            produced: 0,
        }
    }
    pub fn process(&mut self, input: &[f32], output: &mut Vec<f32>) {
        if self.passthrough {
            output.extend_from_slice(input);
            return;
        }
        self.buffer.extend_from_slice(input);
        self.consumed += input.len() as u64;
        self.drain(output, u64::MAX);
    }
    /// Flushes the filter tail. The output then has ceil(input / step) samples.
    pub fn finish(&mut self, output: &mut Vec<f32>) {
        if self.passthrough {
            return;
        }
        let expected = (self.consumed * self.output_rate).div_ceil(self.input_rate);
        self.buffer
            .extend(std::iter::repeat_n(0.0, self.half as usize + 2));
        self.drain(output, expected);
    }
    fn drain(&mut self, output: &mut Vec<f32>, limit: u64) {
        while self.produced < limit {
            let numerator = self.produced * self.input_rate;
            let whole = (numerator / self.output_rate) as i64;
            let remainder = numerator % self.output_rate;
            let last = whole + self.half;
            if last - self.start >= self.buffer.len() as i64 {
                break;
            }
            let first = whole - self.half + i64::from(remainder > 0);
            let fraction = remainder as f64 / self.output_rate as f64;
            let mut sum = 0.0f32;
            for index in first..=last {
                // Offset from the left kernel edge: index - centre + half.
                let offset =
                    ((index - whole + self.half) as f64 - fraction) * RESAMPLER_TABLE_RESOLUTION;
                let base = offset as usize;
                let weight_fraction = (offset - base as f64) as f32;
                let weight =
                    self.table[base] + (self.table[base + 1] - self.table[base]) * weight_fraction;
                sum += self.buffer[(index - self.start) as usize] * weight;
            }
            output.push(sum);
            self.produced += 1;
        }
        let next = (self.produced * self.input_rate / self.output_rate) as i64;
        let keep = next - self.half - 1 - self.start;
        if keep >= 8192 {
            self.buffer.drain(..keep as usize);
            self.start += keep;
        }
    }
}

struct ChunkWriter<'a> {
    destination: &'a Path,
    rate: u32,
    seconds: u32,
    writer: Option<hound::WavWriter<std::io::BufWriter<fs::File>>>,
    in_chunk: u64,
    chunks: Vec<PathBuf>,
}
impl ChunkWriter<'_> {
    fn push(&mut self, value: f32) -> Result<(), String> {
        if self.in_chunk == self.rate as u64 * u64::from(self.seconds) {
            if let Some(writer) = self.writer.take() {
                writer
                    .finalize()
                    .map_err(|error| format!("errors.audioPrepare|{error}"))?;
            }
            self.in_chunk = 0;
        }
        if self.writer.is_none() {
            let path = self
                .destination
                .join(format!("chunk-{:06}.wav", self.chunks.len()));
            let spec = hound::WavSpec {
                channels: 1,
                sample_rate: self.rate,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            };
            self.writer = Some(
                hound::WavWriter::create(&path, spec)
                    .map_err(|error| format!("errors.audioPrepare|{error}"))?,
            );
            self.chunks.push(path);
        }
        let sample = if value.is_finite() {
            (value * 32767.0).round().clamp(-32768.0, 32767.0) as i16
        } else {
            0
        };
        self.writer
            .as_mut()
            .unwrap()
            .write_sample(sample)
            .map_err(|error| format!("errors.audioPrepare|{error}"))?;
        self.in_chunk += 1;
        Ok(())
    }
    fn finish(mut self) -> Result<Vec<PathBuf>, String> {
        if let Some(writer) = self.writer.take() {
            writer
                .finalize()
                .map_err(|error| format!("errors.audioPrepare|{error}"))?;
        }
        Ok(self.chunks)
    }
}

// Each decoded packet is bounded by the codec. Mono PCM is resampled to at
// most 16 kHz and written directly to disk in `seconds`-long pieces; an hour-long
// recording never becomes an hour-long memory buffer. A corrupt packet is
// skipped (as players do) instead of discarding the whole recording.
pub fn prepare_cloud_chunks(
    input: &Path,
    destination: &Path,
    seconds: u32,
    mut progress: impl FnMut(u64, u64),
) -> Result<Vec<PathBuf>, String> {
    let mut probed = probe(input)?;
    let track = probed.format.default_track().ok_or("errors.audioEmpty")?;
    let track_id = track.id;
    let parameters = track.codec_params.clone();
    let total = parameters.n_frames.unwrap_or(0);
    let mut decoder = symphonia::default::get_codecs()
        .make(&parameters, &DecoderOptions::default())
        .map_err(|error| format!("errors.audioDecodeUnsupported|{error}"))?;
    let mut output: Option<(ChunkWriter, Resampler)> = None;
    let mut sample_rate = 0u32;
    let mut decoded_frames = 0u64;
    let mut skipped = 0u64;
    let mut last_error = String::new();
    let mut mono = Vec::new();
    let mut resampled = Vec::new();
    loop {
        let packet = match probed.format.next_packet() {
            Ok(packet) => packet,
            Err(Error::IoError(error)) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                break
            }
            Err(error) => return Err(format!("errors.audioDecode|{error}")),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(decoded) => decoded,
            Err(Error::DecodeError(error)) => {
                skipped += 1;
                last_error = error.to_string();
                continue;
            }
            Err(error) => return Err(format!("errors.audioDecode|{error}")),
        };
        let spec = *decoded.spec();
        let channels = spec.channels.count();
        if channels == 0 || spec.rate == 0 {
            return Err("errors.audioDecodeUnsupported".into());
        }
        if sample_rate != 0 && sample_rate != spec.rate {
            return Err("errors.audioSampleRateChanged".into());
        }
        sample_rate = spec.rate;
        let (writer, resampler) = output.get_or_insert_with(|| {
            let rate = sample_rate.min(SPEECH_SAMPLE_RATE);
            (
                ChunkWriter {
                    destination,
                    rate,
                    seconds: seconds.max(1),
                    writer: None,
                    in_chunk: 0,
                    chunks: Vec::new(),
                },
                Resampler::new(sample_rate, rate),
            )
        });
        let mut buffer = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
        buffer.copy_interleaved_ref(decoded);
        mono.clear();
        mono.extend(
            buffer
                .samples()
                .chunks_exact(channels)
                .map(|frame| frame.iter().sum::<f32>() / channels as f32),
        );
        decoded_frames += mono.len() as u64;
        resampled.clear();
        resampler.process(&mono, &mut resampled);
        for value in &resampled {
            writer.push(*value)?;
        }
        progress(decoded_frames, total.max(decoded_frames));
    }
    let Some((mut writer, mut resampler)) = output else {
        return Err(if skipped > 0 {
            format!("errors.audioDecode|{last_error}")
        } else {
            "errors.audioEmpty".into()
        });
    };
    resampled.clear();
    resampler.finish(&mut resampled);
    for value in &resampled {
        writer.push(*value)?;
    }
    let chunks = writer.finish()?;
    if chunks.is_empty() {
        return Err("errors.audioEmpty".into());
    }
    Ok(chunks)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> PathBuf {
        let unique = uuid::Uuid::new_v4();
        let directory = std::env::temp_dir().join(format!("hearfolio-decode-{unique}"));
        fs::create_dir_all(&directory).unwrap();
        directory
    }
    #[test]
    fn decoder_splits_stereo_into_bounded_mono_chunks_without_ffmpeg() {
        let dir = fixture();
        let input = dir.join("source.wav");
        let mut writer = hound::WavWriter::create(
            &input,
            hound::WavSpec {
                channels: 2,
                sample_rate: 16000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .unwrap();
        for _ in 0..16000 * 31 {
            writer.write_sample::<i16>(1000).unwrap();
            writer.write_sample::<i16>(3000).unwrap();
        }
        writer.finalize().unwrap();
        let chunks = prepare_cloud_chunks(&input, &dir, 30, |_, _| {}).unwrap();
        assert_eq!(chunks.len(), 2);
        let mut first = hound::WavReader::open(&chunks[0]).unwrap();
        assert_eq!(first.spec().channels, 1);
        assert_eq!(first.duration(), 16000 * 30);
        assert_eq!(first.samples::<i16>().next().unwrap().unwrap(), 2000);
        assert_eq!(
            hound::WavReader::open(&chunks[1]).unwrap().duration(),
            16000
        );
        // The configured length decides the boundaries: 31 s fits one 60 s piece.
        let longer = dir.join("longer");
        fs::create_dir_all(&longer).unwrap();
        let single = prepare_cloud_chunks(&input, &longer, 60, |_, _| {}).unwrap();
        assert_eq!(single.len(), 1);
        assert_eq!(
            hound::WavReader::open(&single[0]).unwrap().duration(),
            16000 * 31
        );
        assert_eq!(detect_extension(&input).unwrap(), "wav");
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn packet_duration_handles_aac_without_container_frame_count() {
        let dir = fixture();
        let input = dir.join("source.aac");
        fs::write(&input, include_bytes!("../tests/fixtures/sine.aac")).unwrap();
        let probed = probe(&input).unwrap();
        let track = probed.format.default_track().unwrap();
        let track_id = track.id;
        let mut parameters = track.codec_params.clone();
        // Model a recorder/stream that omitted frame count in its metadata.
        parameters.n_frames = None;
        let seconds = duration_from_packets(probed, parameters, track_id).unwrap();
        assert!(
            (0.5..0.56).contains(&seconds),
            "unexpected duration: {seconds}"
        );
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn native_pcm_recording_preserves_exact_sample_duration_and_playable_audio() {
        let dir = fixture();
        let input = dir.join("recording.wav");
        let mut writer = hound::WavWriter::create(
            &input,
            hound::WavSpec {
                channels: 1,
                sample_rate: 48000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .unwrap();
        for index in 0..24000 {
            writer
                .write_sample(
                    (f64::sin(index as f64 * 2.0 * std::f64::consts::PI * 440.0 / 48000.0)
                        * 16000.0) as i16,
                )
                .unwrap();
        }
        writer.finalize().unwrap();
        assert_eq!(duration(&input), Some(0.5));
        assert_eq!(detect_extension(&input).unwrap(), "wav");
        let chunks = prepare_cloud_chunks(&input, &dir, 30, |_, _| {}).unwrap();
        let mut decoded = hound::WavReader::open(&chunks[0]).unwrap();
        // 48 kHz sources are uploaded at 16 kHz with the same duration.
        assert_eq!(decoded.spec().sample_rate, 16000);
        assert_eq!(decoded.duration(), 8000);
        let energy: f64 = decoded
            .samples::<i16>()
            .map(|sample| (sample.unwrap() as f64 / 32768.0).powi(2))
            .sum();
        assert!((energy / 8000.0).sqrt() > 0.3);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn resampler_is_chunking_invariant_keeps_speech_band_and_rejects_aliases() {
        let rate = 44100;
        let tone = |frequency: f64, count: usize| -> Vec<f32> {
            (0..count)
                .map(|i| {
                    (i as f64 * 2.0 * std::f64::consts::PI * frequency / rate as f64).sin() as f32
                        * 0.5
                })
                .collect()
        };
        let rms = |samples: &[f32]| {
            let body = &samples[400..samples.len() - 400];
            (body.iter().map(|s| (*s as f64).powi(2)).sum::<f64>() / body.len() as f64).sqrt()
        };
        let input = tone(1000.0, rate as usize);
        let mut whole = Vec::new();
        let mut resampler = Resampler::new(rate, 16000);
        resampler.process(&input, &mut whole);
        resampler.finish(&mut whole);
        assert_eq!(whole.len(), 16000);
        let mut pieces = Vec::new();
        let mut resampler = Resampler::new(rate, 16000);
        for piece in input.chunks(4093) {
            resampler.process(piece, &mut pieces);
        }
        resampler.finish(&mut pieces);
        assert_eq!(whole, pieces);
        assert_eq!(Resampler::new(48000, 16000).half, 54);
        // 1 kHz passes at unity gain (0.5 peak = 0.354 RMS).
        assert!((rms(&whole) - 0.3536).abs() < 0.01, "{}", rms(&whole));
        // 12 kHz would alias to 4 kHz; it must be strongly attenuated (> 60 dB).
        let mut aliased = Vec::new();
        let mut resampler = Resampler::new(rate, 16000);
        resampler.process(&tone(12000.0, rate as usize), &mut aliased);
        resampler.finish(&mut aliased);
        assert!(rms(&aliased) < 0.3536 / 1000.0, "{}", rms(&aliased));
        // Equal or lower input rates pass through untouched.
        let mut same = Vec::new();
        let mut passthrough = Resampler::new(16000, 16000);
        passthrough.process(&input[..100], &mut same);
        passthrough.finish(&mut same);
        assert_eq!(same, input[..100].to_vec());
    }
    #[test]
    fn decoder_rejects_non_audio_bytes() {
        let dir = fixture();
        let input = dir.join("not-audio.m4a");
        fs::write(&input, "not audio").unwrap();
        assert!(prepare_cloud_chunks(&input, &dir, 30, |_, _| {})
            .unwrap_err()
            .starts_with("errors.audioDecodeUnsupported"));
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn decoder_reads_aac_m4a_without_external_processes() {
        let dir = fixture();
        let input = dir.join("source.m4a");
        fs::write(&input, include_bytes!("../tests/fixtures/sine.m4a")).unwrap();
        let input = std::env::var_os("HEARFOLIO_TEST_M4A")
            .map(PathBuf::from)
            .unwrap_or(input);
        assert_eq!(detect_extension(&input).unwrap(), "m4a");
        assert!(duration(&input).unwrap() > 0.0);
        let chunks = prepare_cloud_chunks(&input, &dir, 30, |_, _| {}).unwrap();
        let mut decoded = hound::WavReader::open(&chunks[0]).unwrap();
        assert_eq!(decoded.spec().channels, 1);
        assert!(decoded.duration() > 0);
        assert!(decoded.samples::<i16>().any(|sample| sample.unwrap() != 0));
        fs::remove_dir_all(dir).unwrap();
    }
}

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
    let parameters = &probed.format.default_track()?.codec_params;
    let frames = parameters.n_frames?;
    let seconds = if let Some(time_base) = parameters.time_base {
        let time = time_base.calc_time(frames);
        time.seconds as f64 + time.frac
    } else {
        frames as f64 / parameters.sample_rate? as f64
    };
    (seconds.is_finite() && seconds > 0.0).then_some(seconds)
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

// Each decoded packet is bounded by the codec. Mono PCM is written directly to
// disk in 30-second pieces at the original sample rate; an hour-long recording
// never becomes an hour-long memory buffer.
pub fn prepare_cloud_chunks(
    input: &Path,
    destination: &Path,
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
    let mut chunks = Vec::new();
    let mut writer: Option<hound::WavWriter<std::io::BufWriter<fs::File>>> = None;
    let mut sample_rate = 0u32;
    let mut samples_in_chunk = 0u64;
    let mut decoded_frames = 0u64;
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
        let decoded = decoder
            .decode(&packet)
            .map_err(|error| format!("errors.audioDecode|{error}"))?;
        let spec = *decoded.spec();
        let channels = spec.channels.count();
        if channels == 0 || spec.rate == 0 {
            return Err("errors.audioDecodeUnsupported".into());
        }
        if sample_rate != 0 && sample_rate != spec.rate {
            return Err("errors.audioSampleRateChanged".into());
        }
        sample_rate = spec.rate;
        let mut buffer = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
        buffer.copy_interleaved_ref(decoded);
        for frame in buffer.samples().chunks_exact(channels) {
            if samples_in_chunk == sample_rate as u64 * 30 {
                writer
                    .take()
                    .unwrap()
                    .finalize()
                    .map_err(|error| format!("errors.audioPrepare|{error}"))?;
                samples_in_chunk = 0;
            }
            if writer.is_none() {
                let path = destination.join(format!("chunk-{:06}.wav", chunks.len()));
                let spec = hound::WavSpec {
                    channels: 1,
                    sample_rate,
                    bits_per_sample: 16,
                    sample_format: hound::SampleFormat::Int,
                };
                writer = Some(
                    hound::WavWriter::create(&path, spec)
                        .map_err(|error| format!("errors.audioPrepare|{error}"))?,
                );
                chunks.push(path);
            }
            let mono = frame.iter().sum::<f32>() / channels as f32;
            let sample = if mono.is_finite() {
                (mono * 32767.0).round().clamp(-32768.0, 32767.0) as i16
            } else {
                0
            };
            writer
                .as_mut()
                .unwrap()
                .write_sample(sample)
                .map_err(|error| format!("errors.audioPrepare|{error}"))?;
            samples_in_chunk += 1;
            decoded_frames += 1;
        }
        progress(decoded_frames, total.max(decoded_frames));
    }
    if let Some(writer) = writer {
        writer
            .finalize()
            .map_err(|error| format!("errors.audioPrepare|{error}"))?;
    }
    if chunks.is_empty() {
        return Err("errors.audioEmpty".into());
    }
    Ok(chunks)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
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
        let chunks = prepare_cloud_chunks(&input, &dir, |_, _| {}).unwrap();
        assert_eq!(chunks.len(), 2);
        let mut first = hound::WavReader::open(&chunks[0]).unwrap();
        assert_eq!(first.spec().channels, 1);
        assert_eq!(first.duration(), 16000 * 30);
        assert_eq!(first.samples::<i16>().next().unwrap().unwrap(), 2000);
        assert_eq!(
            hound::WavReader::open(&chunks[1]).unwrap().duration(),
            16000
        );
        assert_eq!(detect_extension(&input).unwrap(), "wav");
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn decoder_rejects_non_audio_bytes() {
        let dir = fixture();
        let input = dir.join("not-audio.m4a");
        fs::write(&input, "not audio").unwrap();
        assert!(prepare_cloud_chunks(&input, &dir, |_, _| {})
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
        let chunks = prepare_cloud_chunks(&input, &dir, |_, _| {}).unwrap();
        let mut decoded = hound::WavReader::open(&chunks[0]).unwrap();
        assert_eq!(decoded.spec().channels, 1);
        assert!(decoded.duration() > 0);
        assert!(decoded.samples::<i16>().any(|sample| sample.unwrap() != 0));
        fs::remove_dir_all(dir).unwrap();
    }
}

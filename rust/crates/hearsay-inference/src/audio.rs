//! Minimal WAV reading for the offline ASR path: a 16 kHz mono `f32` buffer, downmixing stereo.

use std::path::Path;

use crate::error::InferenceError;

/// Contract-fixed ASR input rate (Hz).
pub const SAMPLE_RATE: u32 = 16_000;

/// Read a 16 kHz WAV as mono `f32` in [-1, 1] (stereo is downmixed by averaging channels). Errors
/// if the file is not 16 kHz (no resampler here — the pipeline captures at 16 kHz).
pub fn read_wav_mono_16k(path: impl AsRef<Path>) -> Result<Vec<f32>, InferenceError> {
    let mut reader = hound::WavReader::open(path)
        .map_err(|e| InferenceError::Audio(format!("open wav: {e}")))?;
    let spec = reader.spec();
    if spec.sample_rate != SAMPLE_RATE {
        return Err(InferenceError::Audio(format!(
            "expected {SAMPLE_RATE} Hz, got {}",
            spec.sample_rate
        )));
    }
    let interleaved: Vec<f32> = match (spec.sample_format, spec.bits_per_sample) {
        (hound::SampleFormat::Int, 16) => reader
            .samples::<i16>()
            .map(|s| s.map(|v| v as f32 / 32768.0))
            .collect::<Result<_, _>>()
            .map_err(|e| InferenceError::Audio(format!("read samples: {e}")))?,
        (hound::SampleFormat::Float, 32) => reader
            .samples::<f32>()
            .collect::<Result<_, _>>()
            .map_err(|e| InferenceError::Audio(format!("read samples: {e}")))?,
        (_, bits) => {
            return Err(InferenceError::Audio(format!(
                "unsupported wav sample format ({bits}-bit)"
            )))
        }
    };

    let channels = spec.channels as usize;
    if channels <= 1 {
        return Ok(interleaved);
    }
    Ok(interleaved
        .chunks(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect())
}

/// Read the Them (right) channel of the stereo 16 kHz `audio.wav` as mono `f32` (the refine reads
/// it — it is as clean as a Them-only recording since Me/Them are separate capture devices). Falls
/// back to a mono file's only channel.
pub fn read_them_channel(path: impl AsRef<Path>) -> Result<Vec<f32>, InferenceError> {
    let mut reader = hound::WavReader::open(path)
        .map_err(|e| InferenceError::Audio(format!("open wav: {e}")))?;
    let spec = reader.spec();
    if spec.sample_rate != SAMPLE_RATE {
        return Err(InferenceError::Audio(format!(
            "expected {SAMPLE_RATE} Hz, got {}",
            spec.sample_rate
        )));
    }
    let interleaved: Vec<f32> = match (spec.sample_format, spec.bits_per_sample) {
        (hound::SampleFormat::Int, 16) => reader
            .samples::<i16>()
            .map(|s| s.map(|v| v as f32 / 32768.0))
            .collect::<Result<_, _>>()
            .map_err(|e| InferenceError::Audio(format!("read samples: {e}")))?,
        (hound::SampleFormat::Float, 32) => reader
            .samples::<f32>()
            .collect::<Result<_, _>>()
            .map_err(|e| InferenceError::Audio(format!("read samples: {e}")))?,
        (_, bits) => {
            return Err(InferenceError::Audio(format!(
                "unsupported wav sample format ({bits}-bit)"
            )))
        }
    };
    let channels = spec.channels as usize;
    if channels <= 1 {
        return Ok(interleaved);
    }
    // Right channel (index 1) of each frame.
    Ok(interleaved.chunks(channels).map(|frame| frame[1]).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downmixes_stereo_to_mono() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("s.wav");
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        {
            let mut w = hound::WavWriter::create(&path, spec).unwrap();
            // Frame of L=+0.5, R=-0.5 -> mono average 0.0.
            w.write_sample(16384i16).unwrap();
            w.write_sample(-16384i16).unwrap();
            w.finalize().unwrap();
        }
        let mono = read_wav_mono_16k(&path).unwrap();
        assert_eq!(mono.len(), 1);
        assert!(mono[0].abs() < 1e-4);
    }

    #[test]
    fn rejects_wrong_sample_rate() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("s.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 44_100,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        hound::WavWriter::create(&path, spec)
            .unwrap()
            .finalize()
            .unwrap();
        assert!(matches!(
            read_wav_mono_16k(&path),
            Err(InferenceError::Audio(_))
        ));
    }
}

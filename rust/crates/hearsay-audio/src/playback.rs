//! Exact-seeking playback of an archived FLAC, served as a 16-bit WAV byte stream decoded on demand.
//! WebKit seeks FLAC by estimating byte offsets, which drifts on unevenly compressible audio.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::ops::RangeInclusive;
use std::path::Path;

use claxon::frame::FrameReader;
use claxon::input::{BufferedReader, ReadBytes};

use crate::error::AudioError;
use crate::{CHANNELS, SAMPLE_RATE};

const WAV_HEADER_LEN: u64 = 44;
const BYTES_PER_SAMPLE_FRAME: u64 = CHANNELS as u64 * 2;

/// Where each FLAC frame starts, so a WAV byte range maps to the frames that hold it.
#[derive(Debug, Clone, PartialEq)]
pub struct FlacIndex {
    /// `(byte offset, first sample)` per frame, ascending.
    frames: Vec<(u64, u64)>,
    total_samples: u64,
}

/// A byte reader that tracks its position, so each frame's offset is known before it is decoded.
struct CountingReader<R: Read> {
    inner: BufReader<R>,
    pos: u64,
}

impl<R: Read> CountingReader<R> {
    fn new(inner: R, pos: u64) -> Self {
        CountingReader {
            inner: BufReader::with_capacity(64 * 1024, inner),
            pos,
        }
    }
}

impl<R: Read> ReadBytes for CountingReader<R> {
    fn read_u8(&mut self) -> io::Result<u8> {
        self.read_u8_or_eof()?
            .ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))
    }

    fn read_u8_or_eof(&mut self) -> io::Result<Option<u8>> {
        let Some(&byte) = self.inner.fill_buf()?.first() else {
            return Ok(None);
        };
        self.inner.consume(1);
        self.pos += 1;
        Ok(Some(byte))
    }

    fn read_into(&mut self, buffer: &mut [u8]) -> io::Result<()> {
        self.inner.read_exact(buffer)?;
        self.pos += buffer.len() as u64;
        Ok(())
    }

    fn skip(&mut self, amount: u32) -> io::Result<()> {
        let skipped = io::copy(
            &mut (&mut self.inner).take(u64::from(amount)),
            &mut io::sink(),
        )?;
        if skipped < u64::from(amount) {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        self.pos += skipped;
        Ok(())
    }
}

fn flac_err(e: claxon::Error) -> AudioError {
    AudioError::Flac(e.to_string())
}

/// Byte offset of the first audio frame: past `fLaC` and every metadata block.
fn first_frame_offset(file: &mut File) -> Result<u64, AudioError> {
    let mut marker = [0u8; 4];
    file.read_exact(&mut marker)?;
    if &marker != b"fLaC" {
        return Err(AudioError::Flac("missing fLaC marker".into()));
    }
    let mut offset = 4u64;
    loop {
        let mut header = [0u8; 4];
        file.read_exact(&mut header)?;
        let len = u64::from(u32::from_be_bytes([0, header[1], header[2], header[3]]));
        offset += 4 + len;
        if header[0] & 0x80 != 0 {
            return Ok(offset);
        }
        file.seek(SeekFrom::Start(offset))?;
    }
}

impl FlacIndex {
    /// Index a recording by decoding it once; frames are CRC-checked as they are read.
    pub fn build(path: &Path) -> Result<FlacIndex, AudioError> {
        let info = claxon::FlacReader::open(path)
            .map_err(flac_err)?
            .streaminfo();
        if info.channels as usize != CHANNELS
            || info.sample_rate != SAMPLE_RATE
            || info.bits_per_sample != 16
        {
            return Err(AudioError::Unsupported(format!(
                "{} ch, {} Hz, {}-bit (expected {CHANNELS} ch, {SAMPLE_RATE} Hz, 16-bit)",
                info.channels, info.sample_rate, info.bits_per_sample
            )));
        }
        let mut file = File::open(path)?;
        let start = first_frame_offset(&mut file)?;
        file.seek(SeekFrom::Start(start))?;
        let mut input = CountingReader::new(file, start);
        let mut frames = Vec::new();
        let mut sample = 0u64;
        let mut buffer = Vec::new();
        loop {
            let offset = input.pos;
            let Some(block) = FrameReader::new(&mut input)
                .read_next_or_eof(buffer)
                .map_err(flac_err)?
            else {
                break;
            };
            frames.push((offset, sample));
            sample += u64::from(block.duration());
            buffer = block.into_buffer();
        }
        Ok(FlacIndex {
            frames,
            total_samples: sample,
        })
    }

    /// Length of the WAV this recording plays as.
    pub fn wav_len(&self) -> u64 {
        WAV_HEADER_LEN + self.total_samples * BYTES_PER_SAMPLE_FRAME
    }

    fn wav_header(&self) -> [u8; WAV_HEADER_LEN as usize] {
        let data_len = (self.total_samples * BYTES_PER_SAMPLE_FRAME) as u32;
        let byte_rate = SAMPLE_RATE * BYTES_PER_SAMPLE_FRAME as u32;
        let mut h = [0u8; WAV_HEADER_LEN as usize];
        h[0..4].copy_from_slice(b"RIFF");
        h[4..8].copy_from_slice(&(data_len + 36).to_le_bytes());
        h[8..16].copy_from_slice(b"WAVEfmt ");
        h[16..20].copy_from_slice(&16u32.to_le_bytes());
        h[20..22].copy_from_slice(&1u16.to_le_bytes());
        h[22..24].copy_from_slice(&(CHANNELS as u16).to_le_bytes());
        h[24..28].copy_from_slice(&SAMPLE_RATE.to_le_bytes());
        h[28..32].copy_from_slice(&byte_rate.to_le_bytes());
        h[32..34].copy_from_slice(&(BYTES_PER_SAMPLE_FRAME as u16).to_le_bytes());
        h[34..36].copy_from_slice(&16u16.to_le_bytes());
        h[36..40].copy_from_slice(b"data");
        h[40..44].copy_from_slice(&data_len.to_le_bytes());
        h
    }

    /// Write WAV bytes `range` (clamped to [`Self::wav_len`]) to `sink`, decoding only the frames
    /// that hold it. `path` must be the file this index was built from.
    pub fn read_wav_range<F>(
        &self,
        path: &Path,
        range: RangeInclusive<u64>,
        mut sink: F,
    ) -> Result<(), AudioError>
    where
        F: FnMut(&[u8]) -> Result<(), AudioError>,
    {
        let start = *range.start();
        let end = (*range.end()).min(self.wav_len().saturating_sub(1));
        if start > end {
            return Ok(());
        }
        if start < WAV_HEADER_LEN {
            let header = self.wav_header();
            sink(&header[start as usize..=end.min(WAV_HEADER_LEN - 1) as usize])?;
        }
        if end < WAV_HEADER_LEN {
            return Ok(());
        }
        let data_start = start.max(WAV_HEADER_LEN) - WAV_HEADER_LEN;
        let data_end = end - WAV_HEADER_LEN;
        let first_sample = data_start / BYTES_PER_SAMPLE_FRAME;
        let index = self.frames.partition_point(|&(_, s)| s <= first_sample) - 1;
        let (offset, mut sample) = self.frames[index];

        let mut file = File::open(path)?;
        file.seek(SeekFrom::Start(offset))?;
        let mut reader = FrameReader::new(BufferedReader::new(file));
        let mut buffer = Vec::new();
        let mut bytes = Vec::new();
        while sample * BYTES_PER_SAMPLE_FRAME <= data_end {
            let block = reader
                .read_next_or_eof(buffer)
                .map_err(flac_err)?
                .ok_or_else(|| AudioError::Flac("recording ended before the index".into()))?;
            bytes.clear();
            for (left, right) in block.stereo_samples() {
                bytes.extend_from_slice(&(left as i16).to_le_bytes());
                bytes.extend_from_slice(&(right as i16).to_le_bytes());
            }
            let block_start = sample * BYTES_PER_SAMPLE_FRAME;
            let lo = data_start.max(block_start) - block_start;
            let hi = (data_end - block_start).min(bytes.len() as u64 - 1);
            sink(&bytes[lo as usize..=hi as usize])?;
            sample += u64::from(block.duration());
            buffer = block.into_buffer();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encode_wav_to_flac;
    use crate::test_support::{interleaved_pattern, stereo_spec, write_wav};

    /// Long digital silence then dense noise, so bytes and time diverge as they do in a real
    /// meeting with a muted mic; not a whole number of 4096-sample blocks, so the last is short.
    fn uneven_recording(dir: &Path) -> (std::path::PathBuf, Vec<i16>) {
        let mut samples = vec![0i16; 40_000 * CHANNELS];
        samples.extend(interleaved_pattern(30_001));
        samples.extend(vec![0i16; 20_000 * CHANNELS]);
        samples.extend(interleaved_pattern(7_777));
        let wav = dir.join("audio.wav");
        let flac = dir.join("audio.flac");
        write_wav(&wav, stereo_spec(), &samples);
        encode_wav_to_flac(&wav, &flac).expect("encode");
        (flac, samples)
    }

    fn expected_wav(index: &FlacIndex, samples: &[i16]) -> Vec<u8> {
        let mut out = index.wav_header().to_vec();
        for s in samples {
            out.extend_from_slice(&s.to_le_bytes());
        }
        out
    }

    fn read(index: &FlacIndex, path: &Path, range: RangeInclusive<u64>) -> Vec<u8> {
        let mut out = Vec::new();
        index
            .read_wav_range(path, range, |chunk| {
                out.extend_from_slice(chunk);
                Ok(())
            })
            .expect("read range");
        out
    }

    #[test]
    fn the_full_range_is_a_valid_wav_of_the_original_samples() {
        let dir = tempfile::tempdir().unwrap();
        let (flac, samples) = uneven_recording(dir.path());
        let index = FlacIndex::build(&flac).unwrap();
        let full = read(&index, &flac, 0..=u64::MAX);

        assert_eq!(full.len() as u64, index.wav_len());
        assert_eq!(full, expected_wav(&index, &samples));
        let decoded: Vec<i16> = hound::WavReader::new(io::Cursor::new(full))
            .unwrap()
            .samples::<i16>()
            .map(Result::unwrap)
            .collect();
        assert_eq!(decoded, samples);
    }

    #[test]
    fn every_range_matches_the_same_bytes_of_the_full_wav() {
        let dir = tempfile::tempdir().unwrap();
        let (flac, samples) = uneven_recording(dir.path());
        let index = FlacIndex::build(&flac).unwrap();
        let full = expected_wav(&index, &samples);
        let len = full.len() as u64;
        let block = 4096 * BYTES_PER_SAMPLE_FRAME;
        let ranges = [
            0..=0,
            10..=43,
            40..=50,
            44..=44,
            44 + block - 3..=44 + block + 3,
            44 + 5 * block..=44 + 9 * block - 1,
            123_457..=400_001,
            len - 5..=len - 1,
            len - 5..=len + 1_000,
        ];
        for range in ranges {
            let got = read(&index, &flac, range.clone());
            let end = (*range.end()).min(len - 1);
            assert_eq!(
                got,
                full[*range.start() as usize..=end as usize],
                "range {range:?}"
            );
        }
        assert!(read(&index, &flac, len..=len + 10).is_empty());
    }

    #[test]
    fn the_index_starts_each_frame_where_the_previous_one_ends() {
        let dir = tempfile::tempdir().unwrap();
        let (flac, samples) = uneven_recording(dir.path());
        let index = FlacIndex::build(&flac).unwrap();

        assert_eq!(index.total_samples, (samples.len() / CHANNELS) as u64);
        assert_eq!(index.frames[0].1, 0);
        assert!(index
            .frames
            .windows(2)
            .all(|w| w[1].0 > w[0].0 && w[1].1 == w[0].1 + 4096));
    }
}

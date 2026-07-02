//! Cross-platform offline speaker diarization via sherpa-onnx: pyannote segmentation-3.0 +
//! speaker-embedding + clustering, all ONNX. The non-Swift (Windows) alternative to the
//! `hearsay-diarize` FluidAudio sidecar — it produces the same shape (speaker turns + a per-speaker
//! voiceprint), so it can feed the offline refine on any platform.
//!
//! The diarizer's result exposes only `(start, end, speaker)` segments, so each speaker's voiceprint
//! is computed separately with the same embedding model (the mean of that speaker's audio), matching
//! what the FluidAudio path emits.

use std::collections::HashMap;
use std::path::Path;

use sherpa_onnx::{
    FastClusteringConfig, OfflineSpeakerDiarization, OfflineSpeakerDiarizationConfig,
    OfflineSpeakerSegmentationModelConfig, OfflineSpeakerSegmentationPyannoteModelConfig,
    SpeakerEmbeddingExtractor, SpeakerEmbeddingExtractorConfig,
};

use crate::error::InferenceError;

/// Contract-fixed track sample rate (Hz).
const SAMPLE_RATE: i32 = 16_000;

/// Default agglomerative-clustering cosine threshold (sherpa's own default). NOTE: not yet
/// competitive — on real call audio this pipeline over-clusters (see the `sweep_cluster_threshold`
/// opt-in test: best case ~3 speakers on a known-2 clip with TitaNet, vs FluidAudio's 2). Higher
/// merges more (fewer speakers). DER-tuning (segmentation/embedding model + threshold, validated
/// across labeled clips) is pending before this feeds the refine on non-Mac platforms.
const DEFAULT_CLUSTER_THRESHOLD: f32 = 0.5;

/// One diarizer turn: a 1-based speaker ordinal over `[start_s, end_s)` (meeting time).
#[derive(Debug, Clone, PartialEq)]
pub struct DiarTurn {
    pub speaker: i64,
    pub start_s: f64,
    pub end_s: f64,
}

/// A diarization result: speaker turns + each speaker's voiceprint by ordinal (mean of its
/// segments' audio). Mirrors the Swift `hearsay-diarize` sidecar's `turns` + `speakers` output.
#[derive(Debug, Clone, Default)]
pub struct SherpaDiarization {
    pub turns: Vec<DiarTurn>,
    pub embeddings: HashMap<i64, Vec<f32>>,
}

/// An offline speaker diarizer + speaker embedder, both ONNX (loaded once, reused per meeting).
pub struct SherpaDiarizer {
    diarizer: OfflineSpeakerDiarization,
    embedder: SpeakerEmbeddingExtractor,
}

impl SherpaDiarizer {
    /// Load the pyannote segmentation model + the speaker-embedding model (both `.onnx`), with the
    /// default (tuned) clustering threshold.
    pub fn load(segmentation_model: &Path, embedding_model: &Path) -> Result<Self, InferenceError> {
        Self::load_with_threshold(
            segmentation_model,
            embedding_model,
            DEFAULT_CLUSTER_THRESHOLD,
        )
    }

    /// Load with an explicit clustering cosine threshold (higher merges more -> fewer speakers).
    /// CPU provider by default (portable); accel is a sherpa build concern, not wired here yet.
    pub fn load_with_threshold(
        segmentation_model: &Path,
        embedding_model: &Path,
        cluster_threshold: f32,
    ) -> Result<Self, InferenceError> {
        let embedding = SpeakerEmbeddingExtractorConfig {
            model: Some(embedding_model.to_string_lossy().into_owned()),
            ..Default::default()
        };
        let config = OfflineSpeakerDiarizationConfig {
            segmentation: OfflineSpeakerSegmentationModelConfig {
                pyannote: OfflineSpeakerSegmentationPyannoteModelConfig {
                    model: Some(segmentation_model.to_string_lossy().into_owned()),
                },
                ..Default::default()
            },
            embedding: embedding.clone(),
            clustering: FastClusteringConfig {
                num_clusters: -1,
                threshold: cluster_threshold,
            },
            ..Default::default()
        };
        let diarizer = OfflineSpeakerDiarization::create(&config).ok_or_else(|| {
            InferenceError::Diarize(format!(
                "failed to create diarizer (segmentation model {})",
                segmentation_model.display()
            ))
        })?;
        let embedder = SpeakerEmbeddingExtractor::create(&embedding).ok_or_else(|| {
            InferenceError::Diarize(format!(
                "failed to create speaker embedder (model {})",
                embedding_model.display()
            ))
        })?;
        Ok(Self { diarizer, embedder })
    }

    /// Diarize a 16 kHz mono track: speaker turns (1-based ordinal by first appearance) + a
    /// per-speaker mean voiceprint.
    pub fn diarize(&self, samples: &[f32]) -> Result<SherpaDiarization, InferenceError> {
        let result = self
            .diarizer
            .process(samples)
            .ok_or_else(|| InferenceError::Diarize("diarization produced no result".into()))?;
        let segments = result.sort_by_start_time();

        // sherpa speaker index (0-based, arbitrary) -> our 1-based ordinal by first appearance
        // (segments are start-sorted, so this matches `order_speakers`).
        let mut ordinal: HashMap<i32, i64> = HashMap::new();
        let mut turns = Vec::with_capacity(segments.len());
        let mut speaker_audio: HashMap<i64, Vec<f32>> = HashMap::new();
        for seg in &segments {
            let next = ordinal.len() as i64 + 1;
            let ord = *ordinal.entry(seg.speaker).or_insert(next);
            turns.push(DiarTurn {
                speaker: ord,
                start_s: seg.start as f64,
                end_s: seg.end as f64,
            });
            let start = ((seg.start as f64) * SAMPLE_RATE as f64).max(0.0) as usize;
            let end = ((seg.end as f64) * SAMPLE_RATE as f64) as usize;
            let end = end.min(samples.len());
            if end > start {
                speaker_audio
                    .entry(ord)
                    .or_default()
                    .extend_from_slice(&samples[start..end]);
            }
        }

        let mut embeddings = HashMap::new();
        for (ord, audio) in &speaker_audio {
            if let Some(embedding) = self.embed(audio)? {
                embeddings.insert(*ord, embedding);
            }
        }

        Ok(SherpaDiarization { turns, embeddings })
    }

    /// Embed a mono 16 kHz slice into a single speaker vector (`None` if too short to embed).
    fn embed(&self, samples: &[f32]) -> Result<Option<Vec<f32>>, InferenceError> {
        let stream = self
            .embedder
            .create_stream()
            .ok_or_else(|| InferenceError::Diarize("failed to create embedding stream".into()))?;
        stream.accept_waveform(SAMPLE_RATE, samples);
        stream.input_finished();
        if !self.embedder.is_ready(&stream) {
            return Ok(None);
        }
        Ok(self.embedder.compute(&stream))
    }
}

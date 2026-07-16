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

use hearsay_attribution::{order_speakers, SpeakerTurn};
use sherpa_onnx::{
    FastClusteringConfig, OfflineSpeakerDiarization, OfflineSpeakerDiarizationConfig,
    OfflineSpeakerSegmentationModelConfig, OfflineSpeakerSegmentationPyannoteModelConfig,
    SpeakerEmbeddingExtractor, SpeakerEmbeddingExtractorConfig,
};

use crate::diarizer::{DiarTurn, Diarization, Diarizer};
use crate::error::InferenceError;

/// Contract-fixed track sample rate (Hz).
const SAMPLE_RATE: i32 = 16_000;

/// Default agglomerative-clustering cosine threshold — the best-achievable operating point, not a
/// competitive one. DER-tuning was exhausted under the MIT/BSD/Apache license gate (see the
/// `sweep_cluster_threshold` opt-in test): with pyannote-segmentation-3.0 (the only permissive
/// sherpa segmentation model — the better Rev "reverb" models are Non-Production/non-commercial),
/// this pipeline plateaus at ~3 speakers on a known-2 clip (0.90/0.95/0.97 all give 3; below ~0.85
/// it over-clusters badly) vs FluidAudio's clean 2. TitaNet beats CAM++ as the embedder. So on
/// macOS the Swift/FluidAudio `hearsay-diarize` stays the accuracy tier; this is the cross-platform
/// (Windows) fallback, degraded-but-usable. Higher merges more (fewer speakers).
const DEFAULT_CLUSTER_THRESHOLD: f32 = 0.9;

/// Clustering + segmentation-gating knobs for [`SherpaDiarizer`] (the DER-tuning surface).
#[derive(Clone, Copy, Debug)]
pub struct DiarizeTuning {
    /// Agglomerative cosine threshold (higher merges more -> fewer speakers).
    pub cluster_threshold: f32,
    /// Drop speech turns shorter than this (seconds); trims short spurious turns before clustering.
    pub min_duration_on: f32,
    /// Bridge silence gaps shorter than this (seconds) within a speaker.
    pub min_duration_off: f32,
}

impl Default for DiarizeTuning {
    fn default() -> Self {
        Self {
            cluster_threshold: DEFAULT_CLUSTER_THRESHOLD,
            min_duration_on: 0.3,
            min_duration_off: 0.5,
        }
    }
}

/// An offline speaker diarizer + speaker embedder, both ONNX (loaded once, reused per meeting).
pub struct SherpaDiarizer {
    diarizer: OfflineSpeakerDiarization,
    embedder: SpeakerEmbeddingExtractor,
}

impl SherpaDiarizer {
    /// Load the pyannote segmentation model + the speaker-embedding model (both `.onnx`), with the
    /// default tuning.
    pub fn load(segmentation_model: &Path, embedding_model: &Path) -> Result<Self, InferenceError> {
        Self::load_tuned(
            segmentation_model,
            embedding_model,
            DiarizeTuning::default(),
        )
    }

    /// Load with explicit [`DiarizeTuning`]. CPU provider by default (portable); accel is a sherpa
    /// build concern, not wired here yet.
    pub fn load_tuned(
        segmentation_model: &Path,
        embedding_model: &Path,
        tuning: DiarizeTuning,
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
                threshold: tuning.cluster_threshold,
            },
            min_duration_on: tuning.min_duration_on,
            min_duration_off: tuning.min_duration_off,
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

impl Diarizer for SherpaDiarizer {
    /// Diarize a 16 kHz mono track: speaker turns (1-based ordinal by first appearance) + a
    /// per-speaker mean voiceprint.
    fn diarize(&self, them_samples: &[f32]) -> Result<Diarization, InferenceError> {
        let result = self
            .diarizer
            .process(them_samples)
            .ok_or_else(|| InferenceError::Diarize("diarization produced no result".into()))?;
        let segments = result.sort_by_start_time();

        // sherpa speaker index (0-based, arbitrary) -> our 1-based ordinal by first appearance via
        // the canonical `order_speakers` (segments are already start-sorted).
        let ordering: Vec<SpeakerTurn> = segments
            .iter()
            .map(|seg| SpeakerTurn {
                speaker: seg.speaker.to_string(),
                start_s: seg.start as f64,
                end_s: seg.end as f64,
            })
            .collect();
        let ordinals = order_speakers(&ordering);

        let mut turns = Vec::with_capacity(segments.len());
        let mut speaker_audio: HashMap<i64, Vec<f32>> = HashMap::new();
        for seg in &segments {
            let ord = i64::from(ordinals[&seg.speaker.to_string()]);
            turns.push(DiarTurn {
                speaker: ord,
                start_s: seg.start as f64,
                end_s: seg.end as f64,
            });
            let start = ((seg.start as f64) * SAMPLE_RATE as f64).max(0.0) as usize;
            let end = ((seg.end as f64) * SAMPLE_RATE as f64) as usize;
            let end = end.min(them_samples.len());
            if end > start {
                speaker_audio
                    .entry(ord)
                    .or_default()
                    .extend_from_slice(&them_samples[start..end]);
            }
        }

        let mut embeddings = HashMap::new();
        for (ord, audio) in &speaker_audio {
            if let Some(embedding) = self.embed(audio)? {
                embeddings.insert(*ord, embedding);
            }
        }

        Ok(Diarization { turns, embeddings })
    }
}

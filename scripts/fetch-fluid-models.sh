#!/usr/bin/env bash
# Download the bundled FluidAudio models from HuggingFace, for a machine that has never run the app.
# Entries are <local folder>|<HF repo>|<repo paths>, mirroring FluidAudio's own cache layout.
set -euo pipefail

DEST="${1:-outputs/models/fluidaudio/Models}"
API="https://huggingface.co/api/models"
RESOLVE="https://huggingface.co"

MANIFEST=(
  "parakeet-tdt-0.6b-v3|FluidInference/parakeet-tdt-0.6b-v3-coreml|Decoder.mlmodelc Encoder.mlmodelc JointDecisionv3.mlmodelc Preprocessor.mlmodelc config.json parakeet_v3_vocab.json parakeet_vocab.json"
  "parakeet-unified-en-0.6b|FluidInference/parakeet-unified-en-0.6b-coreml|parakeet_unified_decoder.mlmodelc parakeet_unified_encoder_streaming_70_13_13_int8.mlmodelc parakeet_unified_joint_decision_single_step.mlmodelc parakeet_unified_preprocessor.mlmodelc config.json metadata.json vocab.json"
  "speaker-diarization|FluidInference/speaker-diarization-coreml|Embedding.mlmodelc FBank.mlmodelc PldaRho.mlmodelc Segmentation.mlmodelc pyannote_segmentation.mlmodelc wespeaker_v2.mlmodelc config.json plda-parameters.json xvector-transform.json"
  "silero-vad|FluidInference/silero-vad-coreml|silero-vad-unified-256ms-v6.0.0.mlmodelc config.json"
  "ls-eend/ami|FluidInference/ls-eend-coreml|optimized/ami/500ms"
)

list_files() {
  local repo="$1" path="$2"
  curl -fsS --retry 3 "$API/$repo/tree/main/$path?recursive=true" \
    | jq -r '.[] | select(.type == "file") | .path | select(contains(".mlpackage/") | not)'
}

for entry in "${MANIFEST[@]}"; do
  folder="${entry%%|*}"
  rest="${entry#*|}"
  repo="${rest%%|*}"
  paths="${rest#*|}"

  for path in $paths; do
    if [[ "$path" == *.json ]]; then
      files="$path"
    else
      files="$(list_files "$repo" "$path")"
    fi
    for f in $files; do
      out="$DEST/$folder/$f"
      [ -f "$out" ] && continue
      mkdir -p "$(dirname "$out")"
      echo "  $folder/$f"
      curl -fsSL --retry 3 -C - -o "$out.part" "$RESOLVE/$repo/resolve/main/$f"
      mv "$out.part" "$out"
    done
  done
  echo "fetched $folder"
done

from __future__ import annotations

from pathlib import Path

from hearsay.asr import KNOWN_MODELS, available_models, resolve_model
from hearsay.config.settings import Settings
from hearsay.enums import ASRBackendKind


def test_resolve_known_model_per_backend() -> None:
    assert resolve_model(ASRBackendKind.WHISPERCPP, "large-v3-turbo") == "large-v3-turbo"
    assert (
        resolve_model(ASRBackendKind.MLX, "large-v3-turbo")
        == "mlx-community/whisper-large-v3-turbo"
    )


def test_resolve_unknown_passes_through() -> None:
    assert resolve_model(ASRBackendKind.WHISPERCPP, "/models/custom.bin") == "/models/custom.bin"
    assert resolve_model(ASRBackendKind.MLX, "some-org/some-repo") == "some-org/some-repo"


def test_default_model_is_curated() -> None:
    assert any(model.name == "large-v3-turbo" for model in KNOWN_MODELS)


def test_available_models_lists_curated_and_installed(tmp_path: Path) -> None:
    models_dir = tmp_path / "models"
    models_dir.mkdir()
    (models_dir / "ggml-tiny.bin").write_bytes(b"stub")
    settings = Settings(app_support_dir=tmp_path, models_dir=models_dir)

    infos = available_models(settings)
    names = [info.name for info in infos]
    assert "large-v3-turbo" in names
    installed = [info for info in infos if info.installed]
    assert len(installed) == 1
    assert installed[0].name.endswith("ggml-tiny.bin")

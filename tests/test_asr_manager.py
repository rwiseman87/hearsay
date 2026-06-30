from __future__ import annotations

from hearsay.asr import ParakeetBackend, asr_helper_path, available_models, build_asr
from hearsay.config.settings import Settings


def test_build_asr_is_parakeet() -> None:
    backend = build_asr(Settings())
    assert isinstance(backend, ParakeetBackend)


def test_asr_helper_path_is_sibling_of_capture_helper() -> None:
    settings = Settings()
    assert asr_helper_path(settings) == settings.helper_path.with_name("hearsay-asr")


def test_available_models_lists_parakeet() -> None:
    infos = available_models(Settings())
    assert [info.name for info in infos] == ["parakeet-tdt-v3"]
    assert infos[0].installed

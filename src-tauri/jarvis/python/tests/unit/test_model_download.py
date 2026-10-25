from __future__ import annotations

import json
import traceback
from pathlib import Path
from typing import Any

import pytest

from jarvis_backend import model_download
from jarvis_backend.model_download import download_models, endpoint_candidates


def test_endpoint_candidates_normalize_and_deduplicate() -> None:
    assert endpoint_candidates("https://huggingface.co/", "https://huggingface.co") == (
        "https://huggingface.co",
    )


def test_model_download_uses_official_source_first(tmp_path: Path) -> None:
    calls: list[dict[str, Any]] = []

    def snapshot_download(**kwargs: Any) -> None:
        calls.append(kwargs)

    endpoint = download_models(
        tmp_path,
        "revision",
        snapshot_download=snapshot_download,
        log=lambda _message: None,
    )

    assert endpoint == "https://huggingface.co"
    assert [call["endpoint"] for call in calls] == ["https://huggingface.co"]
    assert calls[0]["revision"] == "revision"
    assert calls[0]["local_dir"] == str(tmp_path)


def test_model_download_retries_with_mirror(tmp_path: Path) -> None:
    endpoints: list[str] = []

    def snapshot_download(**kwargs: Any) -> None:
        endpoints.append(kwargs["endpoint"])
        if len(endpoints) == 1:
            raise TimeoutError("official source timed out")

    endpoint = download_models(
        tmp_path,
        "revision",
        mirror_endpoint="https://mirror.example/",
        snapshot_download=snapshot_download,
        log=lambda _message: None,
    )

    assert endpoint == "https://mirror.example"
    assert endpoints == ["https://huggingface.co", "https://mirror.example"]


def test_model_download_reports_aggregate_byte_progress(tmp_path: Path) -> None:
    progress: list[tuple[int, int]] = []

    def snapshot_download(**kwargs: Any) -> None:
        progress_bar = kwargs["tqdm_class"](
            desc="Reconstructing model files",
            total=model_download.MODEL_TOTAL_BYTES,
            unit="B",
        )
        progress_bar.update(model_download.MODEL_TOTAL_BYTES // 2)
        progress_bar.close()

    download_models(
        tmp_path,
        "revision",
        snapshot_download=snapshot_download,
        log=lambda _message: None,
        progress=lambda completed, total: progress.append((completed, total)),
    )

    assert progress[0] == (0, model_download.MODEL_TOTAL_BYTES)
    assert progress[1] == (
        model_download.MODEL_TOTAL_BYTES // 2,
        model_download.MODEL_TOTAL_BYTES,
    )
    assert progress[-1] == (
        model_download.MODEL_TOTAL_BYTES,
        model_download.MODEL_TOTAL_BYTES,
    )


def test_model_download_redacts_token_from_final_error(tmp_path: Path) -> None:
    token = "private-token"

    def snapshot_download(**_kwargs: Any) -> None:
        raise RuntimeError(f"request rejected for {token}")

    with pytest.raises(RuntimeError) as error:
        download_models(
            tmp_path,
            "revision",
            token=token,
            mirror_endpoint=None,
            snapshot_download=snapshot_download,
            log=lambda _message: None,
        )

    assert token not in str(error.value)
    assert "[redacted]" in str(error.value)
    rendered = "".join(traceback.format_exception(error.value))
    assert token not in rendered


def test_model_validation_and_marker_use_the_pinned_manifest(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    payload = b"portable-model"
    digest = __import__("hashlib").sha256(payload).hexdigest()
    monkeypatch.setattr(
        model_download,
        "MODEL_FILES",
        (("vision/model.gguf", len(payload), digest),),
    )
    model_path = tmp_path / "vision" / "model.gguf"
    model_path.parent.mkdir(parents=True)
    model_path.write_bytes(payload)

    assert model_download.model_files_are_valid(tmp_path)
    assert model_download.model_files_are_valid(tmp_path, verify_hashes=True)
    assert not model_download.model_marker_is_valid(tmp_path)

    model_download.write_model_marker(tmp_path)

    assert model_download.model_marker_is_valid(tmp_path)
    marker = json.loads((tmp_path / model_download.MODEL_MARKER).read_text(encoding="utf-8"))
    assert marker["revision"] == model_download.MODEL_REVISION
    assert marker["files"] == {"vision/model.gguf": digest}

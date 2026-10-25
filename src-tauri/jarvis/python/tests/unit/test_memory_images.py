import base64
import io
import json
import urllib.error
import urllib.request
from datetime import date

import pytest
from fastapi.testclient import TestClient

import jarvis_backend.memory.image_generation as image_module
from jarvis_backend.app import create_app
from jarvis_backend.memory import ImageGenerationClient, ImageProvider, MemoryStore
from jarvis_backend.settings import CourseSettings, MemorySettings, Settings

PNG = b"\x89PNG\r\n\x1a\nimage"
JPEG = b"\xff\xd8\xffimage"
WEBP = b"RIFF\x08\x00\x00\x00WEBPimage"


class FakeResponse:
    def __init__(self, content: bytes) -> None:
        self.stream = io.BytesIO(content)

    def read(self, size: int = -1) -> bytes:
        return self.stream.read(size)

    def __enter__(self):
        return self

    def __exit__(self, *_args) -> None:
        return None


def references(tmp_path):
    character = tmp_path / "character.png"
    style = tmp_path / "style.jpg"
    character.write_bytes(PNG)
    style.write_bytes(JPEG)
    return [character, style]


def provider(api_key: str = "secret-key") -> ImageProvider:
    return ImageProvider("https://images.example/v1", api_key, "image-model")


def test_image_client_posts_ordered_references_and_decodes_base64(
    tmp_path, monkeypatch
):
    captured = {}
    generated = PNG + b"generated"

    def urlopen(request, timeout):
        captured.update(request=request, timeout=timeout)
        payload = {"data": [{"b64_json": base64.b64encode(generated).decode()}]}
        return FakeResponse(json.dumps(payload).encode())

    monkeypatch.setattr(urllib.request, "urlopen", urlopen)
    result = ImageGenerationClient._generate_sync(
        provider(), "日期 2026-07-17，形象见图片1，参考图片2的风格", references(tmp_path), 12
    )

    request = captured["request"]
    body = request.data
    assert result == generated
    assert captured["timeout"] == 12
    assert request.full_url == "https://images.example/v1/images/edits"
    assert request.get_header("Authorization") == "Bearer secret-key"
    assert body.count(b'name="image[]"') == 2
    assert body.index(b'filename="character.png"') < body.index(b'filename="style.jpg"')
    assert "日期 2026-07-17，形象见图片1，参考图片2的风格".encode() in body
    assert b"1536x1024" in body
    assert b"quality" in body and b"high" in body


def test_image_client_downloads_url_result(tmp_path, monkeypatch):
    calls = []

    def urlopen(request, timeout):
        calls.append((request, timeout))
        if isinstance(request, urllib.request.Request):
            return FakeResponse(
                json.dumps({"data": [{"url": "https://cdn.example/result.webp"}]}).encode()
            )
        return FakeResponse(WEBP)

    monkeypatch.setattr(urllib.request, "urlopen", urlopen)

    assert ImageGenerationClient._generate_sync(
        provider(), "prompt", references(tmp_path), 30
    ) == WEBP
    assert calls[1] == ("https://cdn.example/result.webp", 30)


def test_image_client_rejects_unsafe_url_and_invalid_image(tmp_path, monkeypatch):
    def unsafe_urlopen(_request, timeout):
        del timeout
        return FakeResponse(json.dumps({"data": [{"url": "file:///tmp/result.png"}]}).encode())

    monkeypatch.setattr(urllib.request, "urlopen", unsafe_urlopen)
    with pytest.raises(RuntimeError, match="unsupported image URL"):
        ImageGenerationClient._generate_sync(
            provider(), "prompt", references(tmp_path), 30
        )

    invalid = {"data": [{"b64_json": base64.b64encode(b"not an image").decode()}]}
    monkeypatch.setattr(
        urllib.request,
        "urlopen",
        lambda _request, timeout: FakeResponse(json.dumps(invalid).encode()),
    )
    with pytest.raises(RuntimeError, match="unsupported image format"):
        ImageGenerationClient._generate_sync(
            provider(), "prompt", references(tmp_path), 30
        )


def test_image_client_redacts_http_errors_and_limits_base64(
    tmp_path, monkeypatch
):
    secret = "provider-secret"

    def failed(request, timeout):
        del timeout
        payload = json.dumps({"error": {"message": f"invalid key {secret}"}}).encode()
        raise urllib.error.HTTPError(request.full_url, 401, "Unauthorized", {}, io.BytesIO(payload))

    monkeypatch.setattr(urllib.request, "urlopen", failed)
    with pytest.raises(RuntimeError) as error:
        ImageGenerationClient._generate_sync(
            provider(secret), "prompt", references(tmp_path), 30
        )
    assert secret not in str(error.value)
    assert "[redacted]" in str(error.value)

    monkeypatch.setattr(image_module, "MAX_IMAGE_BYTES", 16)
    oversized = {"data": [{"b64_json": base64.b64encode(PNG + b"x" * 16).decode()}]}
    monkeypatch.setattr(
        urllib.request,
        "urlopen",
        lambda _request, timeout: FakeResponse(json.dumps(oversized).encode()),
    )
    with pytest.raises(RuntimeError, match="exceeds 25 MB"):
        ImageGenerationClient._generate_sync(
            provider(), "prompt", references(tmp_path), 30
        )


async def test_daily_image_generates_missing_summary_and_preserves_history(
    tmp_path, monkeypatch
):
    settings = Settings(
        memory=MemorySettings(root=tmp_path / "memory"),
        courses=CourseSettings(
            sessions_root=tmp_path / "sessions", output_root=tmp_path / "courses"
        ),
    )
    orchestrator = create_app(settings=settings).state.orchestrator
    requested_day = date(2026, 7, 17)
    summary_calls = []
    generation_calls = []

    async def generate_summary(day_value):
        summary_calls.append(day_value)
        content = (
            "# 2026-07-17 的记忆\n\n## 今日回顾\n\n"
            "09:00至10:30，完成全双工文本链路验证。"
        )
        orchestrator.memory.write_daily_memory(requested_day, content)
        return {"content": content}

    async def generate_image(image_provider, prompt, image_references):
        generation_calls.append((image_provider, prompt, image_references))
        return PNG

    monkeypatch.setattr(orchestrator, "generate_daily_memory", generate_summary)
    monkeypatch.setattr(orchestrator.image_generator, "generate", generate_image)

    first = await orchestrator.generate_daily_image(
        requested_day.isoformat(), provider("never-persist-this-key")
    )
    second = await orchestrator.generate_daily_image(
        requested_day.isoformat(), provider("never-persist-this-key")
    )

    assert summary_calls == ["2026-07-17"]
    assert first["filename"] != second["filename"]
    assert [item["filename"] for item in orchestrator.daily_images("2026-07-17")] == [
        second["filename"],
        first["filename"],
    ]
    _, prompt, image_references = generation_calls[0]
    assert "2026-07-17" in prompt
    assert "横向卡通日程信息图" in prompt
    assert "09:00至10:30，完成全双工文本链路验证" in prompt
    assert "第一张参考图只决定 AI 贾维斯的角色外形" in prompt
    assert "第二张参考图决定构图、配色、线条和质感" in prompt
    assert [path.name for path in image_references] == [
        "jarvis-character-reference.png",
        "jarvis-style-reference.png",
    ]
    sidecars = list((tmp_path / "memory" / "daily-images" / "2026-07-17").glob("*.json"))
    persisted = "\n".join(path.read_text(encoding="utf-8") for path in sidecars)
    assert "never-persist-this-key" not in persisted
    assert "images.example" not in persisted
    assert orchestrator.memory.memory_days() == [requested_day]
    assert len(orchestrator.events.history("memory.image.generated")) == 2


def test_memory_image_content_route_and_traversal_protection(tmp_path):
    settings = Settings(
        memory=MemorySettings(root=tmp_path / "memory"),
        courses=CourseSettings(
            sessions_root=tmp_path / "sessions", output_root=tmp_path / "courses"
        ),
    )
    app = create_app(settings=settings)
    store: MemoryStore = app.state.orchestrator.memory
    day = date(2026, 7, 17)
    metadata = {
        "id": "record.png",
        "date": day.isoformat(),
        "filename": "record.png",
        "created_at": "2026-07-17T12:00:00Z",
        "model_name": "image-model",
        "content_url": "/api/v1/memory/days/2026-07-17/images/record.png",
    }
    store.write_daily_image(day, "record.png", PNG, metadata)

    with TestClient(app) as client:
        response = client.get("/api/v1/memory/days/2026-07-17/images/record.png")
        assert response.status_code == 200
        assert response.content == PNG
        assert client.get(
            "/api/v1/memory/days/2026-07-17/images/%2e%2e%2fsecret.txt"
        ).status_code == 404

    with pytest.raises(FileNotFoundError):
        store.daily_image_path(day, "../secret.txt")


def test_memory_image_history_ignores_malformed_sidecars(tmp_path):
    store = MemoryStore(tmp_path)
    day = date(2026, 7, 17)
    root = store.daily_images_root / day.isoformat()
    root.mkdir(parents=True)
    (root / "list.json").write_text("[]", encoding="utf-8")
    (root / "traversal.json").write_text(
        json.dumps({"filename": "../outside.png"}), encoding="utf-8"
    )
    (tmp_path / "outside.png").write_bytes(PNG)

    assert store.daily_images(day) == []

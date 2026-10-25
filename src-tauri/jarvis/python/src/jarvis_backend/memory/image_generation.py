from __future__ import annotations

import asyncio
import base64
import json
import mimetypes
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass
from pathlib import Path
from uuid import uuid4

MAX_IMAGE_BYTES = 25 * 1024 * 1024
MAX_JSON_BYTES = 36 * 1024 * 1024


def _is_supported_image(content: bytes) -> bool:
    return content.startswith((b"\x89PNG\r\n\x1a\n", b"\xff\xd8\xff")) or (
        content.startswith(b"RIFF") and content[8:12] == b"WEBP"
    )


def _redact(value: str, secret: str) -> str:
    return value.replace(secret, "[redacted]") if secret else value


@dataclass(frozen=True, slots=True)
class ImageProvider:
    base_url: str
    api_key: str
    model_name: str

    def endpoint(self) -> str:
        value = self.base_url.strip().rstrip("/")
        parsed = urllib.parse.urlparse(value)
        if parsed.scheme not in {"http", "https"} or not parsed.netloc:
            raise ValueError("image API base URL must be an absolute HTTP(S) URL")
        if parsed.username or parsed.password:
            raise ValueError("image API base URL must not contain credentials")
        return value if value.endswith("/images/edits") else value + "/images/edits"

    def validate(self) -> None:
        self.endpoint()
        if not self.api_key.strip():
            raise ValueError("image API key must not be empty")
        if not self.model_name.strip():
            raise ValueError("image model name must not be empty")


def _multipart(
    fields: dict[str, str], images: list[tuple[str, bytes, str]]
) -> tuple[bytes, str]:
    boundary = f"----AIJarvis{uuid4().hex}"
    chunks: list[bytes] = []
    for name, value in fields.items():
        chunks.extend(
            [
                f"--{boundary}\r\n".encode(),
                f'Content-Disposition: form-data; name="{name}"\r\n\r\n'.encode(),
                value.encode("utf-8"),
                b"\r\n",
            ]
        )
    for filename, content, content_type in images:
        chunks.extend(
            [
                f"--{boundary}\r\n".encode(),
                (
                    'Content-Disposition: form-data; name="image[]"; '
                    f'filename="{filename}"\r\n'
                ).encode(),
                f"Content-Type: {content_type}\r\n\r\n".encode(),
                content,
                b"\r\n",
            ]
        )
    chunks.append(f"--{boundary}--\r\n".encode())
    return b"".join(chunks), f"multipart/form-data; boundary={boundary}"


def _response_image(value: dict) -> tuple[bytes | None, str | None]:
    candidates = value.get("data") or value.get("images") or value.get("output") or []
    if not isinstance(candidates, list) or not candidates:
        return None, None
    first = candidates[0]
    if not isinstance(first, dict):
        return None, None
    encoded = first.get("b64_json") or first.get("base64") or first.get("result")
    if isinstance(encoded, str) and encoded:
        if encoded.startswith("data:"):
            encoded = encoded.partition(",")[2]
        try:
            return base64.b64decode(encoded, validate=True), None
        except ValueError as exc:
            raise RuntimeError("image API returned invalid base64 data") from exc
    url = first.get("url")
    return None, str(url) if isinstance(url, str) and url else None


def _read_limited(response, limit: int | None = None) -> bytes:
    byte_limit = MAX_IMAGE_BYTES if limit is None else limit
    data = response.read(byte_limit + 1)
    if len(data) > byte_limit:
        raise RuntimeError("image API response exceeds the allowed size")
    return data


class ImageGenerationClient:
    async def generate(
        self,
        provider: ImageProvider,
        prompt: str,
        reference_images: list[Path],
        *,
        timeout_seconds: float = 300.0,
    ) -> bytes:
        return await asyncio.to_thread(
            self._generate_sync,
            provider,
            prompt,
            reference_images,
            timeout_seconds,
        )

    @staticmethod
    def _generate_sync(
        provider: ImageProvider,
        prompt: str,
        reference_images: list[Path],
        timeout_seconds: float,
    ) -> bytes:
        provider.validate()
        if len(reference_images) != 2:
            raise ValueError("exactly two image references are required")
        files = []
        for path in reference_images:
            content = path.read_bytes()
            if (
                not content
                or len(content) > MAX_IMAGE_BYTES
                or not _is_supported_image(content)
            ):
                raise ValueError(f"invalid reference image: {path.name}")
            files.append(
                (
                    path.name,
                    content,
                    mimetypes.guess_type(path.name)[0] or "application/octet-stream",
                )
            )
        body, content_type = _multipart(
            {
                "model": provider.model_name.strip(),
                "prompt": prompt,
                "size": "1536x1024",
                "quality": "high",
            },
            files,
        )
        request = urllib.request.Request(
            provider.endpoint(),
            data=body,
            headers={
                "Authorization": f"Bearer {provider.api_key.strip()}",
                "Content-Type": content_type,
                "Accept": "application/json",
                "User-Agent": "AI-Jarvis/0.1",
            },
            method="POST",
        )
        try:
            with urllib.request.urlopen(request, timeout=timeout_seconds) as response:
                payload = json.loads(_read_limited(response, MAX_JSON_BYTES).decode("utf-8"))
        except urllib.error.HTTPError as exc:
            detail = exc.read(4096).decode("utf-8", errors="replace")
            try:
                parsed = json.loads(detail)
                error = parsed.get("error", {}) if isinstance(parsed, dict) else {}
                detail = str(
                    (error.get("message") if isinstance(error, dict) else error)
                    or (parsed.get("message") if isinstance(parsed, dict) else "")
                    or detail
                )
            except json.JSONDecodeError:
                pass
            detail = _redact(detail, provider.api_key.strip())
            raise RuntimeError(f"image API returned HTTP {exc.code}: {detail[:300]}") from exc
        except (urllib.error.URLError, TimeoutError) as exc:
            raise RuntimeError(f"image API request failed: {exc}") from exc
        except (UnicodeDecodeError, json.JSONDecodeError) as exc:
            raise RuntimeError("image API returned invalid JSON") from exc

        if not isinstance(payload, dict):
            raise RuntimeError("image API returned an invalid response object")
        image, url = _response_image(payload)
        if image is None and url:
            parsed = urllib.parse.urlparse(url)
            if parsed.scheme not in {"http", "https"}:
                raise RuntimeError("image API returned an unsupported image URL")
            try:
                with urllib.request.urlopen(url, timeout=timeout_seconds) as response:
                    image = _read_limited(response)
            except (urllib.error.URLError, TimeoutError) as exc:
                raise RuntimeError(f"generated image download failed: {exc}") from exc
        if not image:
            raise RuntimeError("image API response contains no image")
        if len(image) > MAX_IMAGE_BYTES:
            raise RuntimeError("generated image exceeds 25 MB")
        if not _is_supported_image(image):
            raise RuntimeError("image API returned an unsupported image format")
        return image

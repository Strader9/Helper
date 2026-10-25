from __future__ import annotations

from datetime import UTC, datetime
from typing import Any, Literal

from pydantic import BaseModel, Field, SecretStr

from jarvis_backend.orchestrator.lifecycle import LifecycleState


class HealthResponse(BaseModel):
    status: Literal["ok", "degraded"]
    lifecycle: LifecycleState
    native_connected: bool
    inference_backend: Literal["cuda", "cpu", "unknown"] = "unknown"
    version: str = "0.1.2"


class CommandRequest(BaseModel):
    command: str = Field(min_length=1, max_length=128)
    arguments: dict[str, Any] = Field(default_factory=dict)


class CommandResponse(BaseModel):
    accepted: bool
    result: dict[str, Any]


class PetChatRequest(BaseModel):
    message: str = Field(min_length=1, max_length=2000)


class PetChatResponse(BaseModel):
    reply: str


class DuplexStartRequest(BaseModel):
    instruction: str = Field(min_length=1, max_length=2000)
    session_id: str | None = Field(default=None, pattern=r"^[A-Za-z0-9_-]+$")


class DuplexStatusResponse(BaseModel):
    active: bool
    session_id: str | None
    instruction: str


class SceneObservation(BaseModel):
    score: float = Field(ge=0, le=1)


class SceneResponse(BaseModel):
    active: bool
    changed: bool


class BarrageRequest(BaseModel):
    id: str = Field(min_length=1, max_length=128)
    text: str = Field(min_length=1, max_length=4096)
    created_at: datetime = Field(default_factory=lambda: datetime.now(UTC))
    priority: int = Field(default=0, ge=-100, le=100)


class BarrageResponse(BaseModel):
    decision: str


class MemoryStatusResponse(BaseModel):
    event_count: int
    summary: str | None
    fact_count: int
    today: str
    today_event_count: int
    today_generated: bool


class MemoryDaySummary(BaseModel):
    date: str
    event_count: int
    generated: bool
    preview: str


class MemoryDayResponse(BaseModel):
    date: str
    event_count: int
    generated: bool
    content: str


class MemoryImageGenerateRequest(BaseModel):
    base_url: str = Field(min_length=8, max_length=2048)
    api_key: SecretStr
    model_name: str = Field(min_length=1, max_length=256)


class MemoryImageResponse(BaseModel):
    id: str
    date: str
    filename: str
    created_at: str
    model_name: str
    content_url: str


class MemorySummaryResponse(BaseModel):
    summary: str


class MemoryClearRequest(BaseModel):
    confirm: bool = False


class MemoryClearResponse(BaseModel):
    cleared: bool


class CourseStartRequest(BaseModel):
    title: str = Field(min_length=1, max_length=256)
    session_id: str | None = Field(default=None, pattern=r"^[A-Za-z0-9_-]+$")


class CourseKeyframeRequest(BaseModel):
    image_base64: str = Field(min_length=1, max_length=6_000_000)
    timestamp_ms: int = Field(ge=0)
    extension: Literal["png", "jpg", "jpeg", "webp"] = "png"
    metadata: dict[str, Any] = Field(default_factory=dict)


class CourseResponse(BaseModel):
    id: str
    title: str
    status: str
    created_at: str
    updated_at: str
    summary: str
    keyframes: list[dict[str, Any]]
    error: str | None
    output_path: str | None


class EventMessage(BaseModel):
    id: str
    topic: str
    payload: dict[str, Any]
    occurred_at: datetime

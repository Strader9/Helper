from __future__ import annotations

import os

from fastapi import APIRouter, Depends, Header, HTTPException, Request, status
from fastapi.responses import FileResponse

from jarvis_backend.memory import ImageProvider
from jarvis_backend.orchestrator import OrchestrationService

from .schemas import (
    BarrageRequest,
    BarrageResponse,
    CommandRequest,
    CommandResponse,
    CourseKeyframeRequest,
    CourseResponse,
    CourseStartRequest,
    DuplexStartRequest,
    DuplexStatusResponse,
    EventMessage,
    HealthResponse,
    MemoryClearRequest,
    MemoryClearResponse,
    MemoryDayResponse,
    MemoryDaySummary,
    MemoryImageGenerateRequest,
    MemoryImageResponse,
    MemoryStatusResponse,
    MemorySummaryResponse,
    PetChatRequest,
    PetChatResponse,
    SceneObservation,
    SceneResponse,
)


async def authorize(request: Request, authorization: str | None = Header(default=None)) -> None:
    expected = request.app.state.orchestrator.settings.server.bearer_token
    if expected and authorization != f"Bearer {expected}":
        raise HTTPException(status.HTTP_401_UNAUTHORIZED, "invalid bearer token")


router = APIRouter(prefix="/api/v1", dependencies=[Depends(authorize)])


def service(request: Request) -> OrchestrationService:
    return request.app.state.orchestrator


@router.get("/health", response_model=HealthResponse)
async def health(request: Request) -> HealthResponse:
    orchestrator = service(request)
    state = orchestrator.lifecycle.snapshot.state
    return HealthResponse(
        status="ok" if state == "ready" else "degraded",
        lifecycle=state,
        native_connected=orchestrator.native_connected,
        inference_backend=os.getenv("JARVIS_ACTIVE_INFERENCE_BACKEND", "unknown"),
    )


@router.post("/commands", response_model=CommandResponse)
async def command(body: CommandRequest, request: Request) -> CommandResponse:
    result = await service(request).command(body.command, body.arguments)
    return CommandResponse(accepted=True, result=result)


@router.post("/assistant/chat", response_model=PetChatResponse)
async def pet_chat(body: PetChatRequest, request: Request) -> PetChatResponse:
    try:
        reply = await service(request).pet_chat(body.message)
    except ValueError as exc:
        raise HTTPException(status.HTTP_422_UNPROCESSABLE_CONTENT, str(exc)) from exc
    except (RuntimeError, TimeoutError) as exc:
        raise HTTPException(
            status.HTTP_503_SERVICE_UNAVAILABLE,
            "本地模型暂时无法回复，请稍后重试",
        ) from exc
    return PetChatResponse(reply=reply)


@router.get("/duplex", response_model=DuplexStatusResponse)
async def duplex_status(request: Request) -> DuplexStatusResponse:
    return DuplexStatusResponse(**service(request).duplex_status())


@router.post("/duplex", response_model=DuplexStatusResponse)
async def start_duplex(body: DuplexStartRequest, request: Request) -> DuplexStatusResponse:
    try:
        result = await service(request).start_duplex(body.instruction, body.session_id)
    except (ValueError, RuntimeError) as exc:
        raise HTTPException(status.HTTP_422_UNPROCESSABLE_ENTITY, str(exc)) from exc
    return DuplexStatusResponse(**result)


@router.delete("/duplex", response_model=DuplexStatusResponse)
async def stop_duplex(request: Request) -> DuplexStatusResponse:
    return DuplexStatusResponse(**(await service(request).stop_duplex()))


@router.post("/scene/observations", response_model=SceneResponse)
async def scene(body: SceneObservation, request: Request) -> SceneResponse:
    orchestrator = service(request)
    changed = await orchestrator.observe_scene(body.score)
    return SceneResponse(active=orchestrator.scene.active, changed=changed)


@router.post("/barrage", response_model=BarrageResponse)
async def barrage(body: BarrageRequest, request: Request) -> BarrageResponse:
    decision = await service(request).submit_barrage(
        body.id, body.text, body.created_at, body.priority
    )
    return BarrageResponse(decision=decision)


@router.get("/memory/status", response_model=MemoryStatusResponse)
async def memory_status(request: Request) -> MemoryStatusResponse:
    return MemoryStatusResponse.model_validate(await service(request).memory_status())


@router.post("/memory/summarize", response_model=MemorySummaryResponse)
async def memory_summarize(request: Request) -> MemorySummaryResponse:
    return MemorySummaryResponse(summary=await service(request).summarize_memory())


@router.post("/memory/clear", response_model=MemoryClearResponse)
async def memory_clear(body: MemoryClearRequest, request: Request) -> MemoryClearResponse:
    if not body.confirm:
        raise HTTPException(status.HTTP_409_CONFLICT, "memory clear requires confirmation")
    await service(request).clear_memory()
    return MemoryClearResponse(cleared=True)


@router.get("/memory/days", response_model=list[MemoryDaySummary])
async def memory_days(request: Request) -> list[MemoryDaySummary]:
    return [
        MemoryDaySummary.model_validate(item)
        for item in await service(request).list_daily_memories()
    ]


@router.get("/memory/days/{day}", response_model=MemoryDayResponse)
async def memory_day(day: str, request: Request) -> MemoryDayResponse:
    try:
        result = await service(request).get_daily_memory(day)
    except ValueError as exc:
        raise HTTPException(status.HTTP_422_UNPROCESSABLE_CONTENT, str(exc)) from exc
    except FileNotFoundError as exc:
        raise HTTPException(status.HTTP_404_NOT_FOUND, "memory day not found") from exc
    return MemoryDayResponse.model_validate(result)


@router.post("/memory/days/{day}/generate", response_model=MemoryDayResponse)
async def memory_day_generate(day: str, request: Request) -> MemoryDayResponse:
    try:
        result = await service(request).generate_daily_memory(day)
    except ValueError as exc:
        raise HTTPException(status.HTTP_422_UNPROCESSABLE_CONTENT, str(exc)) from exc
    except (RuntimeError, TimeoutError) as exc:
        raise HTTPException(
            status.HTTP_503_SERVICE_UNAVAILABLE,
            "本地模型暂时无法生成记忆总结，请稍后重试",
        ) from exc
    return MemoryDayResponse.model_validate(result)


@router.get("/memory/images", response_model=list[MemoryImageResponse])
async def memory_images(request: Request) -> list[MemoryImageResponse]:
    return [
        MemoryImageResponse.model_validate(item)
        for item in service(request).daily_images()
    ]


@router.get("/memory/days/{day}/images", response_model=list[MemoryImageResponse])
async def memory_day_images(day: str, request: Request) -> list[MemoryImageResponse]:
    try:
        items = service(request).daily_images(day)
    except ValueError as exc:
        raise HTTPException(status.HTTP_422_UNPROCESSABLE_CONTENT, str(exc)) from exc
    return [MemoryImageResponse.model_validate(item) for item in items]


@router.post("/memory/days/{day}/images", response_model=MemoryImageResponse)
async def memory_day_image_generate(
    day: str, body: MemoryImageGenerateRequest, request: Request
) -> MemoryImageResponse:
    try:
        result = await service(request).generate_daily_image(
            day,
            ImageProvider(
                base_url=body.base_url,
                api_key=body.api_key.get_secret_value(),
                model_name=body.model_name,
            ),
        )
    except (FileNotFoundError, ValueError, RuntimeError) as exc:
        raise HTTPException(status.HTTP_422_UNPROCESSABLE_CONTENT, str(exc)) from exc
    return MemoryImageResponse.model_validate(result)


@router.get("/memory/days/{day}/images/{filename}", response_class=FileResponse)
async def memory_day_image_content(
    day: str, filename: str, request: Request
) -> FileResponse:
    try:
        path = service(request).daily_image_path(day, filename)
    except (FileNotFoundError, ValueError) as exc:
        raise HTTPException(status.HTTP_404_NOT_FOUND, "memory image not found") from exc
    return FileResponse(path)


def course_response(state: object) -> CourseResponse:
    return CourseResponse.model_validate(state, from_attributes=True)


@router.post("/courses/start", response_model=CourseResponse, status_code=status.HTTP_201_CREATED)
async def course_start(body: CourseStartRequest, request: Request) -> CourseResponse:
    try:
        state = await service(request).start_course(body.title, body.session_id)
    except FileExistsError as exc:
        raise HTTPException(status.HTTP_409_CONFLICT, "course already exists") from exc
    return course_response(state)


@router.post("/courses/{session_id}/finish", response_model=CourseResponse)
async def course_finish(session_id: str, request: Request) -> CourseResponse:
    try:
        state = await service(request).finish_course(session_id)
    except FileNotFoundError as exc:
        raise HTTPException(status.HTTP_404_NOT_FOUND, "course not found") from exc
    return course_response(state)


@router.post("/courses/{session_id}/keyframes", response_model=CourseResponse)
async def course_keyframe(
    session_id: str, body: CourseKeyframeRequest, request: Request
) -> CourseResponse:
    try:
        state = await service(request).add_course_keyframe(
            session_id,
            body.image_base64,
            body.timestamp_ms,
            body.extension,
            body.metadata,
        )
    except FileNotFoundError as exc:
        raise HTTPException(status.HTTP_404_NOT_FOUND, "course not found") from exc
    except (ValueError, RuntimeError) as exc:
        raise HTTPException(status.HTTP_422_UNPROCESSABLE_ENTITY, str(exc)) from exc
    return course_response(state)


@router.get("/courses", response_model=list[CourseResponse])
async def courses(request: Request) -> list[CourseResponse]:
    return [course_response(item) for item in await service(request).list_courses()]


@router.get("/courses/{session_id}", response_model=CourseResponse)
async def course(session_id: str, request: Request) -> CourseResponse:
    try:
        state = await service(request).get_course(session_id)
    except (FileNotFoundError, ValueError) as exc:
        raise HTTPException(status.HTTP_404_NOT_FOUND, "course not found") from exc
    return course_response(state)


@router.get("/events", response_model=list[EventMessage])
async def events(request: Request, topic: str | None = None) -> list[EventMessage]:
    history = service(request).events.history(topic)
    return [EventMessage.model_validate(event, from_attributes=True) for event in history]

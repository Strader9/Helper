from __future__ import annotations

from fastapi import APIRouter, WebSocket, WebSocketDisconnect

from jarvis_backend.api.schemas import EventMessage

router = APIRouter()


@router.websocket("/ws/events")
async def event_stream(websocket: WebSocket) -> None:
    expected = websocket.app.state.orchestrator.settings.server.bearer_token
    supplied = websocket.headers.get("authorization") or (
        f"Bearer {websocket.query_params['token']}" if "token" in websocket.query_params else None
    )
    if expected and supplied != f"Bearer {expected}":
        await websocket.close(code=4401)
        return
    await websocket.accept()
    topics = tuple(filter(None, websocket.query_params.get("topics", "").split(",")))
    subscription = await websocket.app.state.orchestrator.events.subscribe(*topics)
    try:
        async for event in subscription:
            message = EventMessage.model_validate(event, from_attributes=True)
            await websocket.send_json(message.model_dump(mode="json"))
    except WebSocketDisconnect:
        pass
    finally:
        await subscription.close()

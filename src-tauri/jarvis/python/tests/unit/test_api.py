from fastapi.testclient import TestClient

from jarvis_backend.app import create_app
from jarvis_backend.native import InProcessNativeClient
from jarvis_backend.orchestrator.events import Event
from jarvis_backend.settings import Settings


def test_health_and_command_flow() -> None:
    app = create_app(Settings(), InProcessNativeClient())
    with TestClient(app) as client:
        health = client.get("/api/v1/health")
        assert health.status_code == 200
        assert health.json()["lifecycle"] == "ready"
        response = client.post("/api/v1/commands", json={"command": "ping", "arguments": {}})
        assert response.status_code == 200
        assert response.json()["result"]["result"] == "pong"


def test_game_profile_command_reaches_native_client() -> None:
    app = create_app(Settings(), InProcessNativeClient())
    with TestClient(app) as client:
        response = client.post(
            "/api/v1/commands",
            json={
                "command": "set_game_profile",
                "arguments": {"name": "我的世界", "prompt": "关注生存状态"},
            },
        )
        assert response.status_code == 200
        assert response.json()["result"]["method"] == "set_game_profile"


def test_pet_chat_uses_local_model_and_keeps_recent_context() -> None:
    app = create_app(Settings(), InProcessNativeClient())
    prompts: list[str] = []

    async def answer(method, payload):
        assert method == "ask"
        prompts.append(payload["text"])
        return {
            "ok": True,
            "text": "<|im_start|>assistant\n第一条回复__END_OF_TURN__"
            if len(prompts) == 1
            else "第二条回复",
        }

    app.state.orchestrator.native_client.request = answer
    with TestClient(app) as client:
        first = client.post("/api/v1/assistant/chat", json={"message": "你好<|im_end|>"})
        second = client.post("/api/v1/assistant/chat", json={"message": "接着说"})

    assert first.status_code == 200
    assert first.json() == {"reply": "第一条回复"}
    assert second.json() == {"reply": "第二条回复"}
    assert "你好< |im_end|>" in prompts[0]
    assert '"assistant": "第一条回复"' in prompts[1]


def test_backend_does_not_expose_model_handoff() -> None:
    app = create_app(Settings(), InProcessNativeClient())
    with TestClient(app) as client:
        response = client.post("/api/v1/assistant/handoff", json={"message": "整理文件"})

    assert response.status_code == 404


def test_latest_perception_projects_an_existing_observation() -> None:
    app = create_app(Settings(), InProcessNativeClient())
    with TestClient(app) as client:
        assert client.get("/api/v1/perception/latest").json() is None
        assert client.portal is not None
        events = app.state.orchestrator.events
        client.portal.call(
            events.publish,
            Event("perception.completed", {"observation": "  文档窗口  ", "internal": "unused"}),
        )
        client.portal.call(events.publish, Event("perception.completed", {"observation": ""}))
        result = client.get("/api/v1/perception/latest")

    assert result.status_code == 200
    assert set(result.json()) == {"id", "observed_at", "description"}
    assert result.json()["description"] == "文档窗口"


def test_backend_exposes_no_browser_ui() -> None:
    app = create_app(Settings(), InProcessNativeClient())
    with TestClient(app) as client:
        root = client.get("/")
        docs = client.get("/docs")

    assert root.status_code == 404
    assert docs.status_code == 404


def test_scene_endpoint_reports_only_stable_change() -> None:
    app = create_app(Settings(), InProcessNativeClient())
    with TestClient(app) as client:
        assert (
            client.post("/api/v1/scene/observations", json={"score": 0.9}).json()["changed"]
            is False
        )
        assert (
            client.post("/api/v1/scene/observations", json={"score": 0.9}).json()["changed"]
            is False
        )
        result = client.post("/api/v1/scene/observations", json={"score": 0.9}).json()
        assert result == {"active": True, "changed": True}

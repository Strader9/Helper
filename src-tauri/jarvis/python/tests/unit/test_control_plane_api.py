import asyncio
import base64
import json
import re
import time
from datetime import UTC, datetime

import pytest
from fastapi.testclient import TestClient

import jarvis_backend.orchestrator.service as service_module
from jarvis_backend.app import create_app
from jarvis_backend.courses import CourseStatus
from jarvis_backend.memory import MemoryEvent
from jarvis_backend.settings import (
    CourseSettings,
    InteractionSettings,
    MemorySettings,
    SceneSettings,
    Settings,
)


def make_client(tmp_path):
    settings = Settings(
        memory=MemorySettings(root=tmp_path / "memory"),
        courses=CourseSettings(
            sessions_root=tmp_path / "sessions", output_root=tmp_path / "courses"
        ),
    )
    return TestClient(create_app(settings=settings))


def test_daily_memory_activity_categories_use_observed_content():
    def category(text: str, scene: str = "other") -> str:
        event = MemoryEvent("id", "2026-07-16T00:00:00Z", "activity", text, {"scene": scene})
        return service_module.OrchestrationService._memory_activity_category(event)

    assert category("桌面显示浏览器和文件管理器图标，界面静止无明显操作。") == "基本无操作"
    assert category("科幻视频中飞船穿越星云，无交互元素或课程内容。", "course") == (
        "观看视频或游戏画面"
    )
    assert category("《我的世界》第一人称视角，玩家正在用镐子挖掘方块。", "course") == (
        "玩游戏"
    )
    assert category("正在使用在线视频裁剪器处理视频文件。") == "媒体处理"
    assert category("浏览器显示 Bilibili 游戏攻略搜索结果。", "game") == "上网浏览"
    assert category("网课正在讲解 C++ 内存对齐，并整理学习笔记。") == "课程学习"

    events = [
        MemoryEvent("1", "2026-07-16T08:00:00Z", "activity", "编辑项目代码。", {}),
        MemoryEvent(
            "2", "2026-07-16T08:02:00Z", "activity", "浏览 Bilibili 游戏攻略搜索结果。", {}
        ),
        MemoryEvent("3", "2026-07-16T08:04:00Z", "activity", "调试项目代码。", {}),
    ]
    assert "上网浏览1条" in (
        service_module.OrchestrationService._compact_memory_timeline(events)
    )


def test_memory_status_summarize_and_confirmed_clear(tmp_path):
    with make_client(tmp_path) as client:
        store = client.app.state.orchestrator.memory
        store.append("note", "Remember the blue bicycle")

        assert client.get("/api/v1/memory/status").json()["event_count"] == 1
        summary = client.post("/api/v1/memory/summarize")
        assert summary.status_code == 200
        assert summary.json() == {"summary": "Remember the blue bicycle"}
        assert client.post("/api/v1/memory/clear", json={"confirm": False}).status_code == 409
        assert client.get("/api/v1/memory/status").json()["event_count"] == 1
        assert client.post("/api/v1/memory/clear", json={"confirm": True}).json() == {
            "cleared": True
        }
        assert client.get("/api/v1/memory/status").json()["event_count"] == 0

        topics = [event["topic"] for event in client.get("/api/v1/events").json()]
        assert "memory.summarized" in topics
        assert "memory.cleared" in topics


def test_perception_implicitly_maintains_daily_memory_and_deduplicates(
    tmp_path, monkeypatch
):
    clock = [100.0]
    monkeypatch.setattr(service_module.time, "monotonic", lambda: clock[0])
    with make_client(tmp_path) as client:
        native = client.app.state.orchestrator.native_client
        summary_prompts = []

        async def summarize(method, payload):
            assert method == "ask"
            summary_prompts.append(payload["text"])
            latest = max(re.findall(r"\b\d{2}:\d{2}\b", payload["text"]))
            return {
                "text": (
                    "10:00至10:05（约5分钟），编辑 AI 贾维斯的记忆系统代码。"
                    f"10:05至{latest}，运行自动化测试并检查结果。"
                )
            }

        monkeypatch.setattr(native, "request", summarize)

        def perceive(request_id, observation):
            client.portal.call(
                native.emit,
                {
                    "type": "perception.completed",
                    "request_id": request_id,
                    "text": json.dumps(
                        {
                            "scene": "other",
                            "confidence": 0.91,
                            "observation": observation,
                        },
                        ensure_ascii=False,
                    ),
                },
            )
            time.sleep(0.02)

        perceive(1, "用户正在编辑 AI 贾维斯的记忆系统代码。")
        perceive(2, "用户正在编辑 AI 贾维斯的记忆系统代码。")
        assert client.get("/api/v1/memory/status").json()["today_event_count"] == 1

        clock[0] += 121
        perceive(3, "用户正在运行自动化测试并检查测试结果。")
        status = client.get("/api/v1/memory/status").json()
        assert status["today_event_count"] == 2
        assert status["today_generated"] is False

        days = client.get("/api/v1/memory/days").json()
        assert days == [
            {
                "date": status["today"],
                "event_count": 2,
                "generated": False,
                "preview": "用户正在编辑 AI 贾维斯的记忆系统代码。",
            }
        ]
        generated = client.post(
            f"/api/v1/memory/days/{status['today']}/generate"
        ).json()
        assert generated["event_count"] == 2
        assert "10:00至10:05" in generated["content"]
        assert "运行自动化测试并检查结果" in generated["content"]
        assert len(summary_prompts) == 1
        assert "只输出一个连贯正文段落" in summary_prompts[0]
        assert "用户正在编辑 AI 贾维斯的记忆系统代码" in summary_prompts[0]
        assert "用户正在运行自动化测试" in summary_prompts[0]
        assert client.get(
            f"/api/v1/memory/days/{status['today']}"
        ).json() == generated
        assert client.get("/api/v1/memory/days/not-a-date").status_code == 422


def test_daily_memory_generation_reports_local_model_failure(tmp_path, monkeypatch):
    with make_client(tmp_path) as client:
        orchestrator = client.app.state.orchestrator
        orchestrator.memory.append("activity", "用户正在浏览项目文件。", {"scene": "other"})

        async def fail(_method, _payload):
            raise TimeoutError("model timed out")

        monkeypatch.setattr(orchestrator.native_client, "request", fail)
        today = client.get("/api/v1/memory/status").json()["today"]
        response = client.post(f"/api/v1/memory/days/{today}/generate")

        assert response.status_code == 503
        assert response.json()["detail"] == "本地模型暂时无法生成记忆总结，请稍后重试"


async def test_large_daily_memory_is_compacted_before_single_model_summary(
    tmp_path, monkeypatch
):
    settings = Settings(
        memory=MemorySettings(root=tmp_path / "memory"),
        courses=CourseSettings(sessions_root=tmp_path / "sessions"),
    )
    orchestrator = create_app(settings=settings).state.orchestrator
    for index in range(90):
        orchestrator.memory.append(
            "activity",
            f"用户持续浏览项目文件并检查模块关系，这是第 {index + 1} 条连续观察记录。",
            {"scene": "other"},
            timestamp=datetime.now(UTC),
        )
    prompts = []

    async def summarize(method, payload):
        assert method == "ask"
        prompts.append(payload)
        latest = max(re.findall(r"\b\d{2}:\d{2}\b", payload["text"]))
        return {"text": f"10:00至{latest}，浏览项目文件并检查模块关系。"}

    monkeypatch.setattr(orchestrator.native_client, "request", summarize)
    today = datetime.now().astimezone().date().isoformat()
    result = await orchestrator.generate_daily_memory(today)

    assert len(prompts) == 1
    assert all(len(item["text"]) < 8000 for item in prompts)
    assert all(item["_timeout_seconds"] == 120 for item in prompts)
    assert "[项目工作，记录90条]" in prompts[0]["text"]
    assert "浏览项目文件并检查模块关系" in result["content"]


async def test_incomplete_daily_summary_does_not_replace_existing_document(
    tmp_path, monkeypatch
):
    settings = Settings(
        memory=MemorySettings(root=tmp_path / "memory"),
        courses=CourseSettings(sessions_root=tmp_path / "sessions"),
    )
    orchestrator = create_app(settings=settings).state.orchestrator
    event = orchestrator.memory.append(
        "activity",
        "用户正在编写并调试项目代码。",
        {"scene": "other"},
        timestamp=datetime.now(UTC),
    )
    local_time = datetime.fromisoformat(event.timestamp.replace("Z", "+00:00")).astimezone()
    orchestrator.memory.write_daily_memory(local_time.date(), "# existing\n\n完整的旧总结。")

    async def summarize(_method, _payload):
        return {"text": "10:00至11:15，进行项目开发，中间窗口展示"}

    monkeypatch.setattr(orchestrator.native_client, "request", summarize)
    with pytest.raises(RuntimeError, match="latest event"):
        await orchestrator.generate_daily_memory(local_time.date().isoformat())

    assert orchestrator.memory.read_daily_memory(local_time.date()) == (
        "# existing\n\n完整的旧总结。\n"
    )


async def test_recent_activity_is_not_duplicated_after_restart(tmp_path, monkeypatch):
    clock = [50.0]
    monkeypatch.setattr(service_module.time, "monotonic", lambda: clock[0])
    settings = Settings(
        memory=MemorySettings(root=tmp_path / "memory"),
        courses=CourseSettings(sessions_root=tmp_path / "sessions"),
    )
    first = create_app(settings=settings).state.orchestrator
    first.memory.append(
        "activity",
        "用户正在整理项目文档与开发记录。",
        {"scene": "other", "confidence": 0.9, "source": "perception"},
    )

    restarted = create_app(settings=settings).state.orchestrator
    clock[0] += 30
    await restarted._handle_perception(
        {
            "text": json.dumps(
                {
                    "scene": "other",
                    "confidence": 0.9,
                    "observation": "用户正在整理项目文档与开发记录。",
                },
                ensure_ascii=False,
            )
        }
    )

    assert len(restarted.memory.events()) == 1


def test_course_start_finish_and_query(tmp_path):
    with make_client(tmp_path) as client:
        started = client.post(
            "/api/v1/courses/start", json={"title": "Python Lesson", "session_id": "lesson-1"}
        )
        assert started.status_code == 201
        assert started.json()["status"] == "recording"

        listed = client.get("/api/v1/courses")
        assert [item["id"] for item in listed.json()] == ["lesson-1"]
        assert client.get("/api/v1/courses/lesson-1").json()["title"] == "Python Lesson"

        finished = client.post("/api/v1/courses/lesson-1/finish")
        assert finished.status_code == 200
        assert finished.json()["status"] == "complete"
        assert (tmp_path / "courses" / "lesson-1" / "README.md").is_file()
        assert (tmp_path / "courses" / "lesson-1" / "images").is_dir()
        assert client.get("/api/v1/courses/missing").status_code == 404

        topics = [event["topic"] for event in client.get("/api/v1/events").json()]
        assert "course.started" in topics
        assert "course.finished" in topics


def test_continuous_perception_generates_barrage_and_course_notes(tmp_path):
    with make_client(tmp_path) as client:
        native = client.app.state.orchestrator.native_client

        game_payload = {
            "type": "perception.completed",
            "request_id": 1 << 63,
            "text": '{"scene":"game","confidence":0.94,"barrage":"漂亮的反杀！",'
            '"scene_evidence":{"game_surface":true,"interactive_gameplay":true},'
            '"course_note":"","course_title":""}',
        }
        client.portal.call(native.emit, game_payload)
        client.portal.call(native.emit, {**game_payload, "request_id": (1 << 63) + 1})
        time.sleep(0.02)
        events = client.get("/api/v1/events").json()
        assert any(
            event["topic"] == "barrage.generated" and event["payload"]["text"] == "漂亮的反杀！"
            for event in events
        )

        course_payload = {
            "type": "perception.completed",
            "request_id": (1 << 63) + 2,
            "text": '{"scene":"course","confidence":0.91,"barrage":"",'
            '"course_transcript":"牛顿第二定律是 F=ma。",'
            '"course_note":"牛顿第二定律是 F=ma。","course_title":"高中物理",'
            '"capture_keyframe":true,"keyframe_note":"牛顿第二定律公式。",'
            '"assistant_message":""}',
        }
        client.portal.call(native.emit, course_payload)
        client.portal.call(native.emit, {**course_payload, "request_id": (1 << 63) + 3})
        client.portal.call(native.emit, {**course_payload, "request_id": (1 << 63) + 4})
        client.portal.call(native.emit, {**course_payload, "request_id": (1 << 63) + 5})
        time.sleep(0.02)
        courses = client.get("/api/v1/courses").json()
        assert len(courses) == 1
        assert courses[0]["status"] == "recording"
        assert courses[0]["summary"] == ""
        transcript = tmp_path / "sessions" / courses[0]["id"] / "transcript.md"
        assert "F=ma" in transcript.read_text(encoding="utf-8")
        events = client.get("/api/v1/events").json()
        keyframe_request = next(
            event for event in events if event["topic"] == "course.keyframe.requested"
        )
        png = b"\x89PNG\r\n\x1a\nkeyframe"
        recorded = client.post(
            f"/api/v1/courses/{courses[0]['id']}/keyframes",
            json={
                "image_base64": base64.b64encode(png).decode("ascii"),
                "timestamp_ms": keyframe_request["payload"]["timestamp_ms"],
                "extension": "png",
                "metadata": {"source": "desktop"},
            },
        )
        assert recorded.status_code == 200
        assert len(recorded.json()["keyframes"]) == 1

        client.app.state.orchestrator.settings.courses.exit_grace_seconds = 0
        client.app.state.orchestrator.settings.courses.exit_samples = 3
        for offset in range(6, 9):
            client.portal.call(
                native.emit,
                {
                    "type": "perception.completed",
                    "request_id": (1 << 63) + offset,
                    "text": '{"scene":"other","confidence":0.8,"barrage":"",'
                    '"course_note":"","course_title":"",'
                    f'"assistant_message":"{"下载已经完成。" if offset == 6 else ""}"}}',
                },
            )
        time.sleep(0.03)
        completed = client.get("/api/v1/courses").json()[0]
        assert completed["status"] == "complete"
        assert completed["output_path"] is not None
        output = tmp_path / "courses" / completed["id"]
        assert (output / "README.md").is_file()
        assert list((output / "images").glob("*.png"))
        events = client.get("/api/v1/events").json()
        assert not any(
            event["topic"] == "assistant.message" for event in events
        )

        client.portal.call(
            native.emit,
            {
                "type": "perception.completed",
                "request_id": (1 << 63) + 9,
                "text": (
                    '{"scene":"other","confidence":0.9,'
                    '"assistant_message":"新场景已经稳定，可以按刚才的目标继续推进。"}'
                ),
            },
        )
        time.sleep(0.02)
        events = client.get("/api/v1/events").json()
        assistant_messages = [
            event["payload"]["text"]
            for event in events
            if event["topic"] == "assistant.message"
        ]
        assert assistant_messages == ["新场景已经稳定，可以按刚才的目标继续推进。"]


async def test_display_scene_uses_configured_entry_and_exit_samples(tmp_path):
    settings = Settings(
        scene=SceneSettings(
            display_enter_samples=2,
            game_enter_samples=1,
            display_exit_samples=2,
            game_exit_samples=2,
        ),
        memory=MemorySettings(root=tmp_path / "memory"),
        courses=CourseSettings(sessions_root=tmp_path / "sessions"),
    )
    orchestrator = create_app(settings=settings).state.orchestrator

    async def perceive(scene):
        payload = {
            "scene": scene,
            "confidence": 0.95,
            "scene_evidence": {
                "game_surface": scene == "game",
                "interactive_gameplay": scene == "game",
                "game_video_or_stream": False,
                "fullscreen_game_media": False,
                "non_game_surface": scene == "other",
            },
            "observation": "玩家正在第一人称游戏中移动" if scene == "game" else "普通桌面",
        }
        await orchestrator._handle_perception(
            {"type": "perception.completed", "text": json.dumps(payload, ensure_ascii=False)}
        )
        return orchestrator.events.history("perception.completed")[-1].payload["scene"]

    assert await perceive("game") == "game"
    assert await perceive("other") == "game"
    assert await perceive("other") == "other"


async def test_default_game_scene_requires_two_clear_samples(tmp_path):
    settings = Settings(
        memory=MemorySettings(root=tmp_path / "memory"),
        courses=CourseSettings(sessions_root=tmp_path / "sessions"),
    )
    orchestrator = create_app(settings=settings).state.orchestrator

    async def perceive(scene, *, non_game_surface=False):
        await orchestrator._handle_perception(
            {
                "type": "perception.completed",
                "text": json.dumps(
                    {
                        "scene": scene,
                        "confidence": 0.95,
                        "scene_evidence": {
                            "game_surface": scene == "game",
                            "interactive_gameplay": scene == "game",
                            "non_game_surface": non_game_surface,
                        },
                        "observation": "运行中的游戏" if scene == "game" else "桌面",
                    },
                    ensure_ascii=False,
                ),
            }
        )
        return orchestrator.events.history("perception.completed")[-1].payload["scene"]

    assert await perceive("game") == "other"
    assert await perceive("game") == "game"
    assert await perceive("other", non_game_surface=True) == "other"

    orchestrator.display_scene.force("game")
    assert await perceive("other") == "game"
    assert await perceive("other") == "other"


def test_non_game_launcher_evidence_cannot_enter_game_scene():
    result = service_module.OrchestrationService._parse_perception(
        json.dumps(
            {
                "scene": "game",
                "confidence": 0.99,
                "scene_evidence": {
                    "game_surface": True,
                    "interactive_gameplay": True,
                    "game_video_or_stream": False,
                    "fullscreen_game_media": False,
                    "non_game_surface": True,
                },
                "observation": "Steam 游戏库已打开，但游戏尚未启动",
                "barrage_candidates": ["准备开打"],
            },
            ensure_ascii=False,
        )
    )

    assert result["scene"] == "other"
    assert result["barrage_candidates"] == []


async def test_game_surface_without_strong_entry_evidence_stays_other(tmp_path):
    settings = Settings(
        scene=SceneSettings(game_enter_samples=1),
        memory=MemorySettings(root=tmp_path / "memory"),
        courses=CourseSettings(sessions_root=tmp_path / "sessions"),
    )
    orchestrator = create_app(settings=settings).state.orchestrator
    payload = {
        "scene": "game",
        "confidence": 0.96,
        "scene_evidence": {
            "game_surface": True,
            "interactive_gameplay": False,
            "game_video_or_stream": False,
            "fullscreen_game_media": False,
        },
        "observation": "窗口中显示静止的游戏风格场景和状态栏",
        "barrage_candidates": ["这局面可以继续推进"],
    }

    async def perceive():
        await orchestrator._handle_perception(
            {
                "type": "perception.completed",
                "text": json.dumps(payload, ensure_ascii=False),
            }
        )
        return orchestrator.events.history("perception.completed")[-1].payload

    for _ in range(2):
        completed = await perceive()
        assert completed["scene"] == "other"
        assert completed["observed_scene"] == "other"
        assert completed["game_entry_rejected"] is True
        assert completed["barrage_candidates"] == []
    assert orchestrator.events.history("barrage.generated") == []

    orchestrator.display_scene.force("game")
    completed = await perceive()
    assert completed["scene"] == "game"
    assert completed["observed_scene"] == "game"
    assert completed["game_entry_rejected"] is False
    assert [
        event.payload["text"]
        for event in orchestrator.events.history("barrage.generated")
    ] == ["这局面可以继续推进"]


async def test_game_classification_switches_scene_before_barrage_generation(tmp_path):
    settings = Settings(
        scene=SceneSettings(game_enter_samples=1),
        memory=MemorySettings(root=tmp_path / "memory"),
        courses=CourseSettings(sessions_root=tmp_path / "sessions"),
    )
    orchestrator = create_app(settings=settings).state.orchestrator
    classification = {
        "scene": "game",
        "confidence": 0.96,
        "scene_evidence": {
            "game_surface": True,
            "interactive_gameplay": True,
            "game_video_or_stream": False,
            "fullscreen_game_media": False,
        },
        "observation": "玩家正在第一人称射击游戏中移动",
        "barrage_pending": True,
    }

    await orchestrator._handle_perception(
        {
            "type": "perception.completed",
            "text": json.dumps(classification, ensure_ascii=False),
        }
    )

    completed = orchestrator.events.history("perception.completed")[-1].payload
    assert completed["scene"] == "game"
    assert completed["observed_scene"] == "game"
    assert completed["barrage_pending"] is True
    assert completed["barrage_source"] == "pending"
    assert completed["barrage_candidates"] == []
    assert orchestrator.events.history("barrage.generated") == []


async def test_uncertain_game_exit_requires_more_samples(tmp_path):
    settings = Settings(
        scene=SceneSettings(
            display_enter_samples=2,
            game_enter_samples=1,
            display_exit_samples=2,
            game_exit_samples=2,
            game_uncertain_exit_samples=4,
        ),
        memory=MemorySettings(root=tmp_path / "memory"),
        courses=CourseSettings(sessions_root=tmp_path / "sessions"),
    )
    orchestrator = create_app(settings=settings).state.orchestrator
    orchestrator.display_scene.force("game")

    async def perceive_other(*, non_game_surface=False):
        await orchestrator._handle_perception(
            {
                "type": "perception.completed",
                "text": json.dumps(
                    {
                        "scene": "other",
                        "confidence": 0.95,
                        "scene_evidence": {
                            "non_game_surface": non_game_surface,
                            "ordinary_browsing": False,
                        },
                        "observation": "" if not non_game_surface else "文件管理器",
                    },
                    ensure_ascii=False,
                ),
            }
        )
        return orchestrator.events.history("perception.completed")[-1].payload

    for _ in range(3):
        result = await perceive_other()
        assert result["scene"] == "game"
        assert result["uncertain_game_exit"] is True
    assert (await perceive_other())["scene"] == "other"

    orchestrator.display_scene.force("game")
    assert (await perceive_other(non_game_surface=True))["scene"] == "game"
    result = await perceive_other(non_game_surface=True)
    assert result["scene"] == "other"
    assert result["uncertain_game_exit"] is False


async def test_auto_course_survives_transient_scene_misclassification(
    tmp_path, monkeypatch
):
    settings = Settings(
        memory=MemorySettings(root=tmp_path / "memory"),
        courses=CourseSettings(
            sessions_root=tmp_path / "sessions",
            output_root=tmp_path / "courses",
            exit_grace_seconds=30,
            exit_samples=4,
        ),
    )
    orchestrator = create_app(settings=settings).state.orchestrator
    clock = [0.0]
    monkeypatch.setattr(service_module.time, "monotonic", lambda: clock[0])

    async def perceive(scene, *, note=""):
        await orchestrator._handle_perception(
            {
                "type": "perception.completed",
                "text": json.dumps(
                    {
                        "scene": scene,
                        "confidence": 0.9,
                        "course_title": "高中物理：牛顿第二定律",
                        "course_note": note,
                    },
                    ensure_ascii=False,
                ),
            }
        )

    await perceive("course", note="牛顿第二定律说明合外力等于质量与加速度的乘积。")
    assert orchestrator._auto_course_id is None
    clock[0] = 1.0
    await perceive("course", note="牛顿第二定律说明合外力等于质量与加速度的乘积。")
    session_id = orchestrator._auto_course_id
    assert session_id is not None

    for now in (3.0, 6.0):
        clock[0] = now
        await perceive("other")
    assert orchestrator.courses.open(session_id).state.status == CourseStatus.RECORDING
    assert orchestrator.events.history("perception.completed")[-1].payload["scene"] == "other"

    clock[0] = 7.0
    await perceive(
        "course",
        note=(
            "牛顿第二定律说明合外力等于质量与加速度的乘积，即 F=ma；"
            "它表示力是改变物体运动状态的原因。"
        ),
    )
    state = orchestrator.courses.open(session_id).state
    assert orchestrator._auto_course_id == session_id
    assert state.summary == ""

    for now in (10.0, 20.0, 30.0):
        clock[0] = now
        await perceive("other")
    assert orchestrator.courses.open(session_id).state.status == CourseStatus.RECORDING
    assert orchestrator.events.history("perception.completed")[-1].payload["scene"] == "other"

    clock[0] = 40.0
    await perceive("other")
    assert orchestrator.courses.open(session_id).state.status == CourseStatus.COMPLETE
    assert len(orchestrator.courses.sessions()) == 1


async def test_course_transcript_drives_summary_keyframes_and_specific_interactions(
    tmp_path, monkeypatch
):
    settings = Settings(
        memory=MemorySettings(root=tmp_path / "memory"),
        courses=CourseSettings(
            sessions_root=tmp_path / "sessions",
            output_root=tmp_path / "courses",
        ),
    )
    orchestrator = create_app(settings=settings).state.orchestrator
    summary_prompts = []

    async def summarize(method, payload):
        assert method == "ask"
        summary_prompts.append(payload["text"])
        return {
            "text": (
                "### 课程概览\n本节课比较速度与加速度。\n\n"
                "### 核心内容\n"
                "- 速度描述位置随时间的变化率；加速度描述速度随时间的变化率。\n"
                "- 解题时应先区分研究对象及两个物理量的定义。"
            )
        }

    monkeypatch.setattr(orchestrator.native_client, "request", summarize)

    async def perceive(request_id, transcript, interaction, *, capture=False):
        await orchestrator._handle_perception(
            {
                "request_id": request_id,
                "text": json.dumps(
                    {
                        "scene": "course",
                        "confidence": 0.95,
                        "course_title": "运动学：速度与加速度",
                        "course_transcript": transcript,
                        "course_note": "",
                        "course_interaction": interaction,
                        "capture_keyframe": capture,
                        "keyframe_note": "速度与加速度的定义及对比公式。",
                    },
                    ensure_ascii=False,
                ),
            }
        )

    await perceive(
        1,
        "速度表示位置变化快慢，加速度表示速度变化快慢",
        "这课很枯燥，但基础很重要。",
    )
    await perceive(
        2,
        "加速度表示速度变化快慢，二者不要混淆",
        "这里要区分速度和加速度的定义对象。",
        capture=True,
    )

    session = orchestrator.courses.sessions()[0]
    transcript = session.transcript_path.read_text(encoding="utf-8")
    assert transcript.count("加速度表示速度变化快慢") == 1
    assert "二者不要混淆" in transcript
    assert session.state.summary == ""
    assert summary_prompts == []

    events = orchestrator.events.history()
    interactions = [
        event.payload["text"] for event in events if event.topic == "course.interaction"
    ]
    assert interactions == ["这里要区分速度和加速度的定义对象。"]
    keyframes = [event for event in events if event.topic == "course.keyframe.requested"]
    assert len(keyframes) == 1
    assert keyframes[0].payload["note"] == "速度与加速度的定义及对比公式。"

    state = await orchestrator.finish_course(session.state.id)
    assert state.status == CourseStatus.COMPLETE
    assert "位置随时间的变化率" in state.summary
    assert len(summary_prompts) == 1
    assert summary_prompts[0].startswith("[[JARVIS_TEXT_ONLY]]")
    assert "速度表示位置变化快慢" in summary_prompts[0]
    assert "二者不要混淆" in summary_prompts[0]


async def test_recording_course_resumes_after_backend_restart(tmp_path):
    settings = Settings(
        memory=MemorySettings(root=tmp_path / "memory"),
        courses=CourseSettings(
            sessions_root=tmp_path / "sessions",
            output_root=tmp_path / "courses",
        ),
    )
    first = create_app(settings=settings).state.orchestrator
    initial = {
        "text": json.dumps(
            {
                "scene": "course",
                "confidence": 0.95,
                "course_title": "高等数学：映射",
                "course_transcript": "映射要求定义域中的每个元素有唯一对应。",
            },
            ensure_ascii=False,
        )
    }
    await first._handle_perception(initial)
    await first._handle_perception(initial)
    session_id = first._auto_course_id
    assert session_id is not None

    restarted = create_app(settings=settings).state.orchestrator
    assert restarted._auto_course_id == session_id
    resumed = {
        "text": json.dumps(
            {
                "scene": "course",
                "confidence": 0.95,
                "course_transcript": "陪域中的元素可以没有原像。",
            },
            ensure_ascii=False,
        )
    }
    await restarted._handle_perception(resumed)
    await restarted._handle_perception(resumed)

    sessions = restarted.courses.sessions()
    assert len(sessions) == 1
    transcript = sessions[0].transcript_path.read_text(encoding="utf-8")
    assert "每个元素有唯一对应" in transcript
    assert "可以没有原像" in transcript


def test_interaction_frequency_blocks_repeated_barrage_during_dedup_window(tmp_path):
    with make_client(tmp_path) as client:
        native = client.app.state.orchestrator.native_client
        client.app.state.orchestrator.display_scene.force("game")

        def emit(request_id, scene, barrage="", message=""):
            client.portal.call(
                native.emit,
                {
                    "type": "perception.completed",
                    "request_id": request_id,
                    "text": (
                        f'{{"scene":"{scene}","confidence":0.9,'
                        f'"barrage":"{barrage}","assistant_message":"{message}"}}'
                    ),
                },
            )

        emit(1, "other", message="这一步已经理顺，接下来可以直接验证核心结果。")
        emit(2, "game", barrage="漂亮操作！")
        time.sleep(0.03)
        emit(3, "other", message="核心结果还没验证，先别急着扩展到旁支。")
        emit(4, "game", barrage="漂亮操作！")
        time.sleep(0.03)
        repeated_text, repeated_at = client.app.state.orchestrator._recent_barrages[0]
        client.app.state.orchestrator._recent_barrages[0] = (
            repeated_text,
            repeated_at - 90.0,
        )
        emit(5, "game", barrage="漂亮操作！")
        time.sleep(0.03)
        events = client.get("/api/v1/events").json()
        bubbles = [event for event in events if event["topic"] == "assistant.message"]
        barrages = [event for event in events if event["topic"] == "barrage.generated"]
        assert bubbles == []
        assert [event["payload"]["text"] for event in barrages] == [
            "漂亮操作！",
            "漂亮操作！",
        ]


async def test_game_barrage_candidates_are_emitted_as_a_spaced_sequence(tmp_path):
    settings = Settings(
        interaction=InteractionSettings(game_barrage_interval_seconds=0.01),
        memory=MemorySettings(root=tmp_path / "memory"),
        courses=CourseSettings(sessions_root=tmp_path / "sessions"),
    )
    orchestrator = create_app(settings=settings).state.orchestrator
    orchestrator.display_scene.force("game")

    await orchestrator._handle_perception(
        {
            "type": "perception.completed",
            "text": json.dumps(
                {
                    "scene": "game",
                    "confidence": 0.95,
                    "scene_evidence": {"interactive_gameplay": True},
                    "observation": "玩家正在推进战线",
                    "barrage_candidates": [
                        "左侧敌人已经露头",
                        "补给足够继续推进",
                        "这波走位总算醒了",
                    ],
                },
                ensure_ascii=False,
            ),
        }
    )

    assert len(orchestrator.events.history("barrage.generated")) == 1
    await asyncio.sleep(0.06)
    assert [
        event.payload["text"]
        for event in orchestrator.events.history("barrage.generated")
    ] == ["左侧敌人已经露头", "补给足够继续推进", "这波走位总算醒了"]


async def test_leaving_game_cancels_queued_barrage_candidates(tmp_path):
    settings = Settings(
        interaction=InteractionSettings(game_barrage_interval_seconds=0.05),
        memory=MemorySettings(root=tmp_path / "memory"),
        courses=CourseSettings(sessions_root=tmp_path / "sessions"),
    )
    orchestrator = create_app(settings=settings).state.orchestrator
    orchestrator.display_scene.force("game")

    await orchestrator._handle_perception(
        {
            "type": "perception.completed",
            "text": json.dumps(
                {
                    "scene": "game",
                    "confidence": 0.95,
                    "scene_evidence": {"interactive_gameplay": True},
                    "observation": "玩家正在推进战线",
                    "barrage_candidates": ["先稳住左侧视野", "补给充足可以继续推进"],
                },
                ensure_ascii=False,
            ),
        }
    )
    await orchestrator._handle_perception(
        {
            "type": "perception.completed",
            "text": json.dumps(
                {
                    "scene": "other",
                    "confidence": 0.99,
                    "scene_evidence": {"non_game_surface": True},
                    "observation": "用户已经返回桌面",
                },
                ensure_ascii=False,
            ),
        }
    )
    await asyncio.sleep(0.07)

    assert orchestrator.display_scene.current == "other"
    assert [
        event.payload["text"]
        for event in orchestrator.events.history("barrage.generated")
    ] == ["先稳住左侧视野"]


def test_game_barrage_semantic_near_duplicates_are_suppressed(tmp_path):
    with make_client(tmp_path) as client:
        native = client.app.state.orchestrator.native_client
        client.app.state.orchestrator.display_scene.force("game")

        for request_id, barrage in enumerate(
            ["漂亮操作，完成反杀！", "反杀完成，这操作漂亮！", "资源够了，可以推进"],
            start=20,
        ):
            client.portal.call(
                native.emit,
                {
                    "type": "perception.completed",
                    "request_id": request_id,
                    "text": (
                        '{"scene":"game","confidence":1.0,'
                        f'"barrage":"{barrage}","assistant_message":""}}'
                    ),
                },
            )

        time.sleep(0.03)
        generated = [
            event["payload"]["text"]
            for event in client.get("/api/v1/events").json()
            if event["topic"] == "barrage.generated"
        ]
        assert generated == ["漂亮操作，完成反杀！", "资源够了，可以推进"]


def test_game_barrage_prefers_declarative_candidate_over_speculative_question(tmp_path):
    with make_client(tmp_path) as client:
        native = client.app.state.orchestrator.native_client
        client.app.state.orchestrator.display_scene.force("game")
        payload = {
            "scene": "game",
            "confidence": 1.0,
            "observation": "玩家站在草原，附近有羊群",
            "barrage": "头顶方块是惊喜还是陷阱？",
            "barrage_candidates": [
                "头顶方块是惊喜还是陷阱？",
                "羊群把这片草原承包了",
                "视野开阔，先把落脚点定下来",
            ],
            "assistant_message": "",
        }
        client.portal.call(
            native.emit,
            {
                "type": "perception.completed",
                "request_id": 25,
                "text": json.dumps(payload, ensure_ascii=False),
            },
        )

        time.sleep(0.03)
        generated = [
            event["payload"]["text"]
            for event in client.get("/api/v1/events").json()
            if event["topic"] == "barrage.generated"
        ]
        assert generated == ["羊群把这片草原承包了"]


def test_game_barrage_uses_fresh_alternative_when_primary_is_repeated(tmp_path):
    with make_client(tmp_path) as client:
        native = client.app.state.orchestrator.native_client
        client.app.state.orchestrator.display_scene.force("game")

        def emit(request_id, primary, candidates):
            client.portal.call(
                native.emit,
                {
                    "type": "perception.completed",
                    "request_id": request_id,
                    "text": json.dumps(
                        {
                            "scene": "game",
                            "confidence": 1.0,
                            "barrage": primary,
                            "barrage_candidates": candidates,
                            "assistant_message": "",
                        },
                        ensure_ascii=False,
                    ),
                },
            )

        emit(26, "草原这局面挺安稳", ["草原这局面挺安稳"])
        emit(
            27,
            "草原这局面挺安稳",
            ["草原这局面挺安稳", "羊群已经开始自由巡逻", "先沿高处看看地形"],
        )

        time.sleep(0.03)
        generated = [
            event["payload"]["text"]
            for event in client.get("/api/v1/events").json()
            if event["topic"] == "barrage.generated"
        ]
        assert generated == ["草原这局面挺安稳", "羊群已经开始自由巡逻"]


def test_game_barrage_near_duplicate_recovers_before_exact_repeat(tmp_path):
    with make_client(tmp_path) as client:
        native = client.app.state.orchestrator.native_client
        orchestrator = client.app.state.orchestrator
        orchestrator.display_scene.force("game")

        def emit(request_id, barrage):
            client.portal.call(
                native.emit,
                {
                    "type": "perception.completed",
                    "request_id": request_id,
                    "text": (
                        '{"scene":"game","confidence":1.0,'
                        f'"barrage":"{barrage}","assistant_message":""}}'
                    ),
                },
            )

        emit(30, "漂亮操作，完成反杀！")
        time.sleep(0.02)
        original, emitted_at = orchestrator._recent_barrages[0]
        orchestrator._recent_barrages[0] = (original, emitted_at - 31.0)
        emit(31, "反杀完成，这操作漂亮！")
        emit(32, "漂亮操作，完成反杀！")

        time.sleep(0.03)
        generated = [
            event["payload"]["text"]
            for event in client.get("/api/v1/events").json()
            if event["topic"] == "barrage.generated"
        ]
        assert generated == ["漂亮操作，完成反杀！", "反杀完成，这操作漂亮！"]


def test_structured_perception_emits_grounded_ordinary_assistant_message(tmp_path):
    with make_client(tmp_path) as client:
        native = client.app.state.orchestrator.native_client
        client.portal.call(
            native.emit,
            {
                "type": "perception.completed",
                "request_id": 10,
                "text": json.dumps(
                    {
                        "scene": "other",
                        "confidence": 0.9,
                        "assistant_message": "这段说明把关键边界交代得很清楚。",
                        "assistant_candidates": ["搜索结束，准备认真读了。"],
                    },
                    ensure_ascii=False,
                ),
            },
        )
        time.sleep(0.03)

        messages = [
            event["payload"]
            for event in client.get("/api/v1/events").json()
            if event["topic"] == "assistant.message"
        ]
        assert messages == [
            {
                "text": "这段说明把关键边界交代得很清楚。",
                "source": "perception",
                "confidence": 0.9,
            }
        ]


def test_ambient_duplex_only_owns_temporal_video_commentary():
    instruction = service_module.AMBIENT_DUPLEX_INSTRUCTION

    assert "唯一职责" in instruction
    assert "桌面、静态网页、游戏和课程由结构化感知处理，一律 LISTEN" in instruction
    assert "至少有两项一致锚点" in instruction
    assert "标题、封面、播放器控件或孤立字幕不能单独证明" in instruction
    assert "视频在播什么" in instruction
    assert "而没有表达判断、态度或调侃，就必须 LISTEN" in instruction
    assert len(instruction) < 500


@pytest.mark.asyncio
async def test_ordinary_assistant_message_uses_sixteen_second_cooldown(tmp_path):
    settings = Settings(
        memory=MemorySettings(root=tmp_path / "memory"),
        courses=CourseSettings(sessions_root=tmp_path / "sessions"),
    )
    orchestrator = create_app(settings=settings).state.orchestrator
    assert settings.interaction.ordinary_bubble_cooldown_seconds == 16.0

    await orchestrator._emit_ordinary_perception_message(
        {"assistant_message": "下载任务已经完成，文件可以直接使用。", "confidence": 0.9},
        100.0,
    )
    await orchestrator._emit_ordinary_perception_message(
        {"assistant_message": "文章切到新章节，关键定义值得留意。", "confidence": 0.9},
        115.9,
    )
    await orchestrator._emit_ordinary_perception_message(
        {"assistant_message": "视频转入实测环节，先看数据变化。", "confidence": 0.9},
        116.0,
    )

    assert [
        event.payload["text"]
        for event in orchestrator.events.history("assistant.message")
    ] == [
        "下载任务已经完成，文件可以直接使用。",
        "视频转入实测环节，先看数据变化。",
    ]


def test_monitoring_automatically_manages_ambient_duplex(tmp_path):
    with make_client(tmp_path) as client:
        native = client.app.state.orchestrator.native_client
        requests = []
        original_request = native.request

        async def record_request(method, payload):
            requests.append((method, payload))
            return await original_request(method, payload)

        native.request = record_request

        started = client.post(
            "/api/v1/commands",
            json={"command": "start_monitoring", "arguments": {}},
        )
        assert started.status_code == 200
        deadline = time.monotonic() + 1.0
        while not client.get("/api/v1/duplex").json()["active"]:
            assert time.monotonic() < deadline
            time.sleep(0.01)
        assert [method for method, _ in requests[-2:]] == [
            "start_monitoring",
            "start_duplex",
        ]
        assert requests[-1][1]["_timeout_seconds"] == 600.0
        status = client.get("/api/v1/duplex").json()
        assert status["active"] is True
        assert status["session_id"] == "jarvis-ambient"
        assert "理解普通场景中正在播放的视频或直播" in status["instruction"]

        paused = client.post(
            "/api/v1/commands",
            json={"command": "pause_monitoring", "arguments": {}},
        )
        assert paused.status_code == 200
        assert requests[-1][0] == "pause_monitoring"
        assert client.get("/api/v1/duplex").json()["active"] is False

        resumed = client.post(
            "/api/v1/commands",
            json={"command": "resume_monitoring", "arguments": {}},
        )
        assert resumed.status_code == 200
        deadline = time.monotonic() + 1.0
        while not client.get("/api/v1/duplex").json()["active"]:
            assert time.monotonic() < deadline
            time.sleep(0.01)
        assert [method for method, _ in requests[-2:]] == [
            "resume_monitoring",
            "start_duplex",
        ]


async def test_pet_chat_pauses_and_resumes_ambient_duplex(tmp_path, monkeypatch):
    settings = Settings(
        memory=MemorySettings(root=tmp_path / "memory"),
        courses=CourseSettings(sessions_root=tmp_path / "sessions"),
    )
    orchestrator = create_app(settings=settings).state.orchestrator
    await orchestrator.start()
    orchestrator._monitoring_requested = True
    orchestrator._duplex_session_id = "jarvis-ambient"
    orchestrator._duplex_instruction = service_module.AMBIENT_DUPLEX_INSTRUCTION
    requests = []
    resume_entered = asyncio.Event()
    resume_release = asyncio.Event()

    async def request(method, payload):
        requests.append((method, payload))
        if method == "ask":
            return {"text": "GPU reply"}
        if method == "start_duplex":
            resume_entered.set()
            await resume_release.wait()
        return {"ok": True}

    monkeypatch.setattr(orchestrator.native_client, "request", request)
    try:
        assert await orchestrator.pet_chat("hello") == "GPU reply"
        await asyncio.wait_for(resume_entered.wait(), timeout=0.2)
        assert [method for method, _ in requests[:3]] == [
            "stop_duplex",
            "ask",
            "start_duplex",
        ]
        assert requests[1][1]["_timeout_seconds"] == 600.0
        resume_task = orchestrator._ambient_duplex_task
        assert resume_task is not None
        resume_release.set()
        await asyncio.wait_for(resume_task, timeout=0.2)
        assert orchestrator.duplex_status()["session_id"] == "jarvis-ambient"
    finally:
        resume_release.set()
        await orchestrator.stop()


async def test_pet_chat_does_not_resume_ambient_after_monitoring_stops(
    tmp_path, monkeypatch
):
    settings = Settings(
        memory=MemorySettings(root=tmp_path / "memory"),
        courses=CourseSettings(sessions_root=tmp_path / "sessions"),
    )
    orchestrator = create_app(settings=settings).state.orchestrator
    await orchestrator.start()
    orchestrator._monitoring_requested = True
    orchestrator._duplex_session_id = "jarvis-ambient"
    orchestrator._duplex_instruction = service_module.AMBIENT_DUPLEX_INSTRUCTION
    requests = []
    ask_entered = asyncio.Event()
    ask_release = asyncio.Event()

    async def request(method, payload):
        requests.append((method, payload))
        if method == "ask":
            ask_entered.set()
            await ask_release.wait()
            return {"text": "reply after stop"}
        return {"ok": True}

    monkeypatch.setattr(orchestrator.native_client, "request", request)
    try:
        chat_task = asyncio.create_task(orchestrator.pet_chat("hello"))
        await asyncio.wait_for(ask_entered.wait(), timeout=0.2)
        orchestrator._monitoring_requested = False
        ask_release.set()
        assert await asyncio.wait_for(chat_task, timeout=0.2) == "reply after stop"
        await asyncio.sleep(0)
        assert [method for method, _ in requests] == ["stop_duplex", "ask"]
        assert orchestrator._ambient_duplex_task is None
    finally:
        ask_release.set()
        await orchestrator.stop()


async def test_monitoring_returns_before_ambient_duplex_is_ready(tmp_path, monkeypatch):
    settings = Settings(
        memory=MemorySettings(root=tmp_path / "memory"),
        courses=CourseSettings(sessions_root=tmp_path / "sessions"),
    )
    orchestrator = create_app(settings=settings).state.orchestrator
    await orchestrator.start()
    entered = asyncio.Event()
    release = asyncio.Event()
    original_request = orchestrator.native_client.request

    async def delayed_duplex(method, payload):
        if method == "start_duplex":
            entered.set()
            await release.wait()
        return await original_request(method, payload)

    monkeypatch.setattr(orchestrator.native_client, "request", delayed_duplex)
    try:
        result = await asyncio.wait_for(
            orchestrator.command("start_monitoring", {}), timeout=0.2
        )
        assert result["method"] == "start_monitoring"
        await asyncio.wait_for(entered.wait(), timeout=0.2)
        assert orchestrator.duplex_status()["active"] is False
        assert orchestrator.events.history("duplex.task.initializing")

        task = orchestrator._ambient_duplex_task
        assert task is not None
        release.set()
        await asyncio.wait_for(task, timeout=0.2)

        assert orchestrator.duplex_status()["active"] is True
        assert orchestrator.events.history("duplex.task.started")
    finally:
        release.set()
        await orchestrator.stop()


async def test_ambient_duplex_failure_stops_monitoring(tmp_path, monkeypatch):
    settings = Settings(
        memory=MemorySettings(root=tmp_path / "memory"),
        courses=CourseSettings(sessions_root=tmp_path / "sessions"),
    )
    orchestrator = create_app(settings=settings).state.orchestrator
    await orchestrator.start()
    entered = asyncio.Event()
    fail = asyncio.Event()
    requests = []
    original_request = orchestrator.native_client.request

    async def failing_duplex(method, payload):
        requests.append(method)
        if method == "start_duplex":
            entered.set()
            await fail.wait()
            raise RuntimeError("not enough GPU memory")
        return await original_request(method, payload)

    monkeypatch.setattr(orchestrator.native_client, "request", failing_duplex)
    try:
        await orchestrator.command("start_monitoring", {})
        await asyncio.wait_for(entered.wait(), timeout=0.2)
        task = orchestrator._ambient_duplex_task
        assert task is not None
        fail.set()
        await asyncio.wait_for(task, timeout=0.2)

        failures = orchestrator.events.history("duplex.task.failed")
        assert failures[-1].payload["error"] == (
            "环境感知模型初始化失败，请确认可用内存和显存充足后重试"
        )
        assert requests[-1] == "stop_monitoring"
        assert orchestrator.duplex_status()["active"] is False
    finally:
        fail.set()
        await orchestrator.stop()


@pytest.mark.parametrize("stop_command", ["pause_monitoring", "stop_monitoring"])
async def test_stopping_monitoring_cancels_pending_ambient_duplex(
    tmp_path, monkeypatch, stop_command
):
    settings = Settings(
        memory=MemorySettings(root=tmp_path / "memory"),
        courses=CourseSettings(sessions_root=tmp_path / "sessions"),
    )
    orchestrator = create_app(settings=settings).state.orchestrator
    await orchestrator.start()
    entered = asyncio.Event()
    never_ready = asyncio.Event()
    requests = []
    original_request = orchestrator.native_client.request

    async def delayed_duplex(method, payload):
        requests.append(method)
        if method == "start_duplex":
            entered.set()
            await never_ready.wait()
        return await original_request(method, payload)

    monkeypatch.setattr(orchestrator.native_client, "request", delayed_duplex)
    try:
        await orchestrator.command("start_monitoring", {})
        await asyncio.wait_for(entered.wait(), timeout=0.2)

        await asyncio.wait_for(orchestrator.command(stop_command, {}), timeout=0.2)

        assert orchestrator._ambient_duplex_task is None
        assert requests[-1] == stop_command
        assert orchestrator.duplex_status()["active"] is False
        assert not orchestrator.events.history("duplex.task.started")
    finally:
        never_ready.set()
        await orchestrator.stop()


def test_duplex_model_speak_routing_and_compatibility_api(tmp_path):
    with make_client(tmp_path) as client:
        native = client.app.state.orchestrator.native_client

        assert client.get("/api/v1/duplex").json() == {
            "active": False,
            "session_id": None,
            "instruction": "",
        }
        started = client.post(
            "/api/v1/duplex",
            json={
                "session_id": "traffic-light",
                "instruction": "持续观察画面，绿灯亮起时提醒我。",
            },
        )
        assert started.status_code == 200
        assert started.json() == {
            "active": True,
            "session_id": "traffic-light",
            "instruction": "持续观察画面，绿灯亮起时提醒我。",
        }

        client.portal.call(
            native.emit,
            {
                "type": "duplex.decision",
                "session_id": "traffic-light",
                "sequence": 1,
                "ok": True,
                "decision": "listen",
                "text": "",
            },
        )
        client.portal.call(
            native.emit,
            {
                "type": "duplex.decision",
                "session_id": "traffic-light",
                "sequence": 2,
                "ok": True,
                "decision": "speak",
                "text": "绿灯亮了，可以通行。",
            },
        )
        client.portal.call(
            native.emit,
            {
                "type": "duplex.decision",
                "session_id": "traffic-light",
                "sequence": 3,
                "ok": True,
                "decision": "speak",
                "text": "绿灯亮了，可以通行。",
            },
        )
        time.sleep(0.03)

        events = client.get("/api/v1/events").json()
        decisions = [event for event in events if event["topic"] == "duplex.decision"]
        messages = [
            event["payload"]
            for event in events
            if event["topic"] == "assistant.message"
            and event["payload"].get("source") == "duplex"
        ]
        assert [event["payload"]["decision"] for event in decisions] == [
            "listen",
            "speak",
            "speak",
        ]
        assert messages == [
            {
                "text": "绿灯亮了，可以通行。",
                "source": "duplex",
                "session_id": "traffic-light",
            }
        ]

        stopped = client.delete("/api/v1/duplex")
        assert stopped.status_code == 200
        assert stopped.json() == {
            "active": False,
            "session_id": None,
            "instruction": "",
        }


def test_screen_idle_reminders_bypass_model_until_screen_changes(tmp_path, monkeypatch):
    monkeypatch.setattr(service_module.random, "choice", lambda values: values[0])
    with make_client(tmp_path) as client:
        native = client.app.state.orchestrator.native_client

        client.portal.call(
            native.emit,
            {"type": "screen.idle", "idle_seconds": 120},
        )
        client.portal.call(
            native.emit,
            {
                "type": "duplex.decision",
                "session_id": "jarvis-ambient",
                "sequence": 1,
                "ok": True,
                "decision": "speak",
                "text": "这条模型回复不应显示。",
            },
        )
        client.portal.call(
            native.emit,
            {"type": "screen.idle.reminder", "idle_seconds": 180, "sequence": 1},
        )
        time.sleep(0.03)

        messages = [
            event["payload"]
            for event in client.get("/api/v1/events").json()
            if event["topic"] == "assistant.message"
        ]
        assert messages == [{"text": "是在摸鱼吗？", "source": "screen_idle"}]

        client.portal.call(native.emit, {"type": "screen.active", "idle_seconds": 0})
        client.portal.call(
            native.emit,
            {
                "type": "duplex.decision",
                "session_id": "jarvis-ambient",
                "sequence": 2,
                "ok": True,
                "decision": "speak",
                "text": "下载完成了，文件可以直接用了。",
            },
        )
        time.sleep(0.03)

        messages = [
            event["payload"]
            for event in client.get("/api/v1/events").json()
            if event["topic"] == "assistant.message"
        ]
        assert messages[-1] == {
            "text": "下载完成了，文件可以直接用了。",
            "source": "duplex",
            "session_id": "jarvis-ambient",
        }


def test_duplex_task_rejects_empty_instruction(tmp_path):
    with make_client(tmp_path) as client:
        response = client.post("/api/v1/duplex", json={"instruction": "   "})
        control = client.post("/api/v1/duplex", json={"instruction": "watch\u0000now"})

        assert response.status_code == 422
        assert control.status_code == 422


def test_perception_keeps_internal_observation_separate_from_bubble_text():
    result = create_app().state.orchestrator._parse_perception(
        '{"scene":"other","confidence":0.9,'
        '"observation":"编辑器中打开了代码文件",'
        '"assistant_message":"又和代码较上劲了？"}'
    )

    assert result["observation"] == "编辑器中打开了代码文件"
    assert result["assistant_message"] == "又和代码较上劲了？"


@pytest.mark.parametrize(
    ("payload", "expected_scene"),
    [
        (
            {
                "scene": "game",
                "confidence": 0.99,
                "scene_evidence": {
                    "game_surface": True,
                    "interactive_gameplay": False,
                    "game_video_or_stream": True,
                    "fullscreen_game_media": False,
                },
            },
            "other",
        ),
        (
            {
                "scene": "game",
                "confidence": 0.74,
                "scene_evidence": {
                    "game_surface": True,
                    "interactive_gameplay": False,
                    "game_video_or_stream": True,
                    "fullscreen_game_media": True,
                },
                "barrage_candidates": ["这段操作节奏很紧凑"],
            },
            "game",
        ),
        (
            {
                "scene": "game",
                "confidence": 0.99,
                "scene_evidence": {
                    "game_surface": True,
                    "interactive_gameplay": True,
                    "game_video_or_stream": False,
                    "fullscreen_game_media": False,
                },
                "barrage_candidates": ["资源充足，可以推进"],
            },
            "game",
        ),
        (
            {
                "scene": "other",
                "confidence": 0.96,
                "scene_evidence": {
                    "game_surface": True,
                    "interactive_gameplay": False,
                    "game_video_or_stream": False,
                    "fullscreen_game_media": False,
                    "ordinary_browsing": False,
                },
                "observation": "回合结束画面仍显示比分板、小地图和游戏 HUD",
            },
            "other",
        ),
        (
            {
                "scene": "course",
                "confidence": 0.95,
                "scene_evidence": {
                    "active_instruction": True,
                    "course_surface": True,
                    "ordinary_browsing": True,
                },
                "course_note": "网页中出现课程二字，但主体仍是普通浏览。",
            },
            "other",
        ),
        (
            {
                "scene": "course",
                "confidence": 0.95,
                "scene_evidence": {
                    "active_instruction": True,
                    "course_surface": True,
                    "ordinary_browsing": False,
                },
                "course_note": "讲师正在推导牛顿第二定律。",
            },
            "course",
        ),
        (
            {
                "scene": "course",
                "confidence": 0.95,
                "scene_evidence": {
                    "active_instruction": False,
                    "course_surface": True,
                    "instructional_audio": True,
                    "ordinary_browsing": True,
                },
                "course_transcript": "梯度表示函数在该点增长最快的方向。",
                "course_note": "浏览器中的课件正在展示梯度公式。",
            },
            "course",
        ),
        (
            {
                "scene": "course",
                "confidence": 0.95,
                "scene_evidence": {
                    "active_instruction": False,
                    "course_surface": True,
                    "instructional_audio": False,
                    "ordinary_browsing": False,
                },
                "course_note": "画面只有一页静态课堂笔记。",
            },
            "other",
        ),
    ],
)
def test_scene_evidence_validates_gameplay_and_multimodal_courses(
    payload, expected_scene
):
    result = create_app().state.orchestrator._parse_perception(
        json.dumps(payload, ensure_ascii=False)
    )

    assert result["scene"] == expected_scene


def test_legacy_ordinary_candidates_are_discarded(tmp_path):
    with make_client(tmp_path) as client:
        native = client.app.state.orchestrator.native_client
        client.portal.call(
            native.emit,
            {
                "type": "perception.completed",
                "request_id": 88,
                "text": json.dumps(
                    {
                        "scene": "other",
                        "assistant_candidates": [
                            "您正在观看一段舞蹈视频。",
                            "需要我帮忙找类似视频吗？",
                            "表演开场，主人请就位！",
                        ],
                        "assistant_message": "",
                    },
                    ensure_ascii=False,
                ),
            },
        )
        time.sleep(0.03)
        messages = [
            event["payload"]["text"]
            for event in client.get("/api/v1/events").json()
            if event["topic"] == "assistant.message"
        ]
        assert messages == []


def test_duplex_messages_reject_narration_offers_and_uncertainty(tmp_path):
    with make_client(tmp_path) as client:
        orchestrator = client.app.state.orchestrator
        assert orchestrator._clean_duplex_message("你正在浏览项目文件。") == ""
        assert orchestrator._clean_duplex_message("需要我帮你整理文件吗？") == ""
        assert orchestrator._clean_duplex_message("看起来构建可能失败了。") == ""
        assert orchestrator._clean_duplex_message("已经进入“概览”页，开始查看") == ""
        assert orchestrator._clean_duplex_message("应用信息了。") == ""
        assert orchestrator._clean_duplex_message(
            "光标在代码里移动，准备继续往下看。", require_proactive_value=False
        ) == ""
        assert orchestrator._clean_duplex_message(
            "光标在打开多个程序的快捷方式。", require_proactive_value=False
        ) == ""
        assert orchestrator._clean_duplex_message(
            "视频开始了，讲解的是示例课程。", require_proactive_value=False
        ) == ""
        assert orchestrator._clean_duplex_message(
            "新闻页面介绍的是示例主题。", require_proactive_value=False
        ) == ""
        assert orchestrator._clean_duplex_message(
            "好的，正在播放一部关于电影《美丽风景》的介绍视频。",
            require_proactive_value=False,
        ) == ""
        assert orchestrator._clean_duplex_message(
            "当前浏览bilibili视频评论区，页面显示多条评论和相关信息。",
            require_proactive_value=False,
        ) == ""
        assert orchestrator._clean_duplex_message(
            "评论区这火药味，比视频正片还足。",
            require_proactive_value=False,
        ) == "评论区这火药味，比视频正片还足。"
        assert orchestrator._clean_duplex_message(
            "同一个报错看第三遍也不会自己消失，先看第一条堆栈。",
            require_proactive_value=False,
        ) == "同一个报错看第三遍也不会自己消失，先看第一条堆栈。"
        assert orchestrator._clean_duplex_message("下载完成了，文件可以直接用了。") == (
            "下载完成了，文件可以直接用了。"
        )
        assert orchestrator._clean_duplex_message(
            "文档中有端口和连接信息，建议仔细核对。"
        ) == "文档中有端口和连接信息，建议仔细核对。"
        assert orchestrator._clean_duplex_message(
            "构建失败：链接器找不到入口符号。后续所有改动等完整验证后再决定。"
        ) == "构建失败：链接器找不到入口符号。"


def test_ambient_duplex_assembles_adjacent_fragments_before_filtering(tmp_path):
    with make_client(tmp_path) as client:
        orchestrator = client.app.state.orchestrator
        assert orchestrator._assemble_duplex_message(
            "下载已经完成，文件可以直接", "jarvis-ambient", 10.0
        ) == ""
        assert orchestrator._assemble_duplex_message(
            "使用了。", "jarvis-ambient", 11.0
        ) == "下载已经完成，文件可以直接使用了。"
        assert orchestrator._assemble_duplex_message(
            'frame":"显示桌面与代码窗口', "jarvis-ambient", 12.0
        ) == ""
        assert orchestrator._assemble_duplex_message(
            '相关选项。","course_transcript":""', "jarvis-ambient", 13.0
        ) == ""
        assert orchestrator._assemble_duplex_message(
            'course":false,"instructional_audio":false', "jarvis-ambient", 14.0
        ) == ""
        assert orchestrator._assemble_duplex_message(
            "和片段，涉及项目提交", "jarvis-ambient", 15.0
        ) == ""
        assert orchestrator._assemble_duplex_message(
            "文档吧。", "jarvis-ambient", 16.0
        ) == ""
        assert orchestrator._assemble_duplex_message(
            "）、夹和游戏截图，整体为典型桌面", "jarvis-ambient", 20.0
        ) == ""
        assert orchestrator._assemble_duplex_message(
            "一样，找文件得先理清思路。", "jarvis-ambient", 21.0
        ) == ""
        assert orchestrator._assemble_duplex_message(
            "先把同类文件归到一起，找东西会省不少时间。", "jarvis-ambient", 25.0
        ) == "先把同类文件归到一起，找东西会省不少时间。"


def test_truncated_perception_recovers_complete_scene_evidence(tmp_path):
    with make_client(tmp_path) as client:
        result = client.app.state.orchestrator._parse_perception(
            '{"scene":"game","confidence":0.93,"scene_evidence":'
            '{"game_surface":true,"interactive_gameplay":true,'
            '"game_video_or_stream":false,'
            '"fullscreen_game_media":false,'
            '"active_instruction":false,"course_surface":false,'
            '"instructional_audio":false,"ordinary_browsing":false},'
            '"observation":"玩家正在操控角色移动","course_transcript":"'
        )
        assert result["scene"] == "game"
        assert result["confidence"] == 0.93
        assert result["observation"] == "玩家正在操控角色移动"


def test_truncated_perception_rejects_incomplete_game_evidence(tmp_path):
    with make_client(tmp_path) as client:
        with pytest.raises(json.JSONDecodeError, match="incomplete scene evidence"):
            client.app.state.orchestrator._parse_perception(
                '{"scene":"game","confidence":0.93,"scene_evidence":'
                '{"interactive_gameplay":true'
            )


def test_ambient_duplex_rejects_routine_fragments_and_accepts_grounded_comment(tmp_path):
    with make_client(tmp_path) as client:
        native = client.app.state.orchestrator.native_client

        for sequence, text in enumerate(
            [
                "已经进入“概览”页，开始查看",
                "应用信息了。",
                "新闻页面介绍的是示例主题。",
                "这段实现把状态切换和后端命令分开了，交互会顺手很多。",
                "这里的端口配置值得先核对连接目标。",
                "构建失败：链接器找不到入口符号。",
            ],
            start=1,
        ):
            client.portal.call(
                native.emit,
                {
                    "type": "duplex.decision",
                    "session_id": "jarvis-ambient",
                    "sequence": sequence,
                    "ok": True,
                    "decision": "speak",
                    "text": text,
                },
            )

        messages = [
            event["payload"]
            for event in client.get("/api/v1/events").json()
            if event["topic"] == "assistant.message"
        ]
        assert messages == [
            {
                "text": "这段实现把状态切换和后端命令分开了，交互会顺手很多。",
                "source": "duplex",
                "session_id": "jarvis-ambient",
            },
            {
                "text": "这里的端口配置值得先核对连接目标。",
                "source": "duplex",
                "session_id": "jarvis-ambient",
            },
            {
                "text": "构建失败：链接器找不到入口符号。",
                "source": "duplex",
                "session_id": "jarvis-ambient",
            }
        ]


def test_course_interactions_are_low_frequency_and_process_notes_are_discarded(tmp_path):
    with make_client(tmp_path) as client:
        native = client.app.state.orchestrator.native_client

        def emit(request_id, note, interaction):
            client.portal.call(
                native.emit,
                {
                    "type": "perception.completed",
                    "request_id": request_id,
                    "text": (
                        '{"scene":"course","confidence":0.9,"course_title":"Physics",'
                        f'"course_note":"{note}","course_interaction":"{interaction}"}}'
                    ),
                },
            )

        emit(100, "The screen shows a folder of course files.", "Notice why mass matters here.")
        emit(101, "Force equals mass multiplied by acceleration.", "Connect this to inertia.")
        time.sleep(0.03)

        courses = client.get("/api/v1/courses").json()
        assert len(courses) == 1
        assert courses[0]["summary"] == ""
        events = client.get("/api/v1/events").json()
        interactions = [event for event in events if event["topic"] == "course.interaction"]
        assert [event["payload"]["text"] for event in interactions] == [
            "Connect this to inertia."
        ]

        client.app.state.orchestrator._last_course_interaction_at -= 90.0
        emit(
            102, "Acceleration describes how quickly velocity changes.", "Now link force to motion."
        )
        time.sleep(0.03)
        events = client.get("/api/v1/events").json()
        interactions = [event for event in events if event["topic"] == "course.interaction"]
        assert [event["payload"]["text"] for event in interactions] == [
            "Connect this to inertia.",
            "Now link force to motion.",
        ]

        client.app.state.orchestrator._last_course_interaction_at -= 30.0
        emit(103, "Velocity is displacement divided by elapsed time.", "")
        time.sleep(0.03)
        interactions = [
            event
            for event in client.get("/api/v1/events").json()
            if event["topic"] == "course.interaction"
        ]
        assert interactions[-1]["payload"]["text"] == (
            "Velocity is displacement divided by elapsed time。"
        )


def test_game_advice_falls_back_to_barrage_when_model_uses_wrong_field(tmp_path):
    with make_client(tmp_path) as client:
        native = client.app.state.orchestrator.native_client
        client.app.state.orchestrator.display_scene.force("game")
        client.portal.call(
            native.emit,
            {
                "type": "perception.completed",
                "request_id": 99,
                "text": (
                    '{"scene":"game","confidence":1.0,"barrage":"",'
                    '"course_note":"","assistant_message":"先补向日葵，经济别断"}'
                ),
            },
        )
        time.sleep(0.02)
        events = client.get("/api/v1/events").json()
        generated = [event for event in events if event["topic"] == "barrage.generated"]
        assert generated[-1]["payload"]["text"] == "先补向日葵，经济别断"


def test_confirmed_game_without_native_generation_stays_silent(tmp_path):
    with make_client(tmp_path) as client:
        native = client.app.state.orchestrator.native_client
        client.app.state.orchestrator.display_scene.force("game")
        client.portal.call(
            native.emit,
            {
                "type": "perception.completed",
                "request_id": 100,
                "text": json.dumps(
                    {
                        "scene": "game",
                        "confidence": 0.95,
                        "scene_evidence": {
                            "interactive_gameplay": True,
                            "game_video_or_stream": False,
                            "fullscreen_game_media": False,
                        },
                        "observation": "玩家向前移动并探索新的区域",
                        "barrage_candidates": [],
                    },
                    ensure_ascii=False,
                ),
            },
        )

        time.sleep(0.02)
        generated = [
            event["payload"]["text"]
            for event in client.get("/api/v1/events").json()
            if event["topic"] == "barrage.generated"
        ]
        assert generated == []
        completed = next(
            event
            for event in reversed(client.get("/api/v1/events").json())
            if event["topic"] == "perception.completed"
        )
        assert completed["payload"]["barrage_source"] == "missing_generation"
        assert completed["payload"]["barrage_fallback_reason"] == "empty_candidates"

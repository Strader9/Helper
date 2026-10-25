from datetime import UTC, datetime, timedelta

import pytest

from jarvis_backend.barrage import BarrageDecision, BarrageItem, BarragePolicy
from jarvis_backend.orchestrator.scene import CourseSceneStabilizer, SceneHysteresis


def test_scene_hysteresis_debounces_entry_and_exit() -> None:
    scene = SceneHysteresis(0.7, 0.4, enter_samples=2, exit_samples=2)
    assert scene.observe(0.8) is None
    assert scene.observe(0.8).active is True
    assert scene.observe(0.5) is None
    assert scene.observe(0.3) is None
    assert scene.observe(0.3).active is False


def test_course_scene_stabilizer_ignores_brief_misclassification() -> None:
    scene = CourseSceneStabilizer(enter_samples=2, exit_samples=3)
    assert scene.observe("course") == "other"
    assert scene.observe("course") == "course"
    assert scene.observe("other") == "course"
    assert scene.observe("other") == "course"
    assert scene.observe("course") == "course"
    assert scene.observe("game") == "course"
    assert scene.observe("other") == "course"
    assert scene.observe("game") == "course"
    assert scene.observe("game") == "course"
    assert scene.observe("game") == "game"


def test_default_course_scene_stabilizer_requires_two_consistent_samples() -> None:
    scene = CourseSceneStabilizer()

    assert scene.observe("course") == "other"
    assert scene.observe("course") == "course"


def test_course_scene_stabilizer_rejects_zero_game_entry_samples() -> None:
    with pytest.raises(ValueError, match="sample counts must be positive"):
        CourseSceneStabilizer(game_enter_samples=0)


def test_barrage_rejects_stale_and_duplicate_items() -> None:
    policy = BarragePolicy(max_age_seconds=5, max_queue_size=2)
    now = datetime.now(UTC)
    stale = BarrageItem("old", "old", now - timedelta(seconds=6))
    fresh = BarrageItem("new", "hello", now)
    assert policy.offer(stale, now) == BarrageDecision.DROP_STALE
    assert policy.offer(fresh, now) == BarrageDecision.ACCEPT
    assert policy.offer(fresh, now) == BarrageDecision.DROP_DUPLICATE

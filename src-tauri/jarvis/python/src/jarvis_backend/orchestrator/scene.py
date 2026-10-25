from __future__ import annotations

from dataclasses import dataclass


@dataclass(frozen=True, slots=True)
class SceneChange:
    active: bool
    score: float


class SceneHysteresis:
    """Debounces noisy scene scores with asymmetric thresholds and sample counts."""

    def __init__(
        self,
        enter_threshold: float,
        exit_threshold: float,
        enter_samples: int,
        exit_samples: int,
    ) -> None:
        if exit_threshold >= enter_threshold:
            raise ValueError("exit threshold must be below enter threshold")
        self.enter_threshold = enter_threshold
        self.exit_threshold = exit_threshold
        self.enter_samples = enter_samples
        self.exit_samples = exit_samples
        self.active = False
        self._streak = 0

    def observe(self, score: float) -> SceneChange | None:
        qualifies = score <= self.exit_threshold if self.active else score >= self.enter_threshold
        self._streak = self._streak + 1 if qualifies else 0
        needed = self.exit_samples if self.active else self.enter_samples
        if self._streak < needed:
            return None
        self._streak = 0
        self.active = not self.active
        return SceneChange(active=self.active, score=score)


class CourseSceneStabilizer:
    """Requires consistent evidence before entering or switching special scenes."""

    def __init__(
        self,
        enter_samples: int = 2,
        exit_samples: int = 2,
        game_enter_samples: int | None = None,
    ) -> None:
        if (
            enter_samples < 1
            or exit_samples < 1
            or (game_enter_samples is not None and game_enter_samples < 1)
        ):
            raise ValueError("sample counts must be positive")
        self.enter_samples = enter_samples
        self.exit_samples = exit_samples
        self.game_enter_samples = (
            enter_samples if game_enter_samples is None else game_enter_samples
        )
        self.current = "other"
        self._candidate: str | None = None
        self._streak = 0

    def force(self, scene: str) -> None:
        self.current = scene
        self._candidate = None
        self._streak = 0

    def observe(self, scene: str, *, exit_samples: int | None = None) -> str:
        if exit_samples is not None and exit_samples < 1:
            raise ValueError("exit_samples must be positive")
        if scene not in {"game", "course", "other"}:
            scene = "other"
        if scene == self.current:
            self.force(scene)
            return self.current

        self._streak = self._streak + 1 if self._candidate == scene else 1
        self._candidate = scene
        if self.current == "other":
            needed = self.game_enter_samples if scene == "game" else self.enter_samples
        else:
            needed = self.exit_samples if exit_samples is None else exit_samples
        if self._streak >= needed:
            self.force(scene)
        return self.current

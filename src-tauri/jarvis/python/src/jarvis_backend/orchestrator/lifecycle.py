from __future__ import annotations

import asyncio
from dataclasses import dataclass
from datetime import UTC, datetime
from enum import StrEnum


class LifecycleState(StrEnum):
    STOPPED = "stopped"
    STARTING = "starting"
    READY = "ready"
    DEGRADED = "degraded"
    STOPPING = "stopping"
    FAILED = "failed"


_ALLOWED: dict[LifecycleState, frozenset[LifecycleState]] = {
    LifecycleState.STOPPED: frozenset({LifecycleState.STARTING}),
    LifecycleState.STARTING: frozenset(
        {
            LifecycleState.READY,
            LifecycleState.DEGRADED,
            LifecycleState.FAILED,
            LifecycleState.STOPPING,
        }
    ),
    LifecycleState.READY: frozenset(
        {LifecycleState.DEGRADED, LifecycleState.STOPPING, LifecycleState.FAILED}
    ),
    LifecycleState.DEGRADED: frozenset(
        {LifecycleState.READY, LifecycleState.STOPPING, LifecycleState.FAILED}
    ),
    LifecycleState.STOPPING: frozenset({LifecycleState.STOPPED, LifecycleState.FAILED}),
    LifecycleState.FAILED: frozenset({LifecycleState.STARTING, LifecycleState.STOPPING}),
}


class InvalidTransition(RuntimeError):
    pass


@dataclass(frozen=True, slots=True)
class LifecycleSnapshot:
    state: LifecycleState
    reason: str | None
    changed_at: datetime


class Lifecycle:
    def __init__(self) -> None:
        self._state = LifecycleState.STOPPED
        self._reason: str | None = None
        self._changed_at = datetime.now(UTC)
        self._lock = asyncio.Lock()

    @property
    def snapshot(self) -> LifecycleSnapshot:
        return LifecycleSnapshot(self._state, self._reason, self._changed_at)

    async def transition(
        self, target: LifecycleState, reason: str | None = None
    ) -> LifecycleSnapshot:
        async with self._lock:
            if target == self._state:
                return self.snapshot
            if target not in _ALLOWED[self._state]:
                raise InvalidTransition(f"cannot transition {self._state} -> {target}")
            self._state = target
            self._reason = reason
            self._changed_at = datetime.now(UTC)
            return self.snapshot

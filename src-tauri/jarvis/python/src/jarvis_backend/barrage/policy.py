from __future__ import annotations

from collections.abc import Iterable
from dataclasses import dataclass
from datetime import UTC, datetime, timedelta
from enum import StrEnum


class BarrageDecision(StrEnum):
    ACCEPT = "accept"
    DROP_STALE = "drop_stale"
    DROP_DUPLICATE = "drop_duplicate"
    DROP_OVERFLOW = "drop_overflow"


@dataclass(frozen=True, slots=True)
class BarrageItem:
    id: str
    text: str
    created_at: datetime
    priority: int = 0


class BarragePolicy:
    """Rejects stale/duplicate work and keeps a bounded priority queue."""

    def __init__(self, max_age_seconds: float, max_queue_size: int):
        self.max_age = timedelta(seconds=max_age_seconds)
        self.max_queue_size = max_queue_size
        self._seen: set[str] = set()
        self._items: list[BarrageItem] = []

    def offer(self, item: BarrageItem, now: datetime | None = None) -> BarrageDecision:
        now = now or datetime.now(UTC)
        created_at = item.created_at
        if created_at.tzinfo is None:
            created_at = created_at.replace(tzinfo=UTC)
        if now - created_at > self.max_age:
            return BarrageDecision.DROP_STALE
        if item.id in self._seen:
            return BarrageDecision.DROP_DUPLICATE
        if len(self._items) >= self.max_queue_size:
            lowest = min(self._items, key=lambda current: current.priority)
            if item.priority <= lowest.priority:
                return BarrageDecision.DROP_OVERFLOW
            self._items.remove(lowest)
            self._seen.discard(lowest.id)
        self._seen.add(item.id)
        self._items.append(item)
        return BarrageDecision.ACCEPT

    def drain(self) -> Iterable[BarrageItem]:
        items = sorted(self._items, key=lambda item: (-item.priority, item.created_at))
        self._items.clear()
        self._seen.clear()
        return items

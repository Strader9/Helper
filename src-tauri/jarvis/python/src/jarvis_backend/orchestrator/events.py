from __future__ import annotations

import asyncio
from collections import deque
from collections.abc import AsyncIterator
from dataclasses import dataclass
from datetime import UTC, datetime
from typing import Any
from uuid import uuid4


@dataclass(frozen=True, slots=True)
class Event:
    topic: str
    payload: dict[str, Any]
    id: str = ""
    occurred_at: datetime | None = None

    def __post_init__(self) -> None:
        if not self.id:
            object.__setattr__(self, "id", uuid4().hex)
        if self.occurred_at is None:
            object.__setattr__(self, "occurred_at", datetime.now(UTC))


class Subscription:
    def __init__(self, bus: EventBus, queue: asyncio.Queue[Event], topics: frozenset[str]):
        self._bus = bus
        self._queue = queue
        self._topics = topics
        self._closed = False

    async def get(self) -> Event:
        return await self._queue.get()

    async def __aiter__(self) -> AsyncIterator[Event]:
        while not self._closed:
            yield await self.get()

    async def close(self) -> None:
        if not self._closed:
            self._closed = True
            await self._bus.unsubscribe(self)


class EventBus:
    """Bounded in-process fan-out bus; slow subscribers lose oldest events."""

    def __init__(self, history_size: int = 100, subscriber_queue_size: int = 128):
        self._history: deque[Event] = deque(maxlen=history_size)
        self._queue_size = subscriber_queue_size
        self._subscriptions: set[Subscription] = set()
        self._lock = asyncio.Lock()

    async def publish(self, event: Event) -> Event:
        async with self._lock:
            self._history.append(event)
            subscribers = tuple(self._subscriptions)
        for subscription in subscribers:
            if subscription._topics and event.topic not in subscription._topics:
                continue
            if subscription._queue.full():
                subscription._queue.get_nowait()
            subscription._queue.put_nowait(event)
        return event

    async def subscribe(self, *topics: str) -> Subscription:
        subscription = Subscription(
            self, asyncio.Queue(maxsize=self._queue_size), frozenset(topics)
        )
        async with self._lock:
            self._subscriptions.add(subscription)
        return subscription

    async def unsubscribe(self, subscription: Subscription) -> None:
        async with self._lock:
            self._subscriptions.discard(subscription)

    def history(self, topic: str | None = None) -> list[Event]:
        return [event for event in self._history if topic is None or event.topic == topic]

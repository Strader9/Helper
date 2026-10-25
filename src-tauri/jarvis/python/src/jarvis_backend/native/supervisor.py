from __future__ import annotations

import asyncio
from contextlib import suppress

from .client import NativeClient


class WorkerSupervisor:
    """Owns one worker client and forwards events without leaking background tasks."""

    def __init__(self, client: NativeClient, *, restart_limit: int = 3):
        self.client = client
        self.restart_limit = restart_limit
        self._pump_task: asyncio.Task[None] | None = None
        self._sink = None

    async def start(self, event_sink=None) -> None:
        self._sink = event_sink
        await self.client.start()
        self._pump_task = asyncio.create_task(self._pump_events(), name="native-event-pump")

    async def stop(self) -> None:
        await self.client.stop()
        if self._pump_task:
            with suppress(asyncio.CancelledError):
                await self._pump_task
            self._pump_task = None

    async def _pump_events(self) -> None:
        async for event in self.client.events():
            if self._sink is not None:
                result = self._sink(event)
                if asyncio.iscoroutine(result):
                    await result

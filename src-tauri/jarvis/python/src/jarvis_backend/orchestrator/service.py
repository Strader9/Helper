from __future__ import annotations

import asyncio
import base64
import binascii
import json
import logging
import random
import re
import time
from collections import deque
from collections.abc import Sequence
from datetime import UTC, date, datetime, timedelta
from difflib import SequenceMatcher
from pathlib import Path
from typing import Any
from uuid import uuid4

from jarvis_backend.barrage import BarrageItem, BarragePolicy
from jarvis_backend.courses import CourseRepository, CourseState, CourseStatus, desktop_path
from jarvis_backend.memory import (
    ImageGenerationClient,
    ImageProvider,
    MemoryEvent,
    MemoryStore,
)
from jarvis_backend.native import NativeClient, WorkerSupervisor
from jarvis_backend.orchestrator.events import Event, EventBus
from jarvis_backend.orchestrator.lifecycle import Lifecycle, LifecycleState
from jarvis_backend.orchestrator.scene import CourseSceneStabilizer, SceneHysteresis
from jarvis_backend.prompts import (
    AMBIENT_DUPLEX_INSTRUCTION,
    build_course_chunk_prompt,
    build_daily_image_prompt,
    build_daily_summary_prompt,
    build_final_course_summary_prompt,
    build_pet_chat_prompt,
)
from jarvis_backend.settings import Settings

logger = logging.getLogger(__name__)
ASSETS_DIR = Path(__file__).resolve().parents[1] / "assets"

AMBIENT_DUPLEX_SESSION_ID = "jarvis-ambient"
PET_CHAT_TIMEOUT_SECONDS = 600.0
SCREEN_IDLE_MESSAGES = (
    "是在摸鱼吗？",
    "ZZZ...",
    "摸鱼小神仙是你吗？？",
    "屏幕都快睡着了。",
    "今天的鱼摸得很有节奏嘛。",
)
def _normalize_text(text: str) -> str:
    return re.sub(r"[\W_]+", "", text.casefold())


def _texts_are_similar(left: str, right: str) -> bool:
    left_normalized = _normalize_text(left)
    right_normalized = _normalize_text(right)
    if not left_normalized or not right_normalized:
        return False
    if left_normalized == right_normalized:
        return True
    if min(len(left_normalized), len(right_normalized)) < 4:
        return False
    sequence_ratio = SequenceMatcher(None, left_normalized, right_normalized).ratio()
    shorter = min(len(left_normalized), len(right_normalized))
    shared_characters = sum(
        min(left_normalized.count(character), right_normalized.count(character))
        for character in set(left_normalized)
    )
    character_coverage = shared_characters / shorter
    length_ratio = shorter / max(len(left_normalized), len(right_normalized))
    return sequence_ratio >= 0.78 or (character_coverage >= 0.9 and length_ratio >= 0.65)


def _barrage_quality_penalty(text: str) -> int:
    penalty = 0
    if re.search(r"[？?]|是.{0,10}还是|是不是|难道|莫非", text):
        penalty += 6
    if re.search(r"看起来|似乎|可能|大概|也许|不知道", text):
        penalty += 3
    if re.search(r"根据画面|当前画面|画面中|屏幕上", text):
        penalty += 5
    if len(text) < 6:
        penalty += 2
    return penalty


class OrchestrationService:
    def __init__(
        self,
        settings: Settings,
        native_client: NativeClient,
        event_bus: EventBus | None = None,
    ) -> None:
        self.settings = settings
        self.native_client = native_client
        self.events = event_bus or EventBus()
        self.lifecycle = Lifecycle()
        self.scene = SceneHysteresis(
            settings.scene.enter_threshold,
            settings.scene.exit_threshold,
            settings.scene.enter_samples,
            settings.scene.exit_samples,
        )
        self.barrage = BarragePolicy(
            settings.barrage.max_age_seconds, settings.barrage.max_queue_size
        )
        self.memory = MemoryStore(settings.memory.root)
        self.image_generator = ImageGenerationClient()
        self.courses = CourseRepository(settings.courses.sessions_root)
        self.supervisor = WorkerSupervisor(native_client)
        recording = [
            session
            for session in self.courses.sessions()
            if session.state.status == CourseStatus.RECORDING
        ]
        active_course = recording[-1] if recording else None
        self._auto_course_id = active_course.state.id if active_course else None
        self._non_course_streak = 0
        self._non_course_started_at: float | None = None
        self._last_course_transcript = (
            active_course.transcript_path.read_text(encoding="utf-8")[-2000:].strip()
            if active_course
            else ""
        )
        self.display_scene = CourseSceneStabilizer(
            enter_samples=settings.scene.display_enter_samples,
            exit_samples=settings.scene.display_exit_samples,
            game_enter_samples=settings.scene.game_enter_samples,
        )
        self._pending_course_results: deque[dict[str, Any]] = deque(
            maxlen=self.display_scene.enter_samples
        )
        self._recent_barrages: deque[tuple[str, float]] = deque(maxlen=12)
        self._barrage_task: asyncio.Task[None] | None = None
        self._ambient_duplex_task: asyncio.Task[None] | None = None
        self._monitoring_requested = False
        self._recent_duplex_messages: deque[tuple[str, float]] = deque(maxlen=4)
        self._recent_assistant_messages: deque[tuple[str, float]] = deque(maxlen=6)
        self._pet_chat_history: deque[tuple[str, str]] = deque(maxlen=4)
        self._pet_chat_lock = asyncio.Lock()
        self._pending_duplex_fragment: tuple[str | None, str, float] | None = None
        self._discarding_duplex_fragment: tuple[str | None, float] | None = None
        self._duplex_session_id: str | None = None
        self._duplex_instruction = ""
        self._screen_idle = False
        self._last_course_interaction = ""
        self._last_course_interaction_at = float("-inf")
        self._last_keyframe_requested_at: dict[str, float] = {}
        recent_activity = next(
            (event for event in reversed(self.memory.events()) if event.kind == "activity"),
            None,
        )
        today = datetime.now().astimezone().date()
        self._last_memory_activity: tuple[str, str, float] | None = (
            (
                str(recent_activity.metadata.get("scene", "other")),
                recent_activity.text,
                time.monotonic(),
            )
            if recent_activity and self.memory.event_day(recent_activity) == today
            else None
        )

    async def start(self) -> None:
        await self.lifecycle.transition(LifecycleState.STARTING)
        try:
            await self.supervisor.start(self._on_native_event)
        except Exception as exc:
            await self.lifecycle.transition(LifecycleState.FAILED, str(exc))
            raise
        await self.lifecycle.transition(LifecycleState.READY)
        await self.events.publish(Event("lifecycle.changed", {"state": "ready"}))

    async def stop(self) -> None:
        if self.lifecycle.snapshot.state == LifecycleState.STOPPED:
            return
        await self.lifecycle.transition(LifecycleState.STOPPING)
        self._monitoring_requested = False
        await self._cancel_ambient_duplex_start()
        await self._cancel_barrage_sequence()
        await self.supervisor.stop()
        await self.lifecycle.transition(LifecycleState.STOPPED)
        await self.events.publish(Event("lifecycle.changed", {"state": "stopped"}))

    async def command(self, method: str, arguments: dict[str, Any]) -> dict[str, Any]:
        if method == "start_duplex":
            return await self.start_duplex(
                str(arguments.get("instruction", "")),
                str(arguments.get("session_id", "")) or None,
            )
        if method == "stop_duplex":
            return await self.stop_duplex()
        if method in {"pause_monitoring", "stop_monitoring"}:
            self._monitoring_requested = False
            await self._cancel_ambient_duplex_start()
        result = await self.native_client.request(method, arguments)
        if method in {"start_monitoring", "resume_monitoring"}:
            self._screen_idle = False
            self._monitoring_requested = True
            await self._cancel_ambient_duplex_start()
            self._ambient_duplex_task = asyncio.create_task(
                self._initialize_ambient_duplex(),
                name="jarvis-ambient-duplex-start",
            )
        elif method in {"pause_monitoring", "stop_monitoring"}:
            self._screen_idle = False
            await self._cancel_barrage_sequence()
            previous_id = self._clear_duplex_state()
            if previous_id is not None:
                await self.events.publish(
                    Event("duplex.task.stopped", {"session_id": previous_id})
                )
        await self.events.publish(Event("command.completed", {"command": method, "result": result}))
        return result

    async def _cancel_ambient_duplex_start(self) -> None:
        task = self._ambient_duplex_task
        self._ambient_duplex_task = None
        if task is None or task is asyncio.current_task():
            return
        task.cancel()
        try:
            await task
        except asyncio.CancelledError:
            pass

    async def _initialize_ambient_duplex(self) -> None:
        task = asyncio.current_task()
        await self.events.publish(
            Event(
                "duplex.task.initializing",
                {"session_id": AMBIENT_DUPLEX_SESSION_ID},
            )
        )
        try:
            await self.start_duplex(
                AMBIENT_DUPLEX_INSTRUCTION,
                AMBIENT_DUPLEX_SESSION_ID,
            )
        except asyncio.CancelledError:
            raise
        except Exception as exc:
            if self._ambient_duplex_task is not task or not self._monitoring_requested:
                return
            self._monitoring_requested = False
            logger.exception("ambient duplex initialization failed")
            message = self._ambient_duplex_error(exc)
            await self.events.publish(
                Event(
                    "duplex.task.failed",
                    {
                        "session_id": AMBIENT_DUPLEX_SESSION_ID,
                        "error": message,
                    },
                )
            )
            try:
                await self.native_client.request("stop_monitoring", {})
            except Exception:
                logger.exception("failed to stop monitoring after duplex initialization failure")
        finally:
            if self._ambient_duplex_task is task:
                self._ambient_duplex_task = None

    @staticmethod
    def _ambient_duplex_error(error: Exception) -> str:
        detail = str(error).casefold()
        if "timeout" in detail or "timed out" in detail:
            return "环境感知模型初始化超时，请关闭占用内存或显存的程序后重试"
        if any(marker in detail for marker in ("gpu", "memory", "显存", "内存")):
            return "环境感知模型初始化失败，请确认可用内存和显存充足后重试"
        return "环境感知模型初始化失败，请查看运行日志后重试"

    async def pet_chat(self, message: str) -> str:
        cleaned = message.strip()
        if not cleaned:
            raise ValueError("message must not be empty")
        if len(cleaned) > 2000:
            raise ValueError("message exceeds 2000 characters")
        if any(ord(character) < 32 and character not in "\n\t" for character in cleaned):
            raise ValueError("message contains unsupported control characters")
        cleaned = cleaned.replace("<|", "< |")

        async with self._pet_chat_lock:
            paused_ambient_duplex = (
                self._monitoring_requested
                and self._duplex_session_id == AMBIENT_DUPLEX_SESSION_ID
            )
            if paused_ambient_duplex:
                await self._stop_duplex_native()
            history = [
                {"user": user[-1000:], "assistant": assistant[-1500:]}
                for user, assistant in self._pet_chat_history
            ]
            prompt = build_pet_chat_prompt(history, cleaned)
            try:
                response = await self.native_client.request(
                    "ask", {"text": prompt, "_timeout_seconds": PET_CHAT_TIMEOUT_SECONDS}
                )
                reply = self._clean_pet_chat_reply(str(response.get("text", "")))
                if not reply:
                    raise RuntimeError("local model returned an empty chat response")
                self._pet_chat_history.append((cleaned, reply))
                await self.events.publish(
                    Event("pet.chat.completed", {"message_length": len(cleaned)})
                )
                return reply
            finally:
                if (
                    paused_ambient_duplex
                    and self._monitoring_requested
                    and self._duplex_session_id is None
                    and self._ambient_duplex_task is None
                ):
                    self._ambient_duplex_task = asyncio.create_task(
                        self._initialize_ambient_duplex(),
                        name="jarvis-ambient-duplex-resume-after-chat",
                    )

    @staticmethod
    def _clean_pet_chat_reply(message: str) -> str:
        cleaned = re.sub(r"<\|(?:im_start|im_end|endoftext)\|>", "", message)
        cleaned = cleaned.replace("__END_OF_TURN__", "").strip()
        cleaned = re.sub(r"^assistant\s*[:：]?\s*", "", cleaned, flags=re.IGNORECASE)
        return cleaned[:4000]

    async def observe_scene(self, score: float) -> bool:
        change = self.scene.observe(score)
        if change:
            await self.events.publish(
                Event("scene.changed", {"active": change.active, "score": change.score})
            )
        return change is not None

    async def submit_barrage(
        self, item_id: str, text: str, created_at: datetime, priority: int
    ) -> str:
        decision = self.barrage.offer(BarrageItem(item_id, text, created_at, priority))
        await self.events.publish(
            Event("barrage.decision", {"id": item_id, "decision": decision.value})
        )
        return decision.value

    async def memory_status(self) -> dict[str, Any]:
        events = self.memory.events()
        today = datetime.now().astimezone().date()
        today_events = self.memory.events_for_day(today)
        return {
            "event_count": len(events),
            "summary": self.memory.read_summary(),
            "fact_count": len(self.memory.read_facts()),
            "today": today.isoformat(),
            "today_event_count": len(today_events),
            "today_generated": self.memory.read_daily_memory(today) is not None,
        }

    async def summarize_memory(self) -> str:
        def summarize(events: Sequence[MemoryEvent], previous: str | None) -> str:
            lines = [event.text.strip() for event in events if event.text.strip()]
            return "\n".join(lines) if lines else (previous or "")

        summary = self.memory.summarize(summarize)
        await self.events.publish(Event("memory.summarized", {"summary": summary}))
        return summary

    async def clear_memory(self) -> None:
        self.memory.clear()
        self._last_memory_activity = None
        await self.events.publish(Event("memory.cleared", {}))

    @staticmethod
    def _memory_day(value: str) -> date:
        try:
            return date.fromisoformat(value)
        except ValueError as exc:
            raise ValueError("memory date must use YYYY-MM-DD") from exc

    @staticmethod
    def _memory_preview(content: str) -> str:
        for line in content.splitlines():
            cleaned = line.strip().lstrip("#>- ").strip()
            if (
                cleaned
                and not re.fullmatch(r"\d{4}-\d{2}-\d{2} 的记忆", cleaned)
                and cleaned not in {"今日概览", "活动时间线", "今日回顾"}
                and not cleaned.startswith(("生成于", "由本地模型总结于"))
            ):
                return cleaned[:100]
        return ""

    @staticmethod
    def _memory_activity_category(event: MemoryEvent) -> str:
        text = event.text.casefold()
        scene = str(event.metadata.get("scene", "other"))
        negative_course = re.search(
            r"(?:无|没有|非)[^，。；]{0,12}(?:课程|授课|教学)", text
        )
        course_markers = ("课程内容", "网课", "授课", "讲课", "教学视频", "学习笔记")
        if not negative_course and any(marker in text for marker in course_markers):
            return "课程学习"

        game_markers = ("minecraft", "我的世界", "游戏画面", "游戏场景")
        game_actions = (
            "玩家",
            "第一人称",
            "手持",
            "操作",
            "战斗",
            "关卡",
            "hud",
            "角色",
            "移动",
            "挖掘",
        )
        active_game = (scene == "game" or any(marker in text for marker in game_markers)) and any(
            marker in text
            for marker in game_actions
        )
        if active_game:
            return "玩游戏"

        media_tool_markers = ("视频压缩", "在线视频压缩", "视频裁剪", "裁剪器")
        if any(marker in text for marker in media_tool_markers):
            return "媒体处理"

        direct_work_markers = (
            "代码",
            "编程",
            "项目",
            "开发",
            "调试",
            "ide",
            "python",
            "javascript",
            "c++",
            "visual studio",
            "vs code",
            "codex",
            "godex",
        )
        file_markers = ("文件资源管理器", "文件夹", "文件列表", "文件管理")
        file_actions = (
            "正在浏览",
            "浏览名为",
            "整理",
            "处理",
            "移动文件",
            "复制文件",
            "选中",
            "右键",
            "压缩",
            "裁剪",
        )
        if any(marker in text for marker in direct_work_markers) or (
            any(marker in text for marker in file_markers)
            and any(marker in text for marker in file_actions)
        ):
            return "项目工作"

        web_markers = (
            "bilibili",
            "哔哩",
            "miaocut",
            "购物",
            "商品",
            "下单",
            "购物车",
            "电商",
            "搜索结果",
            "新闻",
            "推荐内容",
            "社交媒体",
        )
        if any(marker in text for marker in web_markers):
            return "上网浏览"

        media_markers = ("电影", "正在播放", "持续播放", "飞船", "星云", "科幻", "游戏启动")
        if "视频文件" not in text and any(marker in text for marker in media_markers):
            return "观看视频或游戏画面"

        desktop_markers = ("桌面", "锁屏")
        idle_markers = (
            "无交互",
            "静止",
            "静态",
            "无动态",
            "无明显操作",
            "无明显交互",
            "无明显课程或游戏界面",
        )
        if "锁屏" in text or (
            any(marker in text for marker in desktop_markers)
            and any(marker in text for marker in idle_markers)
        ):
            return "基本无操作"
        return "日常操作"

    @staticmethod
    def _memory_detail_markers(category: str) -> tuple[str, ...]:
        return {
            "项目工作": (
                "项目",
                "代码",
                "python",
                "javascript",
                "c++",
                "codex",
                "godex",
                "minicpm",
                "内存",
                "提示词",
                "驱动",
                "压缩",
                "裁剪",
            ),
            "课程学习": ("课程", "网课", "讲解", "学习笔记", "知识点"),
            "玩游戏": ("minecraft", "我的世界", "玩家", "挖掘", "移动", "关卡"),
            "上网浏览": ("bilibili", "哔哩", "购物", "商品", "新闻", "搜索结果"),
            "媒体处理": ("视频", "压缩", "裁剪", "转换", "进度"),
            "观看视频或游戏画面": ("科幻", "飞船", "星云", "电影", "播放"),
        }.get(category, ())

    @classmethod
    def _memory_detail_excerpt(
        cls, event: MemoryEvent, category: str, limit: int
    ) -> tuple[int, str]:
        text = event.text.strip()
        folded = text.casefold()
        positions = [
            folded.index(marker)
            for marker in cls._memory_detail_markers(category)
            if marker in folded
        ]
        score = len(positions)
        start = 0
        if len(text) > limit and positions:
            start = max(0, min(positions) - 8)
        return score, text[start : start + limit].strip("，。； ")

    @classmethod
    def _compact_memory_timeline(
        cls, events: Sequence[MemoryEvent], limit: int = 2800
    ) -> str:
        local_timezone = datetime.now().astimezone().tzinfo
        categorized: list[tuple[datetime, MemoryEvent, str]] = []
        for event in events:
            timestamp = datetime.fromisoformat(event.timestamp.replace("Z", "+00:00"))
            local_time = timestamp.astimezone(local_timezone)
            categorized.append((local_time, event, cls._memory_activity_category(event)))

        for index in range(1, len(categorized) - 1):
            previous = categorized[index - 1]
            current = categorized[index]
            following = categorized[index + 1]
            if (
                previous[2] == following[2] != current[2]
                and current[2] in {"日常操作", "基本无操作"}
                and following[0] - previous[0] <= timedelta(minutes=10)
            ):
                categorized[index] = (current[0], current[1], previous[2])

        buckets: list[list[tuple[datetime, MemoryEvent, str]]] = []
        bucket_keys: list[tuple[date, int]] = []
        for item in categorized:
            local_time = item[0]
            key = (local_time.date(), (local_time.hour * 60 + local_time.minute) // 90)
            if not bucket_keys or bucket_keys[-1] != key:
                bucket_keys.append(key)
                buckets.append([])
            buckets[-1].append(item)

        details_per_bucket = 4
        overhead = 72
        detail_limit = max(
            36,
            (limit - len(buckets) * overhead)
            // max(1, len(buckets) * details_per_bucket),
        )
        lines = []
        category_priority = {
            "课程学习": 6,
            "玩游戏": 6,
            "上网浏览": 6,
            "媒体处理": 5,
            "项目工作": 4,
            "观看视频或游戏画面": 3,
            "基本无操作": 1,
            "日常操作": 0,
        }
        for bucket in buckets:
            first_time = bucket[0][0]
            last_time = bucket[-1][0]
            candidates = []
            counts: dict[str, int] = {}
            for index, (_, event, category) in enumerate(bucket):
                counts[category] = counts.get(category, 0) + 1
                score, excerpt = cls._memory_detail_excerpt(
                    event, category, detail_limit
                )
                candidates.append(
                    (category_priority[category], score, index, category, excerpt)
                )

            selected: list[tuple[int, int, int, str, str]] = []
            for category in sorted(
                counts, key=lambda item: -category_priority[item]
            ):
                if category_priority[category] < 3:
                    continue
                best = min(
                    (item for item in candidates if item[3] == category),
                    key=lambda item: (-item[1], item[2]),
                )
                selected.append(best)
                if len(selected) == details_per_bucket:
                    break
            for candidate in sorted(
                candidates, key=lambda item: (-item[0], -item[1], item[2])
            ):
                if len(selected) == details_per_bucket:
                    break
                if candidate not in selected and candidate[4] not in {
                    item[4] for item in selected
                }:
                    selected.append(candidate)

            selected.sort(key=lambda item: item[2])
            details = "；".join(item[4] for item in selected if item[4])
            if len(counts) == 1:
                category, count = next(iter(counts.items()))
                category_summary = f"{category}，记录{count}条"
            else:
                category_summary = "；".join(
                    f"{category}{count}条" for category, count in counts.items()
                )
                category_summary += f"，共{len(bucket)}条"
            lines.append(
                f"{first_time:%H:%M}-{last_time:%H:%M} "
                f"[{category_summary}] {details}"
            )
        return "\n".join(lines)

    async def _ask_memory_summarizer(self, instruction: str, *, limit: int = 6000) -> str:
        response = await self.native_client.request(
            "ask",
            {
                "text": "[[JARVIS_TEXT_ONLY]]\n" + instruction,
                "_timeout_seconds": self.settings.memory.summary_timeout_seconds,
            },
        )
        text = str(response.get("text", "")).strip()
        text = re.sub(r"^```(?:markdown|text)?\s*|\s*```$", "", text, flags=re.IGNORECASE)
        if not text:
            raise RuntimeError("memory summary response is empty")
        return text[:limit]

    @staticmethod
    def _memory_summary_covers(
        summary: str, first_event: MemoryEvent, last_event: MemoryEvent
    ) -> bool:
        times = [
            int(hour) * 60 + int(minute)
            for hour, minute in re.findall(r"(?<!\d)([01]\d|2[0-3]):([0-5]\d)", summary)
        ]
        if (
            not times
            or len(times) > 28
            or not summary.rstrip().endswith(("。", "！", "？", ".", "!", "?"))
        ):
            return False
        local_timezone = datetime.now().astimezone().tzinfo
        first_timestamp = datetime.fromisoformat(
            first_event.timestamp.replace("Z", "+00:00")
        ).astimezone(local_timezone)
        last_timestamp = datetime.fromisoformat(
            last_event.timestamp.replace("Z", "+00:00")
        ).astimezone(local_timezone)
        first_minutes = first_timestamp.hour * 60 + first_timestamp.minute
        last_minutes = last_timestamp.hour * 60 + last_timestamp.minute
        return min(times) <= first_minutes + 10 and max(times) >= last_minutes - 10

    @staticmethod
    def _daily_summary_instruction(
        day: date,
        cutoff: datetime,
        source: str,
        first_time: datetime,
        last_time: datetime,
    ) -> str:
        return build_daily_summary_prompt(day, cutoff, source, first_time, last_time)

    async def _summarize_daily_events(
        self, day: date, events: Sequence[MemoryEvent], generated_at: datetime
    ) -> str:
        source = self._compact_memory_timeline(events)

        cutoff = generated_at if day == generated_at.date() else datetime.combine(
            day, datetime.max.time(), tzinfo=generated_at.tzinfo
        )
        local_timezone = datetime.now().astimezone().tzinfo
        first_time = datetime.fromisoformat(
            events[0].timestamp.replace("Z", "+00:00")
        ).astimezone(local_timezone)
        last_time = datetime.fromisoformat(
            events[-1].timestamp.replace("Z", "+00:00")
        ).astimezone(local_timezone)
        summary = await self._ask_memory_summarizer(
            self._daily_summary_instruction(day, cutoff, source, first_time, last_time),
            limit=1800,
        )
        summary = re.sub(r"\s+", " ", summary).strip()
        if not self._memory_summary_covers(summary, events[0], events[-1]):
            logger.warning("Rejected incomplete daily memory summary: %s", summary)
            raise RuntimeError("memory summary did not cover the latest event")
        return summary

    @staticmethod
    def _wrap_daily_memory(day: date, generated_at: datetime, summary: str) -> str:
        return (
            f"# {day.isoformat()} 的记忆\n\n"
            f"> 由本地模型总结于 {generated_at:%Y-%m-%d %H:%M}。\n\n"
            "## 今日回顾\n\n"
            f"{summary.strip()}\n"
        )

    async def generate_daily_memory(self, day_value: str) -> dict[str, Any]:
        day = self._memory_day(day_value)
        events = self.memory.events_for_day(day)
        generated_at = datetime.now().astimezone()
        if events:
            summary = await self._summarize_daily_events(day, events, generated_at)
        else:
            summary = "今天暂时没有记录到可归纳的活动。"
        content = self._wrap_daily_memory(day, generated_at, summary)
        self.memory.write_daily_memory(day, content)
        result = {
            "date": day.isoformat(),
            "event_count": len(events),
            "generated": True,
            "content": content,
        }
        await self.events.publish(
            Event("memory.day.generated", {"date": day.isoformat(), "event_count": len(events)})
        )
        return result

    @staticmethod
    def _daily_review(content: str) -> str:
        marker = "## 今日回顾"
        review = content.partition(marker)[2] if marker in content else content
        return re.sub(r"\s+", " ", review).strip()

    @staticmethod
    def _daily_image_prompt(day: date, review: str) -> str:
        return build_daily_image_prompt(day, review)

    @staticmethod
    def _image_extension(content: bytes) -> str:
        if content.startswith(b"\x89PNG\r\n\x1a\n"):
            return "png"
        if content.startswith(b"\xff\xd8\xff"):
            return "jpg"
        if content.startswith(b"RIFF") and content[8:12] == b"WEBP":
            return "webp"
        raise RuntimeError("generated image uses an unsupported format")

    def daily_images(self, day_value: str | None = None) -> list[dict[str, Any]]:
        day = self._memory_day(day_value) if day_value else None
        return self.memory.daily_images(day)

    def daily_image_path(self, day_value: str, filename: str) -> Path:
        return self.memory.daily_image_path(self._memory_day(day_value), filename)

    async def generate_daily_image(
        self, day_value: str, provider: ImageProvider
    ) -> dict[str, Any]:
        day = self._memory_day(day_value)
        content = self.memory.read_daily_memory(day)
        if content is None:
            generated_memory = await self.generate_daily_memory(day.isoformat())
            content = str(generated_memory["content"])
        review = self._daily_review(content)
        if not review:
            raise RuntimeError("daily memory contains no review to visualize")
        references = [
            ASSETS_DIR / "jarvis-character-reference.png",
            ASSETS_DIR / "jarvis-style-reference.png",
        ]
        missing = [path.name for path in references if not path.is_file()]
        if missing:
            raise RuntimeError("missing image reference assets: " + ", ".join(missing))
        image = await self.image_generator.generate(
            provider,
            self._daily_image_prompt(day, review),
            references,
        )
        created_at = datetime.now(UTC)
        extension = self._image_extension(image)
        filename = f"{created_at:%Y%m%dT%H%M%S}-{uuid4().hex[:8]}.{extension}"
        metadata = {
            "id": filename,
            "date": day.isoformat(),
            "filename": filename,
            "created_at": created_at.isoformat().replace("+00:00", "Z"),
            "model_name": provider.model_name.strip(),
            "content_url": f"/api/v1/memory/days/{day.isoformat()}/images/{filename}",
        }
        self.memory.write_daily_image(day, filename, image, metadata)
        await self.events.publish(Event("memory.image.generated", metadata))
        return metadata

    def duplex_status(self) -> dict[str, Any]:
        return {
            "active": self._duplex_session_id is not None,
            "session_id": self._duplex_session_id,
            "instruction": self._duplex_instruction,
        }

    def _clear_duplex_state(self) -> str | None:
        previous_id = self._duplex_session_id
        self._duplex_session_id = None
        self._duplex_instruction = ""
        self._recent_duplex_messages.clear()
        self._pending_duplex_fragment = None
        self._discarding_duplex_fragment = None
        return previous_id

    async def start_duplex(
        self, instruction: str, session_id: str | None = None
    ) -> dict[str, Any]:
        cleaned = re.sub(r"\s+", " ", instruction).strip()
        if not cleaned:
            raise ValueError("duplex instruction must not be empty")
        if len(cleaned) > 2000:
            raise ValueError("duplex instruction exceeds 2000 characters")
        if any(ord(character) < 32 for character in cleaned):
            raise ValueError("duplex instruction contains unsupported control characters")
        if session_id is not None and not re.fullmatch(r"[A-Za-z0-9_-]+", session_id):
            raise ValueError("duplex session ID contains unsupported characters")
        if self._duplex_session_id is not None:
            await self._stop_duplex_native()
        resolved_id = session_id or f"watch-{uuid4().hex}"
        await self.native_client.request(
            "start_duplex",
            {
                "session_id": resolved_id,
                "instruction": cleaned,
                "_timeout_seconds": 600.0,
            },
        )
        self._duplex_session_id = resolved_id
        self._duplex_instruction = cleaned
        self._recent_duplex_messages.clear()
        self._pending_duplex_fragment = None
        self._discarding_duplex_fragment = None
        await self.events.publish(
            Event(
                "duplex.task.started",
                {"session_id": resolved_id, "instruction": cleaned},
            )
        )
        return self.duplex_status()

    async def stop_duplex(self) -> dict[str, Any]:
        self._monitoring_requested = False
        await self._cancel_ambient_duplex_start()
        return await self._stop_duplex_native()

    async def _stop_duplex_native(self) -> dict[str, Any]:
        previous_id = self._clear_duplex_state()
        await self.native_client.request("stop_duplex", {})
        if previous_id is not None:
            await self.events.publish(
                Event("duplex.task.stopped", {"session_id": previous_id})
            )
        return self.duplex_status()

    async def get_daily_memory(self, day_value: str) -> dict[str, Any]:
        day = self._memory_day(day_value)
        content = self.memory.read_daily_memory(day)
        if content is None:
            raise FileNotFoundError(day.isoformat())
        return {
            "date": day.isoformat(),
            "event_count": len(self.memory.events_for_day(day)),
            "generated": True,
            "content": content,
        }

    async def list_daily_memories(self) -> list[dict[str, Any]]:
        result = []
        for day in self.memory.memory_days():
            content = self.memory.read_daily_memory(day)
            events = self.memory.events_for_day(day)
            result.append(
                {
                    "date": day.isoformat(),
                    "event_count": len(events),
                    "generated": content is not None,
                    "preview": self._memory_preview(content or (events[0].text if events else "")),
                }
            )
        return result

    @staticmethod
    def _clean_memory_activity(text: str) -> str:
        cleaned = re.sub(r"\s+", " ", text).strip()
        uncertain = ("无法判断", "无法识别", "看不清", "没有足够信息", "no clear activity")
        if len(cleaned) < 8 or any(marker in cleaned.casefold() for marker in uncertain):
            return ""
        return cleaned[:240]

    async def _record_memory_activity(self, result: dict[str, Any], now: float) -> None:
        confidence = float(result.get("confidence", 0.0))
        if confidence < self.settings.memory.activity_min_confidence:
            return
        scene = str(result.get("observed_scene", result.get("scene", "other")))
        description = self._clean_memory_activity(str(result.get("observation", "")))
        if not description and scene == "course":
            title = self._clean_memory_activity(str(result.get("course_title", "")))
            description = f"正在学习课程：{title}" if title else ""
        if not description:
            return

        previous = self._last_memory_activity
        if previous is not None:
            previous_scene, previous_text, recorded_at = previous
            elapsed = now - recorded_at
            if (
                scene == previous_scene
                and elapsed < self.settings.memory.activity_min_interval_seconds
            ):
                return
            if (
                scene == previous_scene
                and elapsed < self.settings.memory.activity_duplicate_window_seconds
                and _texts_are_similar(description, previous_text)
            ):
                return

        event = self.memory.append(
            "activity",
            description,
            {
                "scene": scene,
                "confidence": round(confidence, 3),
                "source": "perception",
            },
        )
        day = self.memory.event_day(event)
        self._last_memory_activity = (scene, description, now)
        await self.events.publish(
            Event(
                "memory.activity.recorded",
                {"date": day.isoformat(), "text": description, "scene": scene},
            )
        )

    async def start_course(self, title: str, session_id: str | None = None) -> CourseState:
        state = self.courses.create(title, session_id=session_id).state
        self.display_scene.force("course")
        await self.events.publish(Event("course.started", state.as_dict()))
        return state

    async def finish_course(self, session_id: str) -> CourseState:
        session = self.courses.open(session_id)
        if session.state.status == CourseStatus.RECORDING:
            await self._generate_final_course_summary(session)
        output_root = self.settings.courses.output_root or (desktop_path() / "Jarvis-Courses")
        session.finalize(output_root)
        state = session.state
        if session_id == self._auto_course_id:
            self._auto_course_id = None
            self._non_course_streak = 0
            self._non_course_started_at = None
            self._last_course_transcript = ""
        self._last_course_interaction = ""
        self._last_course_interaction_at = float("-inf")
        await self.events.publish(Event("course.finished", state.as_dict()))
        return state

    async def add_course_keyframe(
        self,
        session_id: str,
        image_base64: str,
        timestamp_ms: int,
        extension: str,
        metadata: dict[str, Any],
    ) -> CourseState:
        try:
            frame = base64.b64decode(image_base64, validate=True)
        except (binascii.Error, ValueError) as exc:
            raise ValueError("keyframe is not valid base64") from exc
        if not frame or len(frame) > 4 * 1024 * 1024:
            raise ValueError("keyframe must be between 1 byte and 4 MiB")
        normalized = extension.casefold()
        signatures = {
            "png": frame.startswith(b"\x89PNG\r\n\x1a\n"),
            "jpg": frame.startswith(b"\xff\xd8\xff"),
            "jpeg": frame.startswith(b"\xff\xd8\xff"),
            "webp": frame.startswith(b"RIFF") and frame[8:12] == b"WEBP",
        }
        if not signatures.get(normalized, False):
            raise ValueError(f"keyframe bytes do not match .{normalized}")
        session = self.courses.open(session_id)
        session.add_keyframe(
            frame,
            timestamp_ms=timestamp_ms,
            extension=normalized,
            metadata=metadata,
        )
        state = session.state
        await self.events.publish(
            Event(
                "course.keyframe.recorded",
                {"id": session_id, "timestamp_ms": timestamp_ms, "count": len(state.keyframes)},
            )
        )
        return state

    async def get_course(self, session_id: str) -> CourseState:
        return self.courses.open(session_id).state

    async def list_courses(self) -> list[CourseState]:
        return [session.state for session in self.courses.sessions()]

    async def _on_native_event(self, payload: dict[str, Any]) -> None:
        topic = str(payload.get("type", "native.event"))
        if topic == "screen.idle":
            self._screen_idle = True
            self._pending_duplex_fragment = None
            self._discarding_duplex_fragment = None
            await self.events.publish(Event(topic, payload))
            return
        if topic == "screen.idle.reminder":
            await self.events.publish(Event(topic, payload))
            if self._screen_idle:
                await self.events.publish(
                    Event(
                        "assistant.message",
                        {
                            "text": random.choice(SCREEN_IDLE_MESSAGES),
                            "source": "screen_idle",
                        },
                    )
                )
            return
        if topic == "screen.active":
            self._screen_idle = False
            self._pending_duplex_fragment = None
            self._discarding_duplex_fragment = None
            await self.events.publish(Event(topic, payload))
            return
        if topic == "perception.completed":
            if self._screen_idle:
                return
            await self._handle_perception(payload)
            return
        if topic == "duplex.decision":
            await self.events.publish(Event(topic, payload))
            if self._screen_idle:
                return
            if payload.get("decision") != "speak" or payload.get("ok") is not True:
                if payload.get("decision") == "listen":
                    self._pending_duplex_fragment = None
                    self._discarding_duplex_fragment = None
                return
            session_id = payload.get("session_id")
            now = time.monotonic()
            assembled = self._assemble_duplex_message(
                str(payload.get("text", "")), session_id, now
            )
            if not assembled:
                return
            text = self._clean_duplex_message(
                assembled,
                require_proactive_value=False,
            )
            if not text:
                return
            while self._recent_duplex_messages and (
                now - self._recent_duplex_messages[0][1] >= 30.0
            ):
                self._recent_duplex_messages.popleft()
            if any(
                _texts_are_similar(text, previous) and now - emitted_at < 10.0
                for previous, emitted_at in self._recent_duplex_messages
            ):
                return
            self._recent_duplex_messages.append((text, now))
            self._recent_assistant_messages.append((text, now))
            await self.events.publish(
                Event(
                    "assistant.message",
                    {
                        "text": text,
                        "source": "duplex",
                        "session_id": payload.get("session_id"),
                    },
                )
            )
            return
        if topic == "duplex.stopped":
            if payload.get("session_id") == self._duplex_session_id:
                self._duplex_session_id = None
                self._duplex_instruction = ""
        await self.events.publish(Event(topic, payload))

    @staticmethod
    def _parse_perception(text: str) -> dict[str, Any]:
        start = text.find("{")
        if start < 0:
            raise ValueError("perception response contains no JSON object")
        source = text[start:]
        try:
            value, _ = json.JSONDecoder().raw_decode(source)
        except json.JSONDecodeError:
            value = OrchestrationService._recover_truncated_perception(source)
        if not isinstance(value, dict):
            raise ValueError("perception response is not an object")
        scene = str(value.get("scene", "other")).casefold()
        if scene not in {"game", "course", "other"}:
            scene = "other"
        try:
            confidence = max(0.0, min(1.0, float(value.get("confidence", 0.0))))
        except (TypeError, ValueError):
            confidence = 0.0
        raw_evidence = value.get("scene_evidence", {})
        evidence = raw_evidence if isinstance(raw_evidence, dict) else {}
        scene_evidence = {
            key: evidence.get(key) is True
            for key in (
                "game_surface",
                "interactive_gameplay",
                "game_video_or_stream",
                "fullscreen_game_media",
                "active_instruction",
                "course_surface",
                "instructional_audio",
                "ordinary_browsing",
                "non_game_surface",
            )
        }
        barrage_pending = value.get("barrage_pending") is True
        classification_recovered = value.get("classification_recovered") is True
        barrage_source = str(value.get("barrage_source", "")).strip()
        if barrage_source not in {
            "pending",
            "model",
            "fallback",
            "missing_generation",
        }:
            barrage_source = ""
        barrage_fallback_reason = str(
            value.get("barrage_fallback_reason", "")
        ).strip()[:64]
        barrage = str(value.get("barrage", "")).strip()
        observation = str(value.get("observation", "")).strip()
        course_transcript = str(value.get("course_transcript", "")).strip()
        course_note = str(value.get("course_note", "")).strip()
        course_interaction = str(value.get("course_interaction", "")).strip()
        assistant_message = str(value.get("assistant_message", "")).strip()
        game_surface = scene_evidence["game_surface"]
        passive_game_media = scene_evidence["game_video_or_stream"]
        fullscreen_game_media = scene_evidence["fullscreen_game_media"]
        if scene == "game":
            interactive = scene_evidence["interactive_gameplay"]
            if not evidence and (
                barrage or value.get("barrage_candidates") or assistant_message
            ):
                interactive = True
            valid_game_scene = (
                not scene_evidence["non_game_surface"]
                and (
                    interactive
                    or (game_surface and not passive_game_media)
                    or (passive_game_media and fullscreen_game_media)
                )
            )
            if confidence < 0.72 or not valid_game_scene:
                scene = "other"
        elif scene == "course":
            active_instruction = scene_evidence["active_instruction"]
            course_surface = scene_evidence["course_surface"]
            instructional_audio = scene_evidence["instructional_audio"]
            if not evidence and (course_transcript or course_note):
                active_instruction = True
                instructional_audio = bool(course_transcript)
                course_surface = bool(course_note)
            # Instructional speech establishes active teaching even when the lecturer
            # is not visible. Browser content needs corroboration from both modalities.
            active_instruction = active_instruction or instructional_audio
            browsing_without_course_corroboration = scene_evidence[
                "ordinary_browsing"
            ] and not (course_surface and instructional_audio)
            if (
                confidence < 0.78
                or not active_instruction
                or not (course_surface or instructional_audio)
                or browsing_without_course_corroboration
            ):
                scene = "other"
        if scene == "game" and not barrage:
            barrage = assistant_message or course_note
        raw_barrage_candidates = value.get("barrage_candidates", [])
        barrage_candidates: list[str] = []
        candidates = [
            barrage,
            *(raw_barrage_candidates if isinstance(raw_barrage_candidates, list) else []),
        ]
        for candidate in candidates:
            candidate_text = str(candidate).strip()[:30]
            if candidate_text and candidate_text not in barrage_candidates:
                barrage_candidates.append(candidate_text)
        if scene == "game" and not barrage_candidates and not barrage_pending:
            barrage_source = "missing_generation"
            barrage_fallback_reason = barrage_fallback_reason or "empty_candidates"
        elif scene == "game" and barrage_pending:
            barrage_source = "pending"
        elif scene == "game" and barrage_candidates and not barrage_source:
            barrage_source = "model"
        return {
            "scene": scene,
            "confidence": confidence,
            "scene_evidence": scene_evidence,
            "barrage_pending": barrage_pending,
            "classification_recovered": classification_recovered,
            "barrage_source": barrage_source,
            "barrage_fallback_reason": barrage_fallback_reason,
            "observation": observation[:300],
            "barrage": barrage[:30] if scene == "game" else "",
            "barrage_candidates": barrage_candidates[:4] if scene == "game" else [],
            "course_transcript": course_transcript[:2000],
            "course_note": course_note[:2000],
            "course_title": str(value.get("course_title", "")).strip()[:128],
            "course_interaction": course_interaction[:100],
            "capture_keyframe": value.get("capture_keyframe") is True,
            "keyframe_note": str(value.get("keyframe_note", "")).strip()[:300],
            "assistant_message": assistant_message[:500],
        }

    @staticmethod
    def _has_game_entry_evidence(result: dict[str, Any]) -> bool:
        evidence = result["scene_evidence"]
        return evidence["game_surface"] and (
            evidence["interactive_gameplay"]
            or (
                evidence["game_video_or_stream"]
                and evidence["fullscreen_game_media"]
            )
        )

    @staticmethod
    def _recover_truncated_perception(source: str) -> dict[str, Any]:
        """Recover only the leading scene contract from a truncated model response."""
        scene_match = re.search(r'"scene"\s*:\s*"(game|course|other)"', source)
        confidence_match = re.search(
            r'"confidence"\s*:\s*(-?(?:\d+(?:\.\d*)?|\.\d+))', source
        )
        if not scene_match or not confidence_match:
            raise json.JSONDecodeError("incomplete perception JSON", source, len(source))

        evidence: dict[str, bool] = {}
        evidence_keys = (
            "game_surface",
            "interactive_gameplay",
            "game_video_or_stream",
            "fullscreen_game_media",
            "active_instruction",
            "course_surface",
            "instructional_audio",
            "ordinary_browsing",
            "non_game_surface",
        )
        for key in evidence_keys:
            match = re.search(rf'"{key}"\s*:\s*(true|false)', source)
            if match:
                evidence[key] = match.group(1) == "true"

        scene = scene_match.group(1)
        required_evidence = {
            "game": {
                "game_surface",
                "interactive_gameplay",
                "game_video_or_stream",
                "fullscreen_game_media",
            },
            "course": {
                "active_instruction",
                "course_surface",
                "instructional_audio",
                "ordinary_browsing",
            },
            "other": set(),
        }[scene]
        if not required_evidence.issubset(evidence):
            raise json.JSONDecodeError("incomplete scene evidence", source, len(source))

        value: dict[str, Any] = {
            "scene": scene,
            "confidence": float(confidence_match.group(1)),
            "scene_evidence": evidence,
        }
        for key in (
            "observation",
            "course_transcript",
            "course_note",
            "course_title",
            "course_interaction",
            "keyframe_note",
        ):
            match = re.search(rf'"{key}"\s*:\s*("(?:\\.|[^"\\])*")', source)
            if match:
                value[key] = json.loads(match.group(1))
        return value

    def _prune_recent_barrages(self, now: float) -> None:
        history_seconds = max(
            self.settings.interaction.game_barrage_repeat_seconds,
            self.settings.interaction.game_barrage_similar_seconds,
        )
        cutoff = now - history_seconds
        while self._recent_barrages and self._recent_barrages[0][1] < cutoff:
            self._recent_barrages.popleft()

    def _barrage_is_available(self, candidate: str, now: float) -> bool:
        normalized_candidate = _normalize_text(candidate)
        return not any(
            (
                normalized_candidate == _normalize_text(previous)
                and now - emitted_at
                < self.settings.interaction.game_barrage_repeat_seconds
            )
            or (
                _texts_are_similar(candidate, previous)
                and now - emitted_at
                < self.settings.interaction.game_barrage_similar_seconds
            )
            for previous, emitted_at in self._recent_barrages
        )

    def _rank_barrage_candidates(self, candidates: Sequence[str], now: float) -> list[str]:
        self._prune_recent_barrages(now)
        ranked: list[tuple[int, float, int, str]] = []
        for index, candidate in enumerate(candidates):
            if not self._barrage_is_available(candidate, now):
                continue
            normalized_candidate = _normalize_text(candidate)
            recent_similarity = max(
                (
                    SequenceMatcher(
                        None, normalized_candidate, _normalize_text(previous)
                    ).ratio()
                    for previous, _ in self._recent_barrages
                ),
                default=0.0,
            )
            ranked.append(
                (_barrage_quality_penalty(candidate), recent_similarity, index, candidate)
            )
        selected: list[str] = []
        for _, _, _, candidate in sorted(ranked):
            if any(_texts_are_similar(candidate, previous) for previous in selected):
                continue
            selected.append(candidate)
        return selected

    async def _cancel_barrage_sequence(self) -> None:
        task = self._barrage_task
        self._barrage_task = None
        if task is None:
            return
        task.cancel()
        try:
            await task
        except asyncio.CancelledError:
            pass

    async def _emit_barrage(self, text: str, metadata: dict[str, Any]) -> bool:
        if self.display_scene.current != "game":
            return False
        now = time.monotonic()
        self._prune_recent_barrages(now)
        if not self._barrage_is_available(text, now):
            return False
        self._recent_barrages.append((text, now))
        await self.events.publish(Event("barrage.generated", {"text": text, **metadata}))
        return True

    async def _emit_barrage_sequence(
        self, candidates: Sequence[str], metadata: dict[str, Any]
    ) -> None:
        try:
            for candidate in candidates:
                await asyncio.sleep(
                    self.settings.interaction.game_barrage_interval_seconds
                )
                if self.display_scene.current != "game":
                    return
                await self._emit_barrage(candidate, metadata)
        finally:
            if asyncio.current_task() is self._barrage_task:
                self._barrage_task = None

    async def _start_barrage_sequence(
        self, candidates: Sequence[str], metadata: dict[str, Any]
    ) -> None:
        await self._cancel_barrage_sequence()
        if not candidates or not await self._emit_barrage(candidates[0], metadata):
            return
        if len(candidates) > 1:
            self._barrage_task = asyncio.create_task(
                self._emit_barrage_sequence(candidates[1:], metadata),
                name="jarvis-game-barrage-sequence",
            )

    async def _handle_perception(self, payload: dict[str, Any]) -> None:
        try:
            result = self._parse_perception(str(payload.get("text", "")))
        except (ValueError, json.JSONDecodeError) as exc:
            await self.events.publish(
                Event(
                    "perception.failed",
                    {"error": str(exc), "request_id": payload.get("request_id")},
                )
            )
            return

        scene = result["scene"]
        game_entry_rejected = (
            scene == "game"
            and self.display_scene.current != "game"
            and not self._has_game_entry_evidence(result)
        )
        if game_entry_rejected:
            scene = "other"
            result["barrage"] = ""
            result["barrage_candidates"] = []
            result["barrage_pending"] = False
            result["barrage_source"] = ""
            result["barrage_fallback_reason"] = ""
        result["game_entry_rejected"] = game_entry_rejected
        now = time.monotonic()
        uncertain_game_exit = (
            self.display_scene.current == "game"
            and scene == "other"
            and not result["scene_evidence"]["non_game_surface"]
            and not result["scene_evidence"]["ordinary_browsing"]
        )
        exit_samples = None
        if self.display_scene.current == "game" and scene != "game":
            exit_samples = (
                self.settings.scene.game_uncertain_exit_samples
                if uncertain_game_exit
                else self.settings.scene.game_exit_samples
            )
        display_scene = self.display_scene.observe(scene, exit_samples=exit_samples)
        result["observed_scene"] = scene
        result["scene"] = display_scene
        result["uncertain_game_exit"] = uncertain_game_exit
        available_candidates: list[str] = []
        if scene == "game":
            available_candidates = self._rank_barrage_candidates(
                result["barrage_candidates"], now
            )
            result["barrage"] = available_candidates[0] if available_candidates else ""

        await self._record_memory_activity(result, now)
        await self.events.publish(Event("perception.completed", result))
        if scene == "other" and display_scene == "other":
            await self._emit_ordinary_perception_message(result, now)
        if scene != "game" or display_scene != "game":
            await self._cancel_barrage_sequence()
        elif available_candidates:
            await self._start_barrage_sequence(
                available_candidates,
                {
                    "confidence": result["confidence"],
                    "source": result["barrage_source"],
                    "fallback_reason": result["barrage_fallback_reason"],
                },
            )

        if scene == "course" and display_scene != "course":
            self._pending_course_results.append(dict(result))
        elif scene != "course" and display_scene != "course":
            self._pending_course_results.clear()

        if scene == "course" and display_scene == "course":
            self._non_course_streak = 0
            self._non_course_started_at = None
            confirmed_results = [*self._pending_course_results, result]
            self._pending_course_results.clear()
            for confirmed_result in confirmed_results:
                await self._handle_confirmed_course_perception(confirmed_result, now)
        elif self._auto_course_id:
            if self._non_course_started_at is None:
                self._non_course_started_at = now
            self._non_course_streak += 1
            outside_course_long_enough = (
                now - self._non_course_started_at
                >= self.settings.courses.exit_grace_seconds
            )
            if (
                self._non_course_streak >= self.settings.courses.exit_samples
                and outside_course_long_enough
            ):
                await self._finish_auto_course()

    async def _emit_ordinary_perception_message(
        self, result: dict[str, Any], now: float
    ) -> None:
        cooldown = self.settings.interaction.ordinary_bubble_cooldown_seconds
        history_window = max(60.0, cooldown * 3)
        while self._recent_assistant_messages and (
            now - self._recent_assistant_messages[0][1] >= history_window
        ):
            self._recent_assistant_messages.popleft()
        if self._recent_assistant_messages and (
            now - self._recent_assistant_messages[-1][1] < cooldown
        ):
            return
        message = self._clean_duplex_message(
            str(result.get("assistant_message", "")),
            require_proactive_value=False,
        )
        if not message or any(
            _texts_are_similar(message, previous)
            for previous, _ in self._recent_assistant_messages
        ):
            return
        self._recent_assistant_messages.append((message, now))
        await self.events.publish(
            Event(
                "assistant.message",
                {
                    "text": message,
                    "source": "perception",
                    "confidence": result["confidence"],
                },
            )
        )

    async def _handle_confirmed_course_perception(
        self, result: dict[str, Any], now: float
    ) -> None:
        await self._record_course_perception(result)
        valid_note = self._clean_course_note(str(result["course_note"]))
        valid_transcript = self._clean_course_transcript(str(result["course_transcript"]))
        knowledge_source = valid_note or valid_transcript
        raw_interaction = str(result["course_interaction"])
        message = (
            self._clean_course_interaction(raw_interaction) if knowledge_source else ""
        )
        if not message and knowledge_source and not raw_interaction.strip():
            message = self._course_note_interaction(knowledge_source)
        cooldown_elapsed = (
            now - self._last_course_interaction_at
            >= self.settings.interaction.course_bubble_cooldown_seconds
        )
        if (
            message
            and message != self._last_course_interaction
            and cooldown_elapsed
        ):
            self._last_course_interaction = message
            self._last_course_interaction_at = now
            await self.events.publish(
                Event(
                    "course.interaction",
                    {"text": message, "confidence": result["confidence"]},
                )
            )

    async def _record_course_perception(self, result: dict[str, Any]) -> None:
        transcript = self._clean_course_transcript(str(result["course_transcript"]))
        note = self._clean_course_note(str(result["course_note"]))
        capture_keyframe = bool(result["capture_keyframe"])
        if not transcript and not note and not capture_keyframe:
            return
        if not self._auto_course_id:
            recording = [
                session
                for session in self.courses.sessions()
                if session.state.status == CourseStatus.RECORDING
            ]
            if recording:
                session = recording[-1]
            else:
                stamp = datetime.now(UTC).strftime("%Y%m%d-%H%M%S")
                title = str(result["course_title"]) or f"自动网课记录 {stamp}"
                session = self.courses.create(title, session_id=f"auto-{stamp}")
                await self.events.publish(Event("course.started", session.state.as_dict()))
            self._auto_course_id = session.state.id

        session = self.courses.open(self._auto_course_id)

        transcript_delta = self._transcript_delta(self._last_course_transcript, transcript)
        if transcript_delta:
            state = session.append_transcript(transcript_delta)
            self._last_course_transcript = transcript
            await self.events.publish(
                Event(
                    "course.transcript.recorded",
                    {"id": state.id, "transcript": transcript_delta},
                )
            )

        state = session.state
        if capture_keyframe:
            await self._request_course_keyframe(state, str(result["keyframe_note"]))

    async def _generate_final_course_summary(self, session: Any) -> None:
        transcript = session.transcript_path.read_text(encoding="utf-8").strip()
        session.update_summary("")
        if not transcript:
            return

        try:
            chunks = self._split_transcript(transcript)
            if len(chunks) == 1:
                source = chunks[0]
            else:
                extracted = []
                for chunk in chunks:
                    extracted.append(
                        await self._ask_course_summarizer(build_course_chunk_prompt(chunk))
                    )
                source = "\n".join(filter(None, extracted))

            summary = await self._ask_course_summarizer(
                build_final_course_summary_prompt(source)
            )
            headings = re.findall(r"^###\s+(.+)$", summary, flags=re.MULTILINE)
            if headings == ["课程概览"] and "尚未进入具体知识讲解" not in summary:
                summary += "\n\n本段尚未进入具体知识讲解。"
            state = session.update_summary(summary[:6000])
        except (RuntimeError, TimeoutError, ValueError) as exc:
            await self.events.publish(
                Event("course.summary.failed", {"id": session.state.id, "error": str(exc)})
            )
            return

        await self.events.publish(
            Event("course.summary.updated", {"id": state.id, "summary": state.summary})
        )

    async def _ask_course_summarizer(self, instruction: str) -> str:
        response = await self.native_client.request(
            "ask", {"text": "[[JARVIS_TEXT_ONLY]]\n" + instruction}
        )
        text = str(response.get("text", "")).strip()
        text = re.sub(r"^```(?:markdown)?\s*|\s*```$", "", text, flags=re.IGNORECASE)
        if not text:
            raise ValueError("course summary response is empty")
        return text

    @staticmethod
    def _split_transcript(transcript: str, limit: int = 3200) -> list[str]:
        chunks: list[str] = []
        current: list[str] = []
        length = 0
        for line in filter(None, map(str.strip, transcript.splitlines())):
            if current and length + len(line) + 1 > limit:
                chunks.append("\n".join(current))
                current, length = [], 0
            current.append(line)
            length += len(line) + 1
        if current:
            chunks.append("\n".join(current))
        return chunks

    @staticmethod
    def _clean_course_transcript(transcript: str) -> str:
        return re.sub(r"\s+", " ", transcript).strip()[:2000]

    @staticmethod
    def _transcript_delta(previous: str, current: str) -> str:
        if not current or current == previous or current in previous:
            return ""
        max_overlap = min(len(previous), len(current))
        for size in range(max_overlap, 3, -1):
            if previous.endswith(current[:size]):
                return current[size:].lstrip(" ，。！？；：,.!?;:")
        return current

    @staticmethod
    def _clean_course_note(note: str) -> str:
        cleaned = re.sub(r"\s+", " ", note).strip().removeprefix("- ")
        lowered = cleaned.casefold()
        process_markers = (
            "metadata:",
            "electron-desktop",
            "正在查看",
            "当前界面",
            "界面显示",
            "当前屏幕",
            "屏幕显示",
            "文件夹",
            "文件列表",
            "用户可能",
            "视频播放器",
            "老师在黑板",
            "教师在黑板",
            "i can see",
            "the screen shows",
            "currently viewing",
        )
        if any(marker in lowered for marker in process_markers):
            return ""
        return cleaned[:2000]

    @staticmethod
    def _clean_course_interaction(message: str) -> str:
        cleaned = re.sub(r"\s+", " ", message).strip()
        generic_markers = (
            "这课很枯燥",
            "课程很枯燥",
            "内容很枯燥",
            "基础很重要",
            "内容很重要",
            "知识很重要",
            "认真听",
            "坚持一下",
            "继续坚持",
            "加油",
            "慢慢来",
            "别走神",
            "不要走神",
            "打好基础",
            "老师讲得",
        )
        process_comment = re.search(
            r"(?:主讲人|讲师|老师).{0,10}(?:提到|提醒|正在)|"
            r"课程(?:内容|结构|安排|版本)|干货|拓展内容|做好笔记",
            cleaned,
        )
        if (
            len(cleaned) < 8
            or process_comment
            or any(marker in cleaned for marker in generic_markers)
        ):
            return ""
        return cleaned[:100]

    def _assemble_duplex_message(
        self, message: str, session_id: Any, now: float
    ) -> str:
        cleaned = re.sub(r"\s+", " ", message).strip()
        resolved_session = str(session_id) if session_id is not None else None
        if resolved_session != AMBIENT_DUPLEX_SESSION_ID:
            return cleaned

        discarding = self._discarding_duplex_fragment
        if discarding is not None:
            discarded_session, discarded_at = discarding
            if discarded_session == resolved_session and now - discarded_at <= 3.0:
                if cleaned and not re.search(r"[。！!?？]$", cleaned):
                    self._discarding_duplex_fragment = (resolved_session, now)
                else:
                    self._discarding_duplex_fragment = None
                return ""
            self._discarding_duplex_fragment = None

        pending = self._pending_duplex_fragment
        self._pending_duplex_fragment = None
        if pending is not None:
            pending_session, pending_text, pending_at = pending
            if pending_session == resolved_session and now - pending_at <= 3.0:
                cleaned = pending_text + cleaned

        if not cleaned:
            return ""
        contaminated = re.search(
            r'[{}]|(?:^|[,\s])[A-Za-z_][A-Za-z0-9_]*"\s*:'
            r'|"[A-Za-z_][A-Za-z0-9_]*"\s*:',
            cleaned,
        )
        broken_start = re.search(
            r"^[、，。；：）】\]}>]|^(?:和|与|及|以及|而且|但是|不过|的)(?!确)",
            cleaned,
        )
        if contaminated or (pending is None and broken_start):
            if not re.search(r"[。！!?？]$", cleaned):
                self._discarding_duplex_fragment = (resolved_session, now)
            return ""
        if not re.search(r"[。！!?？]$", cleaned):
            if len(cleaned) <= 80:
                self._pending_duplex_fragment = (resolved_session, cleaned, now)
            else:
                self._discarding_duplex_fragment = (resolved_session, now)
            return ""
        return cleaned

    @staticmethod
    def _clean_duplex_message(
        message: str, *, require_proactive_value: bool = True
    ) -> str:
        cleaned = re.sub(r"\s+", " ", message).strip()
        if len(cleaned) < 6 or "？" in cleaned or "?" in cleaned:
            return ""
        unsupported_offer = re.search(
            r"需要我|要不要(?:我)?|是否需要|我(?:可以|来|能)(?:帮|替|为)|"
            r"让我(?:帮|来)|帮你|替你|为你(?:打开|搜索|整理|处理|操作)|"
            r"随时(?:告诉|叫|找)我|交给我",
            cleaned,
        )
        narration_probe = re.sub(
            r"^(?:好的|明白|收到)[，,。!！\s]*", "", cleaned, count=1
        )
        routine_narration = re.search(
            r"^[、，。；：）】\]}>]|^(?:和|与|及|以及|而且|但是|不过|的)(?!确)|"
            r"^(?:当前|现在)?(?:正在|已打开|打开了|切换到|进入了|已经进入|开始查看)|"
            r"^(?:当前|现在)(?:用户|你|您)?(?:正在)?(?:浏览|查看|观看|阅读|使用|停留|播放)|"
            r"^(?:用户|你|您)(?:正在|在)?(?:浏览|查看|观看|阅读|使用|停留|播放)|"
            r"^(?:当前|现在)?显示|^操作无(?:明显)?|"
            r"^(?:画面|页面|视频|屏幕)(?:中|里|上)?(?:显示|出现|开始|正在|讲解|播放|内容|是)|"
            r"(?:^|[，,；;。])(?:当前|现在)?(?:页面|画面|屏幕|界面)(?:中|里|上)?"
            r"(?:显示|包含|出现|列出|展示)|"
            r"^(?:你|您|主人|用户)(?:正在|在)|"
            r"^(?:屏幕|画面|界面|桌面)(?:中|上|显示|有)|"
            r"正在为(?:你|您)播放|"
            r"(?:文件|列表|内容|信息)(?:较多|清晰|已经显示)|"
            r"(?:光标|鼠标指针)|"
            r"(?:桌面图标|快捷方式).{0,20}(?:打开|排列|显示)|"
            r"(?:准备|打算)(?:继续|开始|打开|查看|往下)|"
            r"^(?:这|该)?(?:新闻|文章|视频|页面|内容|帖子).{0,12}"
            r"(?:是|关于|讲(?:的)?是|介绍|报道|涉及)",
            narration_probe,
        )
        uncertain = re.search(r"看起来|似乎|可能是|大概|也许|推测|猜测", cleaned)
        if unsupported_offer or routine_narration or uncertain:
            return ""
        proactive_value = re.search(
            r"完成|成功|失败|报错|错误|异常|中断|超时|已保存|已下载|"
            r"构建(?:通过|失败)|测试(?:通过|失败)|风险|危险|授权|权限|"
            r"截止|到期|过期|不足|冲突|占用|泄露|断开|不可用|无法|"
            r"找不到|未找到|空间(?:不足|已满)|建议|注意|留意|提醒|核对|"
            r"确认|避免|谨防|变化|新增|减少|升高|降低",
            cleaned,
        )
        if require_proactive_value and not proactive_value:
            return ""
        first_sentence = re.match(r"^(.{6,60}?[。！!])", cleaned)
        if first_sentence and first_sentence.group(1) != cleaned:
            return first_sentence.group(1)
        if len(cleaned) > 60:
            return ""
        return cleaned

    @classmethod
    def _course_note_interaction(cls, note: str) -> str:
        cleaned = cls._clean_course_note(note)
        if not cleaned:
            return ""
        sentences = [
            sentence.strip(" ，。！？；：,.!?;:")
            for sentence in re.split(r"[。！？；\n]+", cleaned)
            if sentence.strip(" ，。！？；：,.!?;:")
        ]
        candidate = next((sentence for sentence in sentences if len(sentence) >= 10), "")
        return candidate[:80] + ("。" if candidate else "")

    async def _request_course_keyframe(self, state: CourseState, note: str) -> None:
        if len(state.keyframes) >= self.settings.courses.max_keyframes:
            return
        now = time.monotonic()
        previous = self._last_keyframe_requested_at.get(state.id)
        if previous is not None and (
            now - previous < self.settings.courses.keyframe_min_interval_seconds
        ):
            return
        self._last_keyframe_requested_at[state.id] = now
        created_at = datetime.fromisoformat(state.created_at.replace("Z", "+00:00"))
        timestamp_ms = max(0, int((datetime.now(UTC) - created_at).total_seconds() * 1000))
        await self.events.publish(
            Event(
                "course.keyframe.requested",
                {"id": state.id, "timestamp_ms": timestamp_ms, "note": note.strip()[:300]},
            )
        )

    async def _finish_auto_course(self) -> None:
        session_id = self._auto_course_id
        if not session_id:
            return
        await self.finish_course(session_id)

    @property
    def native_connected(self) -> bool:
        return bool(getattr(self.native_client, "running", False))

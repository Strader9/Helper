from __future__ import annotations

import json
import math
import os
import re
import shutil
import tempfile
from collections import Counter
from collections.abc import Callable, Iterable, Mapping, Sequence
from dataclasses import dataclass
from datetime import UTC, date, datetime, timedelta, tzinfo
from pathlib import Path
from typing import Any
from uuid import uuid4

_TOKEN_RE = re.compile(r"[\w'-]+", re.UNICODE)


def _now() -> datetime:
    return datetime.now(UTC)


def _iso(value: datetime) -> str:
    return value.astimezone(UTC).isoformat().replace("+00:00", "Z")


def _parse(value: str) -> datetime:
    return datetime.fromisoformat(value.replace("Z", "+00:00"))


def _atomic_json(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    try:
        with os.fdopen(fd, "w", encoding="utf-8", newline="\n") as stream:
            json.dump(value, stream, ensure_ascii=False, indent=2, sort_keys=True)
            stream.write("\n")
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
    except BaseException:
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass
        raise


def _atomic_text(path: Path, value: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    try:
        with os.fdopen(fd, "w", encoding="utf-8", newline="\n") as stream:
            stream.write(value.rstrip() + "\n")
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
    except BaseException:
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass
        raise


def _atomic_bytes(path: Path, value: bytes) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    try:
        with os.fdopen(fd, "wb") as stream:
            stream.write(value)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
    except BaseException:
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass
        raise


def _tokens(text: str) -> list[str]:
    return [token.casefold() for token in _TOKEN_RE.findall(text)]


@dataclass(frozen=True)
class MemoryEvent:
    id: str
    timestamp: str
    kind: str
    text: str
    metadata: Mapping[str, Any]

    @classmethod
    def from_dict(cls, value: Mapping[str, Any]) -> MemoryEvent:
        return cls(
            id=str(value["id"]),
            timestamp=str(value["timestamp"]),
            kind=str(value["kind"]),
            text=str(value["text"]),
            metadata=dict(value.get("metadata", {})),
        )

    def as_dict(self) -> dict[str, Any]:
        return {
            "id": self.id,
            "timestamp": self.timestamp,
            "kind": self.kind,
            "text": self.text,
            "metadata": dict(self.metadata),
        }


@dataclass(frozen=True)
class SearchResult:
    event: MemoryEvent
    score: float


Summarizer = Callable[[Sequence[MemoryEvent], str | None], str]
FactExtractor = Callable[[Sequence[MemoryEvent]], Iterable[Mapping[str, Any] | str]]


class MemoryStore:
    """Crash-safe, dependency-free memory persisted as files.

    The event log is append-only JSONL. Derived summaries, facts and the search
    index are replace-on-write JSON files and can always be rebuilt.
    """

    def __init__(self, root: str | Path) -> None:
        self.root = Path(root)
        self.events_path = self.root / "events.jsonl"
        self.summary_path = self.root / "summary.json"
        self.facts_path = self.root / "facts.json"
        self.index_path = self.root / "index.json"
        self.daily_root = self.root / "daily"
        self.daily_images_root = self.root / "daily-images"
        self.root.mkdir(parents=True, exist_ok=True)

    def append(
        self,
        kind: str,
        text: str,
        metadata: Mapping[str, Any] | None = None,
        *,
        timestamp: datetime | None = None,
        event_id: str | None = None,
    ) -> MemoryEvent:
        if not kind.strip() or not text.strip():
            raise ValueError("kind and text must be non-empty")
        event = MemoryEvent(
            event_id or uuid4().hex, _iso(timestamp or _now()), kind, text, dict(metadata or {})
        )
        line = json.dumps(event.as_dict(), ensure_ascii=False, separators=(",", ":")) + "\n"
        with self.events_path.open("a", encoding="utf-8", newline="\n") as stream:
            stream.write(line)
            stream.flush()
            os.fsync(stream.fileno())
        return event

    def events(self) -> list[MemoryEvent]:
        if not self.events_path.exists():
            return []
        result: list[MemoryEvent] = []
        with self.events_path.open(encoding="utf-8") as stream:
            for number, line in enumerate(stream, 1):
                if not line.strip():
                    continue
                try:
                    result.append(MemoryEvent.from_dict(json.loads(line)))
                except (KeyError, TypeError, json.JSONDecodeError) as exc:
                    raise ValueError(f"invalid event at line {number}") from exc
        return result

    @staticmethod
    def event_day(event: MemoryEvent, timezone_value: tzinfo | None = None) -> date:
        target_timezone = timezone_value or datetime.now().astimezone().tzinfo
        return _parse(event.timestamp).astimezone(target_timezone).date()

    def events_for_day(
        self, day: date, *, timezone_value: tzinfo | None = None
    ) -> list[MemoryEvent]:
        return [event for event in self.events() if self.event_day(event, timezone_value) == day]

    def memory_days(self, *, timezone_value: tzinfo | None = None) -> list[date]:
        days = {self.event_day(event, timezone_value) for event in self.events()}
        if self.daily_root.exists():
            for path in self.daily_root.glob("????-??-??.md"):
                try:
                    days.add(date.fromisoformat(path.stem))
                except ValueError:
                    continue
        if self.daily_images_root.exists():
            for path in self.daily_images_root.iterdir():
                if not path.is_dir():
                    continue
                try:
                    days.add(date.fromisoformat(path.name))
                except ValueError:
                    continue
        return sorted(days, reverse=True)

    def daily_path(self, day: date) -> Path:
        return self.daily_root / f"{day.isoformat()}.md"

    def write_daily_memory(self, day: date, content: str) -> Path:
        path = self.daily_path(day)
        _atomic_text(path, content)
        return path

    def read_daily_memory(self, day: date) -> str | None:
        path = self.daily_path(day)
        return path.read_text(encoding="utf-8") if path.exists() else None

    def write_daily_image(
        self, day: date, filename: str, content: bytes, metadata: Mapping[str, Any]
    ) -> Path:
        if not re.fullmatch(r"[A-Za-z0-9_.-]+", filename):
            raise ValueError("invalid daily image filename")
        path = self.daily_images_root / day.isoformat() / filename
        _atomic_bytes(path, content)
        _atomic_json(path.with_suffix(path.suffix + ".json"), dict(metadata))
        return path

    def daily_images(self, day: date | None = None) -> list[dict[str, Any]]:
        roots = [self.daily_images_root / day.isoformat()] if day else (
            list(self.daily_images_root.iterdir()) if self.daily_images_root.exists() else []
        )
        result = []
        for root in roots:
            if not root.is_dir():
                continue
            for metadata_path in root.glob("*.json"):
                try:
                    value = json.loads(metadata_path.read_text(encoding="utf-8"))
                except (OSError, json.JSONDecodeError):
                    continue
                if not isinstance(value, dict):
                    continue
                filename = str(value.get("filename", ""))
                if not re.fullmatch(r"[A-Za-z0-9_.-]+", filename):
                    continue
                image_path = root / filename
                if image_path.is_file():
                    result.append(dict(value))
        return sorted(result, key=lambda item: str(item.get("created_at", "")), reverse=True)

    def daily_image_path(self, day: date, filename: str) -> Path:
        if not re.fullmatch(r"[A-Za-z0-9_.-]+", filename):
            raise FileNotFoundError(filename)
        path = self.daily_images_root / day.isoformat() / filename
        if not path.is_file():
            raise FileNotFoundError(filename)
        return path

    def write_summary(self, text: str, *, through_event_id: str | None = None) -> None:
        _atomic_json(
            self.summary_path,
            {"text": text, "through_event_id": through_event_id, "updated_at": _iso(_now())},
        )

    def read_summary(self) -> str | None:
        if not self.summary_path.exists():
            return None
        return str(json.loads(self.summary_path.read_text(encoding="utf-8"))["text"])

    def summarize(
        self, summarizer: Summarizer, *, events: Sequence[MemoryEvent] | None = None
    ) -> str:
        selected = list(events) if events is not None else self.events()
        summary = summarizer(selected, self.read_summary())
        self.write_summary(summary, through_event_id=selected[-1].id if selected else None)
        return summary

    def write_facts(self, facts: Iterable[Mapping[str, Any] | str]) -> None:
        normalized = [
            dict(fact) if isinstance(fact, Mapping) else {"text": str(fact)} for fact in facts
        ]
        _atomic_json(self.facts_path, {"facts": normalized, "updated_at": _iso(_now())})

    def read_facts(self) -> list[dict[str, Any]]:
        if not self.facts_path.exists():
            return []
        return list(json.loads(self.facts_path.read_text(encoding="utf-8")).get("facts", []))

    def extract_facts(self, extractor: FactExtractor) -> list[dict[str, Any]]:
        facts = [
            dict(f) if isinstance(f, Mapping) else {"text": str(f)}
            for f in extractor(self.events())
        ]
        self.write_facts(facts)
        return facts

    def rebuild_index(self) -> dict[str, Any]:
        events = self.events()
        documents = []
        document_frequency: Counter[str] = Counter()
        total_length = 0
        for event in events:
            counts = Counter(_tokens(f"{event.kind} {event.text}"))
            total_length += sum(counts.values())
            document_frequency.update(counts.keys())
            documents.append(
                {"id": event.id, "length": sum(counts.values()), "terms": dict(counts)}
            )
        manifest = {
            "version": 1,
            "event_count": len(events),
            "average_length": total_length / len(events) if events else 0.0,
            "document_frequency": dict(document_frequency),
            "documents": documents,
            "rebuilt_at": _iso(_now()),
        }
        _atomic_json(self.index_path, manifest)
        return manifest

    def search(self, query: str, *, limit: int = 10, kind: str | None = None) -> list[SearchResult]:
        if limit < 1:
            return []
        events = self.events()
        if not self.index_path.exists():
            index = self.rebuild_index()
        else:
            index = json.loads(self.index_path.read_text(encoding="utf-8"))
            if index.get("event_count") != len(events) or [e.id for e in events] != [
                d["id"] for d in index.get("documents", [])
            ]:
                index = self.rebuild_index()
        query_terms = Counter(_tokens(query))
        if not query_terms:
            return []
        count = max(1, int(index["event_count"]))
        average = float(index["average_length"]) or 1.0
        frequency = index["document_frequency"]
        by_id = {event.id: event for event in events}
        results = []
        for document in index["documents"]:
            event = by_id[document["id"]]
            if kind is not None and event.kind != kind:
                continue
            score = 0.0
            length = document["length"]
            for term, query_frequency in query_terms.items():
                term_frequency = document["terms"].get(term, 0)
                if not term_frequency:
                    continue
                inverse = math.log(
                    1.0 + (count - frequency.get(term, 0) + 0.5) / (frequency.get(term, 0) + 0.5)
                )
                score += (
                    query_frequency
                    * inverse
                    * (term_frequency * 2.2)
                    / (term_frequency + 1.2 * (0.25 + 0.75 * length / average))
                )
            if score:
                results.append(SearchResult(event, score))
        return sorted(results, key=lambda item: (-item.score, item.event.timestamp))[:limit]

    def retain(
        self,
        *,
        max_age: timedelta | None = None,
        max_events: int | None = None,
        now: datetime | None = None,
    ) -> int:
        events = self.events()
        retained = events
        if max_age is not None:
            cutoff = (now or _now()) - max_age
            retained = [event for event in retained if _parse(event.timestamp) >= cutoff]
        if max_events is not None:
            if max_events < 0:
                raise ValueError("max_events cannot be negative")
            retained = retained[-max_events:] if max_events else []
        removed = len(events) - len(retained)
        if removed:
            self._replace_events(retained)
            for derived in (self.summary_path, self.facts_path, self.index_path):
                derived.unlink(missing_ok=True)
            self.rebuild_index()
        return removed

    def _replace_events(self, events: Sequence[MemoryEvent]) -> None:
        self.root.mkdir(parents=True, exist_ok=True)
        fd, temporary = tempfile.mkstemp(prefix=".events.", dir=self.root)
        try:
            with os.fdopen(fd, "w", encoding="utf-8", newline="\n") as stream:
                for event in events:
                    stream.write(
                        json.dumps(event.as_dict(), ensure_ascii=False, separators=(",", ":"))
                        + "\n"
                    )
                stream.flush()
                os.fsync(stream.fileno())
            os.replace(temporary, self.events_path)
        except BaseException:
            try:
                os.unlink(temporary)
            except FileNotFoundError:
                pass
            raise

    def clear(self) -> None:
        if self.root.exists():
            shutil.rmtree(self.root)
        self.root.mkdir(parents=True, exist_ok=True)

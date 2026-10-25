from __future__ import annotations

import ctypes
import json
import os
import re
import shutil
import tempfile
from collections.abc import Callable, Mapping
from dataclasses import dataclass, field
from datetime import UTC, datetime
from enum import StrEnum
from pathlib import Path
from typing import Any
from uuid import uuid4


def _iso_now() -> str:
    return datetime.now(UTC).isoformat().replace("+00:00", "Z")


def atomic_write(path: Path, data: bytes) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    try:
        with os.fdopen(fd, "wb") as stream:
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
    except BaseException:
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass
        raise


def atomic_json(path: Path, value: Any) -> None:
    atomic_write(
        path, (json.dumps(value, ensure_ascii=False, indent=2, sort_keys=True) + "\n").encode()
    )


def desktop_path(*, override: str | Path | None = None) -> Path:
    """Resolve Desktop without assuming it is under the home directory."""
    configured = override or os.environ.get("JARVIS_DESKTOP_DIR")
    if configured:
        return Path(configured).expanduser().resolve()
    if os.name == "nt":
        from ctypes import wintypes

        class GUID(ctypes.Structure):
            _fields_ = [
                ("Data1", wintypes.DWORD),
                ("Data2", wintypes.WORD),
                ("Data3", wintypes.WORD),
                ("Data4", ctypes.c_ubyte * 8),
            ]

        desktop = GUID(
            0xB4BFCC3A,
            0xDB2C,
            0x424C,
            (ctypes.c_ubyte * 8)(0xB0, 0x29, 0x7F, 0xE9, 0x9A, 0x87, 0xC6, 0x41),
        )
        output = ctypes.c_wchar_p()
        result = ctypes.windll.shell32.SHGetKnownFolderPath(
            ctypes.byref(desktop), 0, None, ctypes.byref(output)
        )
        if result != 0:
            raise OSError(result, "SHGetKnownFolderPath(FOLDERID_Desktop) failed")
        try:
            if output.value is None:
                raise OSError("SHGetKnownFolderPath returned an empty Desktop path")
            return Path(output.value)
        finally:
            ctypes.windll.ole32.CoTaskMemFree(output)
    return Path.home() / "Desktop"


class CourseStatus(StrEnum):
    RECORDING = "recording"
    FINALIZING = "finalizing"
    COMPLETE = "complete"
    FAILED = "failed"


@dataclass
class CourseState:
    id: str
    title: str
    status: CourseStatus
    created_at: str
    updated_at: str
    summary: str = ""
    keyframes: list[dict[str, Any]] = field(default_factory=list)
    error: str | None = None
    output_path: str | None = None

    @classmethod
    def from_dict(cls, value: Mapping[str, Any]) -> CourseState:
        copied = dict(value)
        copied["status"] = CourseStatus(copied["status"])
        return cls(**copied)

    def as_dict(self) -> dict[str, Any]:
        value = dict(self.__dict__)
        value["status"] = self.status.value
        return value


RollingSummarizer = Callable[[str, str], str]


class CourseSession:
    """Durable recording session with recoverable finalization."""

    def __init__(self, path: str | Path) -> None:
        self.path = Path(path)
        self.state_path = self.path / "state.json"
        self.transcript_path = self.path / "transcript.md"
        self.frames_path = self.path / "frames"

    @classmethod
    def create(
        cls, root: str | Path, title: str, *, session_id: str | None = None
    ) -> CourseSession:
        if not title.strip():
            raise ValueError("title must be non-empty")
        session_id = session_id or uuid4().hex
        session = cls(Path(root) / session_id)
        session.frames_path.mkdir(parents=True, exist_ok=False)
        now = _iso_now()
        session._save(CourseState(session_id, title, CourseStatus.RECORDING, now, now))
        atomic_write(session.transcript_path, b"")
        return session

    @property
    def state(self) -> CourseState:
        return CourseState.from_dict(json.loads(self.state_path.read_text(encoding="utf-8")))

    def _save(self, state: CourseState) -> None:
        state.updated_at = _iso_now()
        atomic_json(self.state_path, state.as_dict())

    def append_transcript(
        self, text: str, *, summarizer: RollingSummarizer | None = None
    ) -> CourseState:
        state = self.state
        if state.status != CourseStatus.RECORDING:
            raise RuntimeError("session is not recording")
        with self.transcript_path.open("a", encoding="utf-8", newline="\n") as stream:
            stream.write(text)
            if text and not text.endswith("\n"):
                stream.write("\n")
            stream.flush()
            os.fsync(stream.fileno())
        if summarizer is not None:
            state.summary = summarizer(state.summary, text)
        self._save(state)
        return state

    def update_summary(self, summary: str) -> CourseState:
        state = self.state
        if state.status != CourseStatus.RECORDING:
            raise RuntimeError("session is not recording")
        state.summary = summary.strip()
        self._save(state)
        return state

    def add_keyframe(
        self,
        frame: bytes,
        *,
        timestamp_ms: int,
        extension: str = ".png",
        metadata: Mapping[str, Any] | None = None,
    ) -> dict[str, Any]:
        state = self.state
        if state.status != CourseStatus.RECORDING:
            raise RuntimeError("session is not recording")
        if timestamp_ms < 0 or not frame:
            raise ValueError("timestamp must be non-negative and frame non-empty")
        extension = "." + extension.lstrip(".").casefold()
        if not re.fullmatch(r"\.[a-z0-9]{1,8}", extension):
            raise ValueError("invalid frame extension")
        number = len(state.keyframes) + 1
        filename = f"{number:06d}-{timestamp_ms:012d}{extension}"
        atomic_write(self.frames_path / filename, frame)  # exact bytes; no transcoding
        item = {
            "filename": filename,
            "timestamp_ms": timestamp_ms,
            "metadata": dict(metadata or {}),
        }
        state.keyframes.append(item)
        self._save(state)
        return item

    def finalize(self, output_dir: str | Path, *, renderer: MarkdownRenderer | None = None) -> Path:
        state = self.state
        if state.status == CourseStatus.COMPLETE and state.output_path:
            return Path(state.output_path)
        if state.status not in {
            CourseStatus.RECORDING,
            CourseStatus.FINALIZING,
            CourseStatus.FAILED,
        }:
            raise RuntimeError(f"cannot finalize session in {state.status.value}")
        state.status, state.error = CourseStatus.FINALIZING, None
        self._save(state)
        output = Path(output_dir) / state.id / "README.md"
        try:
            (renderer or MarkdownRenderer()).render(self, output)
            state.status, state.output_path = CourseStatus.COMPLETE, str(output.resolve())
            self._save(state)
            return output
        except BaseException as exc:
            state.status, state.error = CourseStatus.FAILED, f"{type(exc).__name__}: {exc}"
            self._save(state)
            raise

    def discard(self) -> None:
        shutil.rmtree(self.path)


class MarkdownRenderer:
    def render(self, session: CourseSession, destination: str | Path) -> Path:
        state = session.state
        destination = Path(destination)
        lines = [
            f"# {state.title}",
            "",
            f"- Session: `{state.id}`",
            f"- Created: {state.created_at}",
            "",
        ]
        if state.summary:
            lines += ["## 课程总结", "", state.summary, ""]
        asset_dir = destination.parent / "images"
        asset_dir.mkdir(parents=True, exist_ok=True)
        if state.keyframes:
            lines += ["## Keyframes", ""]
            for frame in state.keyframes:
                source = session.frames_path / frame["filename"]
                target = asset_dir / frame["filename"]
                atomic_write(target, source.read_bytes())
                seconds = frame["timestamp_ms"] / 1000
                lines += [
                    f"### {seconds:.3f}s",
                    "",
                    f"![Keyframe at {seconds:.3f}s]({asset_dir.name}/{target.name})",
                    "",
                ]
                note = str(frame["metadata"].get("note", "")).strip()
                if note:
                    lines += [f"**画面说明：** {note}", ""]
        atomic_write(destination, ("\n".join(lines).rstrip() + "\n").encode("utf-8"))
        return destination


class CourseRepository:
    def __init__(self, root: str | Path) -> None:
        self.root = Path(root)
        self.root.mkdir(parents=True, exist_ok=True)

    def create(self, title: str, *, session_id: str | None = None) -> CourseSession:
        return CourseSession.create(self.root, title, session_id=session_id)

    def open(self, session_id: str) -> CourseSession:
        if not re.fullmatch(r"[A-Za-z0-9_-]+", session_id):
            raise ValueError("invalid session id")
        session = CourseSession(self.root / session_id)
        if not session.state_path.is_file():
            raise FileNotFoundError(session_id)
        return session

    def sessions(self) -> list[CourseSession]:
        return [
            CourseSession(path)
            for path in sorted(self.root.iterdir())
            if (path / "state.json").is_file()
        ]

    def recoverable(self) -> list[CourseSession]:
        return [
            session
            for session in self.sessions()
            if session.state.status in {CourseStatus.FINALIZING, CourseStatus.FAILED}
        ]

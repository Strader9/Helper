import pytest

from jarvis_backend.courses import CourseRepository, CourseStatus, MarkdownRenderer


def test_course_lifecycle_exact_frames_and_markdown(tmp_path):
    repository = CourseRepository(tmp_path / "sessions")
    session = repository.create("Python Lesson", session_id="lesson-1")
    session.append_transcript("Install Python.", summarizer=lambda old, new: old + new.strip())
    session.update_summary("- Install Python before creating the environment.")
    frame = b"\x89PNG\r\n\x1a\nexact-payload"
    item = session.add_keyframe(
        frame,
        timestamp_ms=1234,
        metadata={"source": "electron-desktop", "note": "The force diagram for F=ma."},
    )

    assert (session.frames_path / item["filename"]).read_bytes() == frame
    output = session.finalize(tmp_path / "courses")
    rendered = output.read_text(encoding="utf-8")
    assert "# Python Lesson" in rendered
    assert "Install Python before creating the environment." in rendered
    assert "1.234s" in rendered
    assert "课程总结" in rendered
    assert "画面说明" in rendered
    assert "Metadata:" not in rendered
    assert "electron-desktop" not in rendered
    assert "## Transcript" not in rendered
    assert output == tmp_path / "courses" / "lesson-1" / "README.md"
    assert (output.parent / "images" / item["filename"]).read_bytes() == frame
    assert repository.open("lesson-1").state.status == CourseStatus.COMPLETE
    assert session.finalize(tmp_path / "courses") == output


def test_failed_finalization_is_recoverable(tmp_path):
    repository = CourseRepository(tmp_path / "sessions")
    session = repository.create("Recover Me", session_id="recover-me")

    class BrokenRenderer(MarkdownRenderer):
        def render(self, session, destination):
            raise OSError("disk unavailable")

    with pytest.raises(OSError):
        session.finalize(tmp_path / "out", renderer=BrokenRenderer())
    assert session.state.status == CourseStatus.FAILED
    assert repository.recoverable()[0].state.id == "recover-me"

    output = session.finalize(tmp_path / "out")
    assert output.is_file()
    assert session.state.status == CourseStatus.COMPLETE

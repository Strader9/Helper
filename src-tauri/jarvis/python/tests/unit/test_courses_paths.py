from jarvis_backend.courses import desktop_path


def test_desktop_explicit_override(tmp_path, monkeypatch):
    explicit = tmp_path / "explicit desktop"
    monkeypatch.setenv("JARVIS_DESKTOP_DIR", str(tmp_path / "environment desktop"))
    assert desktop_path(override=explicit) == explicit.resolve()


def test_desktop_environment_override(tmp_path, monkeypatch):
    expected = tmp_path / "test desktop"
    monkeypatch.setenv("JARVIS_DESKTOP_DIR", str(expected))
    assert desktop_path() == expected.resolve()

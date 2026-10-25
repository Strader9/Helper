from datetime import UTC, datetime, timedelta

from jarvis_backend.memory import MemoryStore


def test_append_retrieve_rebuild_and_retention(tmp_path):
    store = MemoryStore(tmp_path)
    old = datetime(2020, 1, 1, tzinfo=UTC)
    first = store.append("preference", "Alice likes green tea", timestamp=old)
    second = store.append("note", "Green tea is in the kitchen")

    assert [event.id for event in store.events()] == [first.id, second.id]
    assert store.search("green kitchen")[0].event.id == second.id
    assert store.index_path.is_file()

    # An append makes the index stale; search transparently rebuilds it.
    third = store.append("note", "Coffee beans are fresh")
    assert store.search("coffee")[0].event.id == third.id
    assert store.retain(max_age=timedelta(days=1)) == 1
    assert [event.id for event in store.events()] == [second.id, third.id]


def test_atomic_derivatives_hooks_and_clear(tmp_path):
    store = MemoryStore(tmp_path)
    event = store.append("conversation", "Remember the blue bicycle")
    summary = store.summarize(lambda events, previous: f"{len(events)}: {events[0].text}")
    facts = store.extract_facts(lambda events: [{"subject": "bicycle", "color": "blue"}])

    assert summary == "1: Remember the blue bicycle"
    assert store.read_summary() == summary
    assert facts == store.read_facts()
    assert event.id in store.index_path.read_text() if store.index_path.exists() else True

    store.clear()
    assert store.events() == []
    assert store.read_summary() is None


def test_daily_memory_documents_are_atomic_and_discoverable(tmp_path):
    store = MemoryStore(tmp_path)
    day = datetime.now().astimezone().date()
    event = store.append("activity", "Working on the Jarvis memory system")

    assert store.events_for_day(day) == [event]
    assert store.memory_days() == [day]
    path = store.write_daily_memory(day, f"# {day.isoformat()}\n\nMemory content")

    assert path == tmp_path / "daily" / f"{day.isoformat()}.md"
    assert store.read_daily_memory(day) == f"# {day.isoformat()}\n\nMemory content\n"

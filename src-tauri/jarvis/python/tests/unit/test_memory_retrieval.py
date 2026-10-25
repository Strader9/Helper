from jarvis_backend.memory import MemoryStore


def test_bm25_ranks_term_frequency_and_filters_kind(tmp_path):
    store = MemoryStore(tmp_path)
    weak = store.append("chat", "python packaging")
    strong = store.append("lesson", "python python python packaging guide")
    store.append("chat", "gardening")

    results = store.search("python")
    assert {item.event.id for item in results} == {weak.id, strong.id}
    assert all(item.score > 0 for item in results)
    assert [item.event.id for item in store.search("python", kind="chat")] == [weak.id]
    assert store.search("!!!") == []

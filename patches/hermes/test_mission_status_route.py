"""Unit tests for mission-complete routing (no gateway needed)."""

from gateway.platforms.mission_status_route import (
    extract_origin_session,
    extract_project_slug,
    extract_status,
    format_mission_callback,
    is_routable_mission_status,
    resolve_live_session_id,
    resolve_mission_delivery_session,
)


class _FakeSessionDB:
    def __init__(self, sessions, resumes=None):
        self.sessions = sessions
        self.resumes = resumes or {}

    def resolve_resume_session_id(self, sid):
        return self.resumes.get(sid, sid)

    def resolve_session_id(self, sid):
        return sid if sid in self.sessions else None

    def get_session(self, sid):
        return self.sessions.get(sid)


class _FakeSessionDBWithMessages(_FakeSessionDB):
    """Variant exposing the message reader — ownership becomes enforceable."""

    def __init__(self, sessions, resumes=None, messages=None):
        super().__init__(sessions, resumes)
        self.messages = messages or {}
        self.appended = []

    def get_messages(self, sid):
        return self.messages.get(sid, [])

    def append_message(self, session_id, role, content):
        self.appended.append((session_id, role, content))
        self.messages.setdefault(session_id, []).append({"content": content})


def test_extracts_nested_project_and_origin():
    payload = {
        "mission_id": "acfb03d2-d088-46c3-a2a3-6576563f06cb",
        "status": "failed",
        "origin_session": "20260813_111430_1310a9",
        "project": {"project": "ec-defensive-research", "tags": []},
    }
    assert extract_origin_session(payload) == "20260813_111430_1310a9"
    assert extract_project_slug(payload) == "ec-defensive-research"
    assert extract_status(payload) == "failed"
    assert is_routable_mission_status(payload)


def test_acknowledged_is_not_routable():
    assert not is_routable_mission_status(
        {"mission_id": "x", "status": "acknowledged"}
    )
    assert not is_routable_mission_status({"status": "failed"})


def test_wake_session_is_used_as_is_without_child_walk():
    db = _FakeSessionDB(
        {
            "20260814_094937_8c5097": {"source": "desktop"},
            "3abf1a_review": {"source": "desktop"},
        },
        resumes={"20260814_094937_8c5097": "3abf1a_review"},
    )
    payload = {
        "mission_id": "acfb03d2",
        "status": "awaiting_user",
        "origin_session": "20260814_review_child",
        "project": "verity-benchmark",
        "wake_session": "20260814_094937_8c5097",
        "wake_source": "project",
    }
    assert resolve_mission_delivery_session(payload, db) == "20260814_094937_8c5097"


def test_prefers_live_origin_over_project():
    db = _FakeSessionDB(
        {"20260813_111430_1310a9": {"source": "desktop"}},
        resumes={"20260812_old": "20260813_111430_1310a9"},
    )
    payload = {
        "mission_id": "acfb03d2",
        "status": "failed",
        "origin_session": "20260812_old",
        "project": "coldcard-rng-cracker",
    }
    assert resolve_mission_delivery_session(payload, db) == "20260813_111430_1310a9"


def test_missing_origin_session_returns_none_without_project_store():
    db = _FakeSessionDB({})
    payload = {
        "mission_id": "acfb03d2",
        "status": "failed",
        "origin_session": "does-not-exist",
    }
    assert resolve_mission_delivery_session(payload, db) is None


def test_continuation_walk():
    db = _FakeSessionDB(
        {"child": {"source": "desktop"}},
        resumes={"parent": "child"},
    )
    assert resolve_live_session_id("parent", db) == "child"
    assert resolve_live_session_id("ghost", db) is None


def test_callback_text_carries_ctrl_and_signature():
    text = format_mission_callback(
        {
            "mission_id": "acfb03d2",
            "status": "failed",
            "title": "coldcard skip kernel",
            "project": "coldcard-rng-cracker",
            "workspace_name": "dgx-spark",
            "short_description": "Codex CLI not found",
            "terminal_reason": "failed",
        }
    )
    assert "[Mission callback: coldcard skip kernel]" in text
    assert "status=failed mission=acfb03d2 workspace=dgx-spark" in text
    assert "Codex CLI not found" in text
    assert "[CTRL: coldcard-rng-cracker | mode=blocked |" in text
    assert "[STATE_SIGNATURE: coldcard-rng-cracker|mission-callback|acfb03d2|failed|inspect]" in text
    assert "[DECISION:]" in text


def test_origin_must_reference_the_mission_when_inspectable():
    from gateway.platforms.mission_status_route import append_mission_callback

    mission = "acfb03d2-d088-46c3-a2a3-6576563f06cb"
    db = _FakeSessionDBWithMessages(
        {"victim": {"source": "desktop"}, "owner": {"source": "desktop"}},
        messages={
            "victim": [{"content": "unrelated chatter"}],
            "owner": [{"content": f"start_mission -> {mission} dispatched"}],
        },
    )
    hijack = {
        "mission_id": mission,
        "status": "failed",
        "origin_session": "victim",
    }
    # The named session exists but never saw this mission — refuse it, and
    # with no project route available the payload is unroutable.
    assert resolve_mission_delivery_session(hijack, db) is None

    legit = dict(hijack, origin_session="owner")
    assert resolve_mission_delivery_session(legit, db) == "owner"


def test_duplicate_event_id_appends_once():
    from gateway.platforms.mission_status_route import append_mission_callback

    db = _FakeSessionDBWithMessages({"s1": {"source": "desktop"}})
    payload = {
        "mission_id": "acfb03d2",
        "status": "failed",
        "title": "retry storm",
        "event_id": "evt-123",
    }
    assert append_mission_callback("s1", payload, db) == "s1"
    assert append_mission_callback("s1", payload, db) == "s1"
    assert len(db.appended) == 1
    # A different delivery still lands.
    assert append_mission_callback("s1", dict(payload, event_id="evt-456"), db) == "s1"
    assert len(db.appended) == 2

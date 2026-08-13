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

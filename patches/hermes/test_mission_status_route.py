"""Unit tests for mission-complete routing (no gateway needed)."""

from gateway.platforms.mission_status_route import (
    MISSION_CALLBACK_WAKE_PROMPT,
    PROJECT_OPERATOR_WAKE_MESSAGE_CAP,
    append_mission_callback,
    extract_origin_session,
    extract_project_slug,
    extract_status,
    format_mission_callback,
    is_routable_mission_status,
    resolve_live_session_id,
    resolve_mission_delivery_session,
    should_wake_mission_callback,
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
        self.messages.setdefault(session_id, []).append(
            {"role": role, "content": content}
        )


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


def test_async_session_db_wrapper_is_unwrapped():
    """Gateway runner._session_db is AsyncSessionDB — must not see coroutines."""
    import asyncio

    inner = _FakeSessionDBWithMessages(
        {"owner": {"source": "desktop"}},
        messages={"owner": [{"content": "start_mission -> acfb03d2"}]},
    )

    class _AsyncWrap:
        def __init__(self, db):
            self._db = db

        def __getattr__(self, name):
            attr = getattr(self._db, name)

            async def _offloaded(*args, **kwargs):
                return attr(*args, **kwargs)

            return _offloaded if callable(attr) else attr

    payload = {
        "mission_id": "acfb03d2",
        "status": "completed",
        "origin_session": "owner",
    }
    wrapped = _AsyncWrap(inner)
    assert resolve_mission_delivery_session(payload, wrapped) == "owner"
    live, appended = append_mission_callback("owner", payload, wrapped)
    assert live == "owner"
    assert appended is True
    assert any("[Mission callback" in t["content"] for t in inner.messages["owner"])
    pending = wrapped.get_session("owner")
    assert asyncio.iscoroutine(pending)
    pending.close()


def test_duplicate_event_id_appends_once():
    db = _FakeSessionDBWithMessages({"s1": {"source": "desktop"}})
    payload = {
        "mission_id": "acfb03d2",
        "status": "failed",
        "title": "retry storm",
        "event_id": "evt-123",
    }
    assert append_mission_callback("s1", payload, db) == ("s1", True)
    assert append_mission_callback("s1", payload, db) == ("s1", False)
    assert len(db.appended) == 1
    # A different delivery still lands.
    assert append_mission_callback("s1", dict(payload, event_id="evt-456"), db) == (
        "s1",
        True,
    )
    assert len(db.appended) == 3  # user separator + assistant for the second event


def test_callback_inserts_user_separator_after_assistant():
    db = _FakeSessionDBWithMessages(
        {"s1": {"source": "desktop"}},
        messages={"s1": [{"role": "assistant", "content": "already replied"}]},
    )
    payload = {"mission_id": "acfb03d2", "status": "completed", "title": "t"}
    live, appended = append_mission_callback("s1", payload, db)
    assert live == "s1" and appended
    roles = [role for _sid, role, _c in db.appended]
    assert roles == ["user", "assistant"]
    assert "has finished" in db.appended[0][2]
    assert "[Mission callback" in db.appended[1][2]


def test_origin_ownership_walks_compression_parent():
    mission = "acfb03d2-d088-46c3-a2a3-6576563f06cb"
    db = _FakeSessionDBWithMessages(
        {
            "parent": {"source": "desktop"},
            "child": {"source": "desktop", "parent_session_id": "parent"},
        },
        resumes={"parent": "child"},
        messages={
            "parent": [{"content": f"start_mission -> {mission} dispatched"}],
            "child": [{"content": "compressed continuation, no mission id"}],
        },
    )
    payload = {
        "mission_id": mission,
        "status": "failed",
        "origin_session": "parent",
    }
    assert resolve_mission_delivery_session(payload, db) == "child"


def test_should_wake_huge_operator_sessions():
    # The wake prompt forbids tools; a 500-message control chat still needs
    # the one-line "mission finished" notice.
    db = _FakeSessionDB({"s1": {"source": "desktop", "message_count": 515}})
    assert should_wake_mission_callback(db, "s1") is True
    db = _FakeSessionDB({"s1": {"source": "desktop", "message_count": 12}})
    assert should_wake_mission_callback(db, "s1") is True
    assert PROJECT_OPERATOR_WAKE_MESSAGE_CAP == 80


def test_should_wake_skips_a_session_being_compacted():
    class _Locked(_FakeSessionDB):
        def get_compression_lock_holder(self, sid):
            return "pid=1:compressor"

    db = _Locked({"s1": {"source": "desktop", "message_count": 4}})
    assert should_wake_mission_callback(db, "s1") is False


def test_wake_prompt_does_not_order_an_inspect_loop():
    lower = MISSION_CALLBACK_WAKE_PROMPT.lower()
    assert "do not inspect" in lower
    assert "do not run tools" in lower
    assert "continue autonomously" not in lower
    assert "if you can continue" not in lower


class _TypedDB:
    """Records every append with its display typing."""

    def __init__(self, last_role=None):
        self.appended = []
        self._last_role = last_role

    def get_session(self, session_id):
        return {"id": session_id}

    def get_messages(self, session_id):
        if self._last_role is None:
            return []
        return [{"role": self._last_role, "content": "x"}]

    def append_message(self, **kwargs):
        self.appended.append(kwargs)


def test_callback_rows_are_typed_for_display():
    from gateway.platforms.mission_status_route import (
        MISSION_CALLBACK_DISPLAY_KIND,
        MISSION_CALLBACK_SEPARATOR_DISPLAY_KIND,
        append_mission_callback,
        mission_callback_display_metadata,
    )

    payload = {
        "mission_id": "693cc6e8-a6cf-4491-86a7-6585625db99e",
        "status": "completed",
        "title": "PR #233 repair",
        "project": "verity-lido",
        "workspace_name": "verity",
        "short_description": "prove SUCCESS, GitHub CLEAN",
        "event_id": "evt-233",
    }
    db = _TypedDB(last_role="assistant")
    live, appended = append_mission_callback("s1", payload, db)
    assert appended and live == "s1"
    separator, callback = db.appended
    assert separator["role"] == "user"
    assert separator["display_kind"] == MISSION_CALLBACK_SEPARATOR_DISPLAY_KIND == "hidden"
    assert callback["role"] == "assistant"
    assert callback["display_kind"] == MISSION_CALLBACK_DISPLAY_KIND == "mission_callback"
    meta = callback["display_metadata"]
    assert meta == mission_callback_display_metadata(payload)
    assert meta["mission_id"] == payload["mission_id"]
    assert meta["status"] == "completed"
    assert meta["title"] == "PR #233 repair"
    assert meta["project"] == "verity-lido"
    assert meta["event_id"] == "evt-233"
    assert meta["workspace"] == "verity"
    assert meta["summary"] == "prove SUCCESS, GitHub CLEAN"
    # The prose still carries the machine trailer for the controller.
    assert "[Mission callback: PR #233 repair]" in callback["content"]


def test_callback_typing_tolerates_an_old_db_shim():
    from gateway.platforms.mission_status_route import append_mission_callback

    class _OldDB:
        def __init__(self):
            self.appended = []

        def get_session(self, session_id):
            return {"id": session_id}

        def get_messages(self, session_id):
            return []

        def append_message(self, session_id, role, content):
            self.appended.append((session_id, role, content))

    db = _OldDB()
    append_mission_callback("s1", {"mission_id": "m1", "status": "failed"}, db)
    assert len(db.appended) == 1
    assert db.appended[0][1] == "assistant"

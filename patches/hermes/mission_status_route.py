"""Route a sandboxed.sh mission-status webhook into a live conversation.

HMAC already authenticated the payload. origin_session is still a hint:
the session must exist (continuations followed). If it does not, the
explicit project route is the only fallback. An unroutable payload returns
None so the webhook adapter can keep its isolated-session behaviour.
"""

from __future__ import annotations

import json
import logging
import re
from pathlib import Path
from typing import Any, Optional, Tuple

logger = logging.getLogger(__name__)

_PENDING_DIRNAME = "pending_mission_callbacks"
_SAFE_MISSION_RE = re.compile(r"[^A-Za-z0-9._:-]+")

_TERMINAL = {
    "completed",
    "failed",
    "not_feasible",
    "notfeasible",
    "blocked",
    "interrupted",
    "awaiting_user",
    "awaitinguser",
}

# Legacy cap kept for tests/import compatibility. The wake prompt is now a
# one-or-two-sentence notice that forbids tools, so a large operator session
# must still be woken — otherwise callbacks append silently and the chat
# looks dead (Lido/EIP-8282, 2026-08-24). Only a compression lock skips.
PROJECT_OPERATOR_WAKE_MESSAGE_CAP = 80

MISSION_CALLBACK_WAKE_PROMPT = (
    "A routed mission-complete callback was just appended to this conversation. "
    "In one or two sentences, tell the operator what finished and whether they "
    "need to act. Do not inspect the mission, do not run tools, and do not "
    "continue the work in this chat — the project controller owns follow-up."
)


def extract_origin_session(payload: dict) -> str:
    return str(
        payload.get("origin_session") or payload.get("origin_session_id") or ""
    ).strip()


def extract_project_slug(payload: dict) -> str:
    project = payload.get("project")
    if isinstance(project, dict):
        project = project.get("project") or project.get("slug")
    return str(project or "").strip()


def extract_status(payload: dict) -> str:
    return str(
        payload.get("status") or payload.get("type") or payload.get("event_type") or ""
    ).strip().lower()


def is_routable_mission_status(payload: dict) -> bool:
    if not str(payload.get("mission_id") or "").strip():
        return False
    return extract_status(payload) in _TERMINAL


def sync_session_db(session_db: Any) -> Any:
    """Prefer the underlying sync SessionDB.

    The gateway runner exposes ``AsyncSessionDB``, whose ``__getattr__``
    wraps every method in ``asyncio.to_thread``. Calling those from this
    sync router yields a coroutine (truthy!) instead of a session row —
    that is how origin-route crashed in production (TAP smoke, 2026-08-15)
    and fell through to a throwaway webhook session.
    """
    inner = getattr(session_db, "_db", None)
    return inner if inner is not None else session_db


def resolve_live_session_id(session_id: str, session_db: Any) -> Optional[str]:
    """Follow continuation / resume pointers; None if the row is gone."""
    sid = (session_id or "").strip()
    if not sid:
        return None
    resolve_resume = getattr(session_db, "resolve_resume_session_id", None)
    resolve_id = getattr(session_db, "resolve_session_id", None)
    if callable(resolve_resume):
        sid = resolve_resume(sid) or sid
    elif callable(resolve_id):
        sid = resolve_id(sid) or sid
    row = session_db.get_session(sid)
    if not row:
        return None
    return sid


def _recent_message_texts(session_db: Any, session_id: str, limit: int = 400):
    """Best-effort read of a session's recent message bodies.

    Feature-detected: SessionDB variants expose ``get_messages`` /
    ``list_messages``. Returns None (not []) when no reader exists, so
    callers can tell "cannot inspect" apart from "inspected, found nothing".
    """
    reader = getattr(session_db, "get_messages", None) or getattr(
        session_db, "list_messages", None
    )
    if not callable(reader):
        return None
    try:
        rows = reader(session_id) or []
    except Exception:
        logger.debug("message read failed for %s", session_id, exc_info=True)
        return None
    texts = []
    for row in rows[-limit:]:
        content = row.get("content") if isinstance(row, dict) else getattr(row, "content", "")
        if content:
            texts.append(str(content))
    return texts


def _parent_session_id(session_db: Any, session_id: str) -> str:
    getter = getattr(session_db, "get_session", None)
    if not callable(getter):
        return ""
    try:
        row = getter(session_id)
    except Exception:
        return ""
    if not row:
        return ""
    parent = (
        row.get("parent_session_id")
        if isinstance(row, dict)
        else getattr(row, "parent_session_id", None)
    )
    return str(parent or "").strip()


def session_references_mission(session_db: Any, session_id: str, mission_id: str) -> bool:
    """Ownership proof for origin routing: the session must already mention
    the mission (the dispatch tool result and prior callbacks embed its id).

    HMAC authenticates the *payload*, not the origin hint inside it — any
    mission creator could name an unrelated conversation. Fail-open only when
    the store exposes no message reader (legacy DBs), fail-closed otherwise.

    Walks ``parent_session_id`` so a compression continuation still owns a
    mission started in the pre-compression parent.
    """
    mission = (mission_id or "").strip()
    if not mission:
        return False
    current = (session_id or "").strip()
    seen: set[str] = set()
    inspected = False
    while current and current not in seen:
        seen.add(current)
        texts = _recent_message_texts(session_db, current)
        if texts is None:
            if not inspected:
                return True  # cannot inspect — legacy store, keep prior behaviour
            break
        inspected = True
        if any(mission in text for text in texts):
            return True
        current = _parent_session_id(session_db, current)
    return False


def extract_event_id(payload: dict) -> str:
    return str(payload.get("event_id") or payload.get("delivery_id") or "").strip()


def resolve_project_session_id(project: str, session_db: Any = None) -> Optional[str]:
    slug = (project or "").strip()
    if not slug:
        return None
    try:
        from hermes_cli import project_routes as routes
        from hermes_cli import projects_db as pdb
    except Exception:
        logger.debug("project route store unavailable", exc_info=True)
        return None
    try:
        with pdb.connect_closing() as conn:
            target = routes.resolve_route_target(
                conn, slug, session_db=session_db
            )
        return str(getattr(target, "session_id", "") or "") or None
    except Exception as exc:
        logger.info("project route %s did not resolve: %s", slug, exc)
        return None


def extract_wake_session(payload: dict) -> str:
    return str(payload.get("wake_session") or "").strip()


def resolve_mission_delivery_session(payload: dict, session_db: Any) -> Optional[str]:
    """Pick the dedicated conversation for this mission-status event."""
    session_db = sync_session_db(session_db)
    if not is_routable_mission_status(payload):
        return None
    # Producer-resolved tip (project binding, else origin). Use the id as-is
    # when the row exists: walking resume children is how a project click
    # used to land on a live review thread instead of the bound session.
    hinted = extract_wake_session(payload)
    if hinted:
        row = session_db.get_session(hinted)
        if row:
            return hinted
        live = resolve_live_session_id(hinted, session_db)
        if live:
            return live
    origin = extract_origin_session(payload)
    if origin:
        live = resolve_live_session_id(origin, session_db)
        if live:
            mission_id = str(payload.get("mission_id") or "").strip()
            if session_references_mission(session_db, live, mission_id):
                return live
            logger.info(
                "origin %s does not reference mission %s — falling back to the project route",
                live,
                mission_id,
            )
    project = extract_project_slug(payload)
    if project:
        return resolve_project_session_id(project, session_db)
    return None


def format_mission_callback(payload: dict) -> str:
    """Human + machine trailer written into the dedicated session."""
    mission_id = str(payload.get("mission_id") or "").strip()
    status = extract_status(payload)
    title = str(payload.get("title") or "mission").strip()
    project = extract_project_slug(payload) or "unknown"
    workspace = str(payload.get("workspace_name") or "").strip()
    bits = [
        payload.get("result_summary"),
        payload.get("short_description"),
        payload.get("terminal_reason"),
        payload.get("terminal_evidence") if status != "completed" else None,
    ]
    body = "\n".join(str(b).strip() for b in bits if b and str(b).strip())
    mode = "active" if status == "completed" else "blocked"
    event_id = extract_event_id(payload)
    lines = [
        f"[Mission callback: {title}]",
        f"status={status} mission={mission_id}"
        + (f" event={event_id}" if event_id else "")
        + (f" workspace={workspace}" if workspace else ""),
    ]
    if body:
        lines.append(body)
    if status != "completed":
        lines.append(
            "If this is infra (missing CLI, auth, workspace), fix or "
            "[DECISION:] — do not stay silent in another session."
        )
    lines.append(
        f"[CTRL: {project} | mode={mode} | wait=0 | next=inspect {mission_id}]"
    )
    lines.append(
        f"[STATE_SIGNATURE: {project}|mission-callback|{mission_id}|{status}|inspect]"
    )
    return "\n".join(lines)


def _last_message_role(session_db: Any, session_id: str) -> Optional[str]:
    reader = getattr(session_db, "get_messages", None) or getattr(
        session_db, "list_messages", None
    )
    if not callable(reader):
        return None
    try:
        rows = reader(session_id) or []
    except Exception:
        return None
    if not rows:
        return None
    last = rows[-1]
    role = last.get("role") if isinstance(last, dict) else getattr(last, "role", None)
    return str(role).strip().lower() if role else None


def should_wake_mission_callback(session_db: Any, live_id: str) -> bool:
    """Whether to start an agent turn after appending a mission callback.

    Append always happens. The wake prompt is a one-or-two-sentence notice
    that forbids tools. Skip only while another writer holds the compression
    lock — a large operator session must still be told that a mission finished.
    """
    sid = (live_id or "").strip()
    if not sid:
        return False
    db = sync_session_db(session_db)
    holder_fn = getattr(db, "get_compression_lock_holder", None)
    if callable(holder_fn):
        try:
            if holder_fn(sid):
                logger.info(
                    "skip mission wake for %s: compression lock held", sid
                )
                return False
        except Exception:
            logger.debug("compression-lock check failed for %s", sid, exc_info=True)
    return True


def append_mission_callback(
    session_id: str, payload: dict, session_db: Any
) -> Tuple[str, bool]:
    """Persist the callback on the live session.

    Returns ``(live_id, appended)``. ``appended`` is False when this exact
    delivery is already in the transcript — callers must not schedule a
    second wake.

    Idempotent per delivery: the producer retries at-least-once on a lost
    HTTP response, so an ``event=<id>`` already present in the transcript
    means this exact delivery landed.

    Role-safe: never writes assistant→assistant. If the tip is already an
    assistant turn, a user separator is inserted first so
    ``repair_message_sequence`` cannot merge the callback into the previous
    model answer.
    """
    session_db = sync_session_db(session_db)
    live = resolve_live_session_id(session_id, session_db) or session_id
    event_id = extract_event_id(payload)
    if event_id:
        texts = _recent_message_texts(session_db, live)
        if texts is not None:
            marker = f" event={event_id}"
            if any("[Mission callback" in t and marker in t for t in texts):
                logger.info(
                    "duplicate mission callback event %s for %s — skipping append",
                    event_id,
                    live,
                )
                return live, False
    content = format_mission_callback(payload)
    if _last_message_role(session_db, live) == "assistant":
        session_db.append_message(
            session_id=live,
            role="user",
            content="A mission you started has finished. The result follows.",
        )
    session_db.append_message(session_id=live, role="assistant", content=content)
    return live, True


def _pending_callback_path(mission_id: str) -> Path:
    from hermes_constants import get_hermes_home

    safe = _SAFE_MISSION_RE.sub("_", (mission_id or "").strip())[:128] or "unknown"
    return get_hermes_home() / _PENDING_DIRNAME / f"{safe}.json"


def stash_unroutable_callback(mission_id: str, payload: dict) -> None:
    """Keep a terminal webhook that arrived before enroll/ownership proof."""
    mid = (mission_id or "").strip()
    if not mid or not isinstance(payload, dict):
        return
    path = _pending_callback_path(mid)
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        tmp = path.with_suffix(".tmp")
        tmp.write_text(json.dumps(payload), encoding="utf-8")
        tmp.replace(path)
    except Exception:
        logger.debug("failed to stash pending mission callback %s", mid, exc_info=True)


def take_stashed_callback(mission_id: str) -> Optional[dict]:
    """Pop a previously stashed terminal callback, or None."""
    mid = (mission_id or "").strip()
    if not mid:
        return None
    path = _pending_callback_path(mid)
    if not path.exists():
        return None
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except Exception:
        data = None
    try:
        path.unlink()
    except Exception:
        pass
    return data if isinstance(data, dict) else None

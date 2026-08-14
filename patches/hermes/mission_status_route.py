"""Route a sandboxed.sh mission-status webhook into a live conversation.

HMAC already authenticated the payload. origin_session is still a hint:
the session must exist (continuations followed). If it does not, the
explicit project route is the only fallback. An unroutable payload returns
None so the webhook adapter can keep its isolated-session behaviour.
"""

from __future__ import annotations

import logging
from typing import Any, Optional

logger = logging.getLogger(__name__)

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


def session_references_mission(session_db: Any, session_id: str, mission_id: str) -> bool:
    """Ownership proof for origin routing: the session must already mention
    the mission (the dispatch tool result and prior callbacks embed its id).

    HMAC authenticates the *payload*, not the origin hint inside it — any
    mission creator could name an unrelated conversation. Fail-open only when
    the store exposes no message reader (legacy DBs), fail-closed otherwise.
    """
    mission = (mission_id or "").strip()
    if not mission:
        return False
    texts = _recent_message_texts(session_db, session_id)
    if texts is None:
        return True  # cannot inspect — legacy store, keep prior behaviour
    return any(mission in text for text in texts)


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


def append_mission_callback(session_id: str, payload: dict, session_db: Any) -> str:
    """Persist the callback on the live session. Returns the live id.

    Idempotent per delivery: the producer retries at-least-once on a lost
    HTTP response, so an ``event=<id>`` already present in the transcript
    means this exact delivery landed — appending again would double the
    callback and schedule a second autonomous wake.
    """
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
                return live
    content = format_mission_callback(payload)
    session_db.append_message(session_id=live, role="assistant", content=content)
    return live

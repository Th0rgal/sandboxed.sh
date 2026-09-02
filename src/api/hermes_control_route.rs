//! Write-through replica of the operator bind into Hermes `projects.db`.
//!
//! The roster `project_bindings` row is the fact the operator declared.
//! Hermes cron `deliver=project:<slug>` still reads `project_session_routes`.
//! Bind/unbind keep that replica in sync so the two stores cannot drift.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};

fn projects_db_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var("HERMES_PROJECTS_DB")
        .ok()
        .map(PathBuf::from)
        .filter(|path| path.is_file())
    {
        return Some(path);
    }
    if let Some(home) = std::env::var("HERMES_HOME").ok().map(PathBuf::from) {
        let path = home.join("projects.db");
        if path.is_file() {
            return Some(path);
        }
    }
    if let Some(state) = std::env::var("HERMES_STATE_DB").ok().map(PathBuf::from) {
        if let Some(parent) = state.parent() {
            let path = parent.join("projects.db");
            if path.is_file() {
                return Some(path);
            }
        }
    }
    None
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn new_project_id() -> String {
    format!("p_{:08x}", now_secs() as u32 ^ (std::process::id()))
}

fn open(path: &Path) -> Result<Connection, String> {
    Connection::open(path).map_err(|e| e.to_string())
}

fn project_id_for_slug(conn: &Connection, slug: &str) -> Result<Option<String>, String> {
    conn.query_row(
        "SELECT id FROM projects WHERE slug = ?1 AND archived = 0",
        params![slug],
        |row| row.get(0),
    )
    .optional()
    .map_err(|e| e.to_string())
}

/// Ensure a Hermes project exists under `canonical` and bind it to `session_id`.
pub fn bind_canonical(canonical: &str, session_id: &str) -> Result<(), String> {
    let Some(path) = projects_db_path() else {
        return Ok(());
    };
    let conn = open(&path)?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS project_session_routes (
            project_id  TEXT PRIMARY KEY,
            session_id  TEXT NOT NULL,
            bound_at    INTEGER NOT NULL,
            updated_at  INTEGER NOT NULL
         );",
    )
    .map_err(|e| e.to_string())?;
    let id = match project_id_for_slug(&conn, canonical)? {
        Some(id) => id,
        None => {
            let id = new_project_id();
            conn.execute(
                "INSERT INTO projects (id, slug, name, created_at, archived) \
                 VALUES (?1, ?2, ?3, ?4, 0)",
                params![id, canonical, canonical, now_secs()],
            )
            .map_err(|e| e.to_string())?;
            id
        }
    };
    let now = now_secs();
    conn.execute(
        "INSERT INTO project_session_routes (project_id, session_id, bound_at, updated_at) \
         VALUES (?1, ?2, ?3, ?3) \
         ON CONFLICT(project_id) DO UPDATE SET \
           session_id = excluded.session_id, \
           updated_at = excluded.updated_at",
        params![id, session_id, now],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn unbind_canonical(canonical: &str) -> Result<(), String> {
    let Some(path) = projects_db_path() else {
        return Ok(());
    };
    let conn = open(&path)?;
    let Some(id) = project_id_for_slug(&conn, canonical)? else {
        return Ok(());
    };
    conn.execute(
        "DELETE FROM project_session_routes WHERE project_id = ?1",
        params![id],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Rename a Hermes project slug in place (id preserved). No-op if missing.
pub fn rename_slug(old: &str, new: &str) -> Result<(), String> {
    if old == new {
        return Ok(());
    }
    let Some(path) = projects_db_path() else {
        return Ok(());
    };
    let conn = open(&path)?;
    conn.execute(
        "UPDATE projects SET slug = ?2 WHERE slug = ?1 AND archived = 0",
        params![old, new],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

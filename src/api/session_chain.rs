//! Walk Hermes conversation continuation chains.
//!
//! Hermes compresses a long conversation and forks a continuation, linked to
//! its parent by `sessions.parent_session_id`. A project binding names one
//! session id, so the binding goes stale the moment the conversation rolls
//! over — measured on the Lido audit, four times in a single night.
//!
//! Hermes solves this on its own side by treating the stored route as a
//! *declared* fact and resolving forward at read time. This mirrors that,
//! reading the same `state.db` that the projects board already opens read-only.
//! Nothing here writes.
//!
//! Both directions are needed, for different questions:
//!
//! * **Forward** — "the operator bound `verity` to session X; which
//!   conversation should the board open today?" Walk down to the live tip.
//! * **Backward** — "a mission was spawned from session Y; which project does
//!   it belong to?" Walk up until an ancestor matches a binding.

use std::collections::HashSet;
use std::path::Path;

/// Depth cap for either walk.
///
/// A cycle in `parent_session_id` should be impossible, but a malformed row
/// must not hang a request that runs on a page render. The `seen` set already
/// makes a cycle terminate; this bounds a pathologically long legitimate chain
/// too. Real chains are single digits.
const MAX_CHAIN_DEPTH: usize = 64;

fn open(db_path: &Path) -> Option<rusqlite::Connection> {
    rusqlite::Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok()
}

/// The live continuation tip of `session_id`, following children forward.
///
/// Returns `session_id` itself when it has no continuation, when the database
/// is unreadable, or when the row does not exist. Degrading to the declared id
/// is the right failure: it is what the operator asked for, and a stale answer
/// beats no answer on a display path.
pub fn live_tip(db_path: &Path, session_id: &str) -> String {
    let Some(connection) = open(db_path) else {
        return session_id.to_string();
    };
    let mut current = session_id.to_string();
    let mut seen: HashSet<String> = HashSet::from([current.clone()]);

    for _ in 0..MAX_CHAIN_DEPTH {
        let Some(next) = newest_livable_child(&connection, &current) else {
            break;
        };
        if !seen.insert(next.clone()) {
            break;
        }
        current = next;
    }
    current
}

/// Newest child that is actually a conversation, not a failed compression fork.
fn newest_livable_child(connection: &rusqlite::Connection, parent: &str) -> Option<String> {
    match newest_livable_child_rich(connection, parent) {
        Ok(found) => found,
        Err(_) => connection
            .query_row(
                "SELECT id FROM sessions WHERE parent_session_id = ?1 \
                 ORDER BY id DESC LIMIT 1",
                [parent],
                |row| row.get(0),
            )
            .ok(),
    }
}

fn newest_livable_child_rich(
    connection: &rusqlite::Connection,
    parent: &str,
) -> Result<Option<String>, rusqlite::Error> {
    let mut statement = connection.prepare(
        "SELECT id, ended_at, end_reason, started_at, message_count \
         FROM sessions WHERE parent_session_id = ?1 ORDER BY id DESC",
    )?;
    let rows = statement.query_map([parent], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<f64>>(1).ok().flatten(),
            row.get::<_, Option<String>>(2).ok().flatten(),
            row.get::<_, Option<f64>>(3).ok().flatten(),
            row.get::<_, Option<i64>>(4).ok().flatten(),
        ))
    })?;
    for row in rows.flatten() {
        let (id, ended_at, end_reason, started_at, message_count) = row;
        if child_is_livable(end_reason.as_deref(), started_at, ended_at, message_count) {
            return Ok(Some(id));
        }
    }
    Ok(None)
}

fn child_is_livable(
    end_reason: Option<&str>,
    started_at: Option<f64>,
    ended_at: Option<f64>,
    message_count: Option<i64>,
) -> bool {
    let reason = end_reason.unwrap_or("").trim().to_ascii_lowercase();
    if matches!(
        reason.as_str(),
        "compression_exhausted" | "compression_ineffective"
    ) {
        return false;
    }
    if let (Some(start), Some(end)) = (started_at, ended_at) {
        let lifetime = end - start;
        if (0.0..=5.0).contains(&lifetime) && message_count.unwrap_or(0) == 0 {
            return false;
        }
    }
    true
}

/// `session_id` and its ancestors, nearest first.
///
/// Used to attribute a mission: the conversation it was spawned from may be
/// several continuations below the one an operator actually bound.
pub fn ancestry(db_path: &Path, session_id: &str) -> Vec<String> {
    let mut chain = vec![session_id.to_string()];
    let Some(connection) = open(db_path) else {
        return chain;
    };
    let mut current = session_id.to_string();
    let mut seen: HashSet<String> = HashSet::from([current.clone()]);

    for _ in 0..MAX_CHAIN_DEPTH {
        let parent: Option<String> = connection
            .query_row(
                "SELECT parent_session_id FROM sessions WHERE id = ?1",
                [&current],
                |row| row.get(0),
            )
            .ok()
            .flatten();
        match parent {
            Some(next) if !next.trim().is_empty() && seen.insert(next.clone()) => {
                chain.push(next.clone());
                current = next;
            }
            _ => break,
        }
    }
    chain
}

/// Which project owns `session_id`, walking the continuation chain both ways.
///
/// A binding names the session the operator declared. Hermes then forks
/// children (`parent_session_id`). Looking up the tip must still find the
/// project; looking up an ancestor must too.
pub fn slug_for_bound_session(
    session_id: &str,
    bindings: &[(String, String)],
    ancestry: &[String],
    tip_of: impl Fn(&str) -> String,
) -> Option<String> {
    if session_id.trim().is_empty() {
        return None;
    }
    for (slug, bound) in bindings {
        if ancestry.iter().any(|id| id == bound) || tip_of(bound) == session_id {
            return Some(slug.clone());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a state.db with just the columns these walks touch.
    fn db(rows: &[(&str, Option<&str>)]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("state.db");
        let connection = rusqlite::Connection::open(&path).expect("open");
        connection
            .execute(
                "CREATE TABLE sessions (id TEXT PRIMARY KEY, parent_session_id TEXT)",
                [],
            )
            .expect("schema");
        for (id, parent) in rows {
            connection
                .execute(
                    "INSERT INTO sessions (id, parent_session_id) VALUES (?1, ?2)",
                    rusqlite::params![id, parent],
                )
                .expect("insert");
        }
        (dir, path)
    }

    struct RichRow<'a> {
        id: &'a str,
        parent: Option<&'a str>,
        started_at: f64,
        ended_at: Option<f64>,
        end_reason: Option<&'a str>,
        message_count: i64,
    }

    fn rich_db(rows: &[RichRow<'_>]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("state.db");
        let connection = rusqlite::Connection::open(&path).expect("open");
        connection
            .execute(
                "CREATE TABLE sessions (\
                    id TEXT PRIMARY KEY, \
                    parent_session_id TEXT, \
                    started_at REAL, \
                    ended_at REAL, \
                    end_reason TEXT, \
                    message_count INTEGER\
                )",
                [],
            )
            .expect("schema");
        for row in rows {
            connection
                .execute(
                    "INSERT INTO sessions \
                     (id, parent_session_id, started_at, ended_at, end_reason, message_count) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    rusqlite::params![
                        row.id,
                        row.parent,
                        row.started_at,
                        row.ended_at,
                        row.end_reason,
                        row.message_count
                    ],
                )
                .expect("insert");
        }
        (dir, path)
    }

    /// The shape measured on the Lido audit: bound once, rolled over four times.
    #[test]
    fn the_tip_is_the_end_of_the_chain() {
        let (_dir, path) = db(&[
            ("30e544", None),
            ("fad004", Some("30e544")),
            ("527c1a", Some("fad004")),
            ("e15a30", Some("527c1a")),
        ]);
        assert_eq!(live_tip(&path, "30e544"), "e15a30");
        assert_eq!(live_tip(&path, "527c1a"), "e15a30");
        assert_eq!(live_tip(&path, "e15a30"), "e15a30");
    }

    #[test]
    fn ancestry_reaches_the_bound_session_from_a_descendant() {
        let (_dir, path) = db(&[
            ("30e544", None),
            ("fad004", Some("30e544")),
            ("527c1a", Some("fad004")),
            ("e15a30", Some("527c1a")),
        ]);
        assert_eq!(
            ancestry(&path, "e15a30"),
            vec!["e15a30", "527c1a", "fad004", "30e544"]
        );
    }

    #[test]
    fn an_unknown_session_resolves_to_itself() {
        // Not an error: the caller asked about something this database does
        // not know, and inventing an answer would be worse than echoing.
        let (_dir, path) = db(&[("a", None)]);
        assert_eq!(live_tip(&path, "ghost"), "ghost");
        assert_eq!(ancestry(&path, "ghost"), vec!["ghost"]);
    }

    #[test]
    fn an_unreadable_database_degrades_to_the_declared_id() {
        let missing = std::path::Path::new("/nonexistent/state.db");
        assert_eq!(live_tip(missing, "sess"), "sess");
        assert_eq!(ancestry(missing, "sess"), vec!["sess"]);
    }

    /// A malformed parent link must not hang a page render.
    #[test]
    fn a_cycle_terminates() {
        let (_dir, path) = db(&[("a", Some("b")), ("b", Some("a"))]);
        assert_eq!(live_tip(&path, "a"), "b");
        let chain = ancestry(&path, "a");
        assert_eq!(chain, vec!["a", "b"]);
    }

    #[test]
    fn a_blank_parent_is_no_parent() {
        let (_dir, path) = db(&[("root", Some("")), ("child", Some("root"))]);
        assert_eq!(ancestry(&path, "child"), vec!["child", "root"]);
    }

    #[test]
    fn a_continuation_tip_resolves_to_the_bound_project() {
        let (_dir, path) = db(&[
            ("74bd9b", None),
            ("202eac", Some("74bd9b")),
            ("7fdc25", Some("202eac")),
            ("2a62eb", Some("7fdc25")),
        ]);
        let bindings = vec![("verity-core".to_string(), "74bd9b".to_string())];
        let chain = ancestry(&path, "2a62eb");
        assert_eq!(
            slug_for_bound_session("2a62eb", &bindings, &chain, |id| live_tip(&path, id)),
            Some("verity-core".to_string())
        );
        assert_eq!(
            slug_for_bound_session("74bd9b", &bindings, &ancestry(&path, "74bd9b"), |id| {
                live_tip(&path, id)
            }),
            Some("verity-core".to_string())
        );
        assert_eq!(
            slug_for_bound_session("ghost", &bindings, &ancestry(&path, "ghost"), |id| {
                live_tip(&path, id)
            }),
            None
        );
    }

    #[test]
    fn a_compression_exhausted_child_is_not_the_tip() {
        let (_dir, path) = rich_db(&[
            RichRow {
                id: "parent",
                parent: None,
                started_at: 1_000.0,
                ended_at: None,
                end_reason: None,
                message_count: 40,
            },
            RichRow {
                id: "dead-child",
                parent: Some("parent"),
                started_at: 2_000.0,
                ended_at: Some(2_003.0),
                end_reason: Some("compression_exhausted"),
                message_count: 0,
            },
        ]);
        assert_eq!(live_tip(&path, "parent"), "parent");
    }

    #[test]
    fn a_successful_continuation_is_the_tip() {
        let (_dir, path) = rich_db(&[
            RichRow {
                id: "parent",
                parent: None,
                started_at: 1_000.0,
                ended_at: Some(2_000.0),
                end_reason: Some("compression"),
                message_count: 40,
            },
            RichRow {
                id: "child",
                parent: Some("parent"),
                started_at: 2_000.0,
                ended_at: None,
                end_reason: None,
                message_count: 12,
            },
        ]);
        assert_eq!(live_tip(&path, "parent"), "child");
    }

    #[test]
    fn an_alias_binding_is_the_first_match_the_handler_must_canonicalize() {
        // Prod 2026-08-15: /by-session returned `verity-roadmap` because that
        // binding is listed before `verity-core`. The HTTP handler folds it.
        let (_dir, path) = db(&[("74bd9b", None), ("2a62eb", Some("74bd9b"))]);
        let bindings = vec![
            ("verity-roadmap".to_string(), "74bd9b".to_string()),
            ("verity-core".to_string(), "74bd9b".to_string()),
        ];
        let chain = ancestry(&path, "2a62eb");
        let raw = slug_for_bound_session("2a62eb", &bindings, &chain, |id| live_tip(&path, id));
        assert_eq!(raw.as_deref(), Some("verity-roadmap"));
        let mut aliases = std::collections::HashMap::new();
        aliases.insert("verity-roadmap".to_string(), "verity-core".to_string());
        assert_eq!(
            crate::api::projects_overview::canonicalize_project_slug_with(
                &aliases,
                raw.as_deref().unwrap()
            ),
            "verity-core"
        );
    }
}

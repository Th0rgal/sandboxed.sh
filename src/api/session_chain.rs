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
        let child: Option<String> = connection
            .query_row(
                "SELECT id FROM sessions WHERE parent_session_id = ?1 \
                 ORDER BY id DESC LIMIT 1",
                [&current],
                |row| row.get(0),
            )
            .ok();
        match child {
            Some(next) if seen.insert(next.clone()) => current = next,
            _ => break,
        }
    }
    current
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
}

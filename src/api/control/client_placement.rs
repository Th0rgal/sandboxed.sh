//! Missions Orb runs on the desktop that created them.
//!
//! The scheduler only starts a local harness for a pending mission that still
//! has a deferred goal. A client placement never receives one, and the stores
//! also drop a row tagged [`TAG`] so a stray goal cannot dispatch it.

pub const TAG: &str = "placement:client";

pub fn is_client_placement(value: Option<&str>) -> bool {
    matches!(value.map(str::trim), Some("client"))
}

pub fn is_tagged(tags: &[String]) -> bool {
    tags.iter().any(|tag| tag == TAG)
}

/// Pending + deferred goal is the scheduler's ticket. A client placement is
/// never that ticket, even if a goal was written by mistake.
pub fn scheduler_accepts(pending: bool, has_deferred_goal: bool, tags: &[String]) -> bool {
    pending && has_deferred_goal && !is_tagged(tags)
}

/// Appended to the SQLite pending-mission query. The column stores a JSON array.
pub const SQL_EXCLUDE: &str = " AND IFNULL(tags,'') NOT LIKE '%placement:client%'";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_placement_is_not_a_scheduler_ticket() {
        assert!(scheduler_accepts(true, true, &[]));
        assert!(!scheduler_accepts(true, true, &[TAG.into()]));
        assert!(!scheduler_accepts(true, false, &[]));
        assert!(!scheduler_accepts(false, true, &[]));
        assert!(is_client_placement(Some(" client ")));
        assert!(!is_client_placement(Some("core")));
        assert!(!is_client_placement(None));
    }
}

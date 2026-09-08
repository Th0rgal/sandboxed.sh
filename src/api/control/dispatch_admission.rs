//! Admission is serialized with assignment edits and the lease sweep. The HTTP
//! request carries its assertion to this boundary; HTTP never mutates identity.
use super::*;

pub(crate) static DISPATCH_ADMISSION: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub(crate) struct DispatchFileLock(std::fs::File);
impl Drop for DispatchFileLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.0);
    }
}

/// Lock order: process admission mutex, durable admission file, PR writer
/// lock, stores. This also serializes cooperating backend processes.
pub(crate) async fn durable_lock(config: &Config) -> Result<DispatchFileLock, String> {
    let dir = config.working_dir.join(".sandboxed-sh/missions");
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| e.to_string())?;
    tokio::task::spawn_blocking(move || {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(dir.join(".dispatch-admission.lock"))
            .map_err(|e| e.to_string())?;
        fs2::FileExt::lock_exclusive(&file).map_err(|e| e.to_string())?;
        Ok(DispatchFileLock(file))
    })
    .await
    .map_err(|e| e.to_string())?
}

pub struct DispatchAdmission {
    pub state: Arc<AppState>,
    pub store: Arc<dyn MissionStore>,
    pub internal_work_hint: Option<String>,
    pub patch: crate::api::writer_recycle::WriterIdentityPatch,
}

impl std::fmt::Debug for DispatchAdmission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DispatchAdmission")
            .field("patch", &self.patch)
            .finish_non_exhaustive()
    }
}

fn identity_patch(mission: &Mission) -> crate::api::mission_store::MissionProjectPatch {
    crate::api::mission_store::MissionProjectPatch {
        title: Some(mission.title.clone()),
        project: Some(mission.project.project.clone()),
        track: Some(mission.project.track.clone()),
        github_pr: Some(mission.project.github_pr.clone()),
        tag_patch: Some(crate::api::mission_store::MissionTagPatch::capabilities(
            &mission.project.tags,
        )),
        ..Default::default()
    }
}

struct AdmissionReceipt {
    _file_guard: DispatchFileLock,
    state: Arc<AppState>,
    store: Arc<dyn MissionStore>,
    before: Mission,
    after: Mission,
    acquired: Vec<String>,
    changed: bool,
    actor_before: Option<serde_json::Value>,
    actor_may_have_started: bool,
}

impl AdmissionReceipt {
    fn journal(&self, phase: &str) -> serde_json::Value {
        serde_json::json!({
            "phase": phase,
            "assignment_lifetime": "quiescent-retag-v1",
            "before": { "title": self.before.title, "project": self.before.project, "status": self.before.status, "status_metadata": crate::api::mission_store::MissionStatusSnapshot::capture(&self.before) },
            "after": { "title": self.after.title, "project": self.after.project },
            "acquired": self.acquired,
            "actor_before": self.actor_before,
            "actor_may_have_started": self.actor_may_have_started,
            "identity_changed": self.changed,
        })
    }

    async fn finish(self, accepted: bool) -> Result<(), String> {
        let id = self.before.id.to_string();
        // A durable phase makes a failed cleanup retryable without guessing
        // whether a live actor accepted the new assignment. If this write
        // fails, the pending journal keeps BOTH identities fenced.
        self.state.projects.save_dispatch_admission(
            &id,
            &self.journal(if accepted { "accepted" } else { "rejected" }),
        )?;
        #[cfg(test)]
        if !accepted {
            super::dispatch_admission_tests::crash_checkpoint("dispatch_rejected");
        }
        if accepted {
            self.state
                .projects
                .retain_attempt_lease(&id, self.acquired.last().map(String::as_str))?;
        } else {
            if self.changed {
                self.store
                    .update_mission_project(self.before.id, identity_patch(&self.before))
                    .await?;
            }
            if self.actor_may_have_started {
                restore_status(
                    &self.store,
                    &self.state,
                    self.before.id,
                    &crate::api::mission_store::MissionStatusSnapshot::capture(&self.before),
                )
                .await?;
            }
            for id in self.acquired {
                self.state.projects.expire_lease(&id)?;
            }
        }
        self.state.projects.clear_dispatch_admission(&id)?;
        Ok(())
    }
}

async fn restore_status(
    store: &Arc<dyn MissionStore>,
    state: &Arc<AppState>,
    id: Uuid,
    snapshot: &crate::api::mission_store::MissionStatusSnapshot,
) -> Result<(), String> {
    store.restore_mission_status(id, snapshot).await?;
    for session in state.control.all_sessions().await {
        if !Arc::ptr_eq(&session.mission_store, store) {
            continue;
        }
        let _ = session.events_tx.send(AgentEvent::MissionStatusChanged {
            mission_id: id,
            status: snapshot.status,
            summary: snapshot.terminal_reason.clone(),
        });
    }
    Ok(())
}

/// Retry a known outcome after a cross-store failure. An outcome lost to a
/// crash is deliberately quarantined: never guess that queued work was not
/// accepted, or release the old identity while a runner might still use it.
pub(crate) async fn recover_dispatch(
    state: &Arc<AppState>,
    store: &Arc<dyn MissionStore>,
    id: Uuid,
) -> Result<(), String> {
    let key = id.to_string();
    let Some(journal) = state.projects.dispatch_admission(&key)? else {
        return Ok(());
    };
    if journal["phase"] == "project-edit" {
        let before: crate::api::mission_store::MissionProject =
            serde_json::from_value(journal["before"]["project"].clone())
                .map_err(|e| e.to_string())?;
        let after: crate::api::mission_store::MissionProject =
            serde_json::from_value(journal["after"]["project"].clone())
                .map_err(|e| e.to_string())?;
        let current = store
            .get_mission(id)
            .await?
            .ok_or_else(|| format!("project edit mission {id} missing"))?;
        let matches = |expected: &crate::api::mission_store::MissionProject| {
            let fields = [
                (
                    &before.project,
                    &after.project,
                    &current.project.project,
                    &expected.project,
                ),
                (
                    &before.track,
                    &after.track,
                    &current.project.track,
                    &expected.track,
                ),
                (
                    &before.intent,
                    &after.intent,
                    &current.project.intent,
                    &expected.intent,
                ),
                (
                    &before.github_pr,
                    &after.github_pr,
                    &current.project.github_pr,
                    &expected.github_pr,
                ),
                (
                    &before.desired_state,
                    &after.desired_state,
                    &current.project.desired_state,
                    &expected.desired_state,
                ),
                (
                    &before.next_check_at,
                    &after.next_check_at,
                    &current.project.next_check_at,
                    &expected.next_check_at,
                ),
            ];
            fields
                .iter()
                .all(|(before, after, current, expected)| before == after || current == expected)
                && before.tags.iter().chain(&after.tags).all(|tag| {
                    before.tags.contains(tag) == after.tags.contains(tag)
                        || current.project.tags.contains(tag) == expected.tags.contains(tag)
                })
        };
        let accepted = if matches(&after) {
            true
        } else if matches(&before) {
            false
        } else {
            return Err("dispatch_recovery_required: project edit outcome cannot be reconciled; both assignments remain fenced".into());
        };
        let mut resolved = journal.clone();
        resolved["phase"] = serde_json::json!(if accepted {
            "accepted"
        } else {
            "project-edit-rejected"
        });
        resolved["assignment_lifetime"] = serde_json::json!("quiescent-retag-v1");
        state.projects.save_dispatch_admission(&key, &resolved)?;
        if accepted {
            let keep = journal["acquired"]
                .as_array()
                .and_then(|leases| leases.last())
                .and_then(|id| id.as_str());
            state.projects.retain_attempt_lease(&key, keep)?;
        } else {
            for lease in journal["acquired"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_str())
            {
                state.projects.expire_lease(lease)?;
            }
        }
        return state.projects.clear_dispatch_admission(&key);
    }
    match journal["phase"].as_str() {
        Some("project-edit-rejected") => {
            for lease in journal["acquired"].as_array().into_iter().flatten().filter_map(|v| v.as_str()) { state.projects.expire_lease(lease)?; }
        }
        Some("rejected" | "preparing") => {
            let project: crate::api::mission_store::MissionProject = serde_json::from_value(journal["before"]["project"].clone()).map_err(|e| e.to_string())?;
            let title: Option<String> = serde_json::from_value(journal["before"]["title"].clone()).map_err(|e| e.to_string())?;
            if journal["identity_changed"] != false {
            store.update_mission_project(id, crate::api::mission_store::MissionProjectPatch {
                title: Some(title), project: Some(project.project), track: Some(project.track), github_pr: Some(project.github_pr), tag_patch: Some(crate::api::mission_store::MissionTagPatch::capabilities(&project.tags)), ..Default::default()
            }).await?;
            }
            if journal["phase"] != "preparing" && journal["actor_may_have_started"] != false {
            // Legacy receipts did not retain status metadata. Refuse to invent
            // diagnostics and timestamps while treating that rollback as complete.
            let metadata = serde_json::from_value(journal["before"]["status_metadata"].clone())
                .map_err(|e| format!("dispatch_recovery_required: missing status snapshot: {e}"))?;
            restore_status(store, state, id, &metadata).await?;
            }
            for lease in journal["acquired"].as_array().into_iter().flatten().filter_map(|v| v.as_str()) { state.projects.expire_lease(lease)?; }
        }
        Some("accepted") => {
            if journal["assignment_lifetime"].as_str() != Some("quiescent-retag-v1")
                && journal["before"]["project"] != journal["after"]["project"]
            {
                return Err("dispatch_recovery_required: legacy accepted retag has no execution-quiescence proof; both assignments remain fenced".into());
            }
            let keep = journal["acquired"].as_array().and_then(|leases| leases.last()).and_then(|id| id.as_str());
            state.projects.retain_attempt_lease(&key, keep)?;
        }

        _ => return Err(format!("dispatch_recovery_required: mission {id} has an unknown admission outcome; both assignments remain fenced")),
    }
    state.projects.clear_dispatch_admission(&key)
}

pub(super) async fn finish_project_edit(
    state: &Arc<AppState>,
    store: &Arc<dyn MissionStore>,
    id: Uuid,
    accepted: bool,
) -> Result<(), String> {
    let key = id.to_string();
    let mut journal = state
        .projects
        .dispatch_admission(&key)?
        .ok_or_else(|| "project edit receipt missing".to_string())?;
    journal["phase"] = serde_json::json!(if accepted {
        "accepted"
    } else {
        "project-edit-rejected"
    });
    journal["assignment_lifetime"] = serde_json::json!("quiescent-retag-v1");
    state.projects.save_dispatch_admission(&key, &journal)?;
    recover_dispatch(state, store, id).await
}

/// Periodic recovery must also reach missions with no HTTP session since
/// restart. Opening their existing store does not start a runner or grant
/// authority to a different user; the receipt names the exact mission.
pub(crate) async fn recover_sweep(state: &Arc<AppState>) -> Result<(), String> {
    for (attempt, journal) in state.projects.dispatch_admissions()? {
        if journal["phase"] == "pending" {
            continue;
        }
        let Ok(id) = Uuid::parse_str(&attempt) else {
            continue;
        };
        let recovered = async {
            if let Some((store, _)) = state.control.find_mission_store_owner(id).await? {
                return recover_dispatch(state, &store, id).await;
            }
            let inventory = state.control.mission_store_inventory().await?;
            for path in inventory.offline_sqlite {
                let store: Arc<dyn MissionStore> = Arc::new(
                    crate::api::mission_store::SqliteMissionStore::open_for_admission_recovery(
                        path,
                    )
                    .await?,
                );
                if store.get_mission(id).await?.is_some() {
                    return recover_dispatch(state, &store, id).await;
                }
            }
            for user in inventory.offline_file_users {
                let store: Arc<dyn MissionStore> = Arc::new(
                    crate::api::mission_store::FileMissionStore::new(
                        inventory.base_dir.clone(),
                        &user,
                    )
                    .await?,
                );
                if store.get_mission(id).await?.is_some() {
                    return recover_dispatch(state, &store, id).await;
                }
            }
            Err("admission recovery owner is not available".to_string())
        }
        .await;
        if let Err(error) = recovered {
            tracing::warn!(mission_id = %id, %error, "Admission cleanup remains fenced for retry");
        }
    }
    Ok(())
}

/// Presentation status alone cannot prove that old work has stopped. The actor
/// supplies its runner/queue state while holding the admission mutex; durable
/// runs also fence execution owned by another cooperating process. Pending
/// objectives are deliberately refused, never concatenated across assignments.
pub(super) async fn require_quiescent(
    state: &Arc<AppState>,
    store: &Arc<dyn MissionStore>,
    mission: &Mission,
    actor_busy: bool,
) -> Result<(), String> {
    if actor_busy
        || matches!(
            mission.status,
            MissionStatus::Active | MissionStatus::Pending
        )
        || store.get_active_mission_run(mission.id).await?.is_some()
        || store
            .get_deferred_goal(mission.id)
            .await?
            .is_some_and(|goal| !goal.trim().is_empty())
        || crate::remote_node::job_ledger::load(&state.config.working_dir)
            .await
            .map_err(|error| error.to_string())?
            .iter()
            .any(|handle| handle.mission_id == mission.id)
    {
        return Err("assignment_busy: stop and drain old running, queued, or Pending work before changing assignment".into());
    }
    Ok(())
}

async fn prepare(
    admission: DispatchAdmission,
    id: Uuid,
    resume: bool,
    actor_busy: bool,
    actor_before: Option<serde_json::Value>,
) -> Result<AdmissionReceipt, String> {
    let file_guard = durable_lock(&admission.state.config).await?;
    let store = admission.store.clone();
    recover_dispatch(&admission.state, &store, id).await?;
    let before = store
        .get_mission(id)
        .await?
        .ok_or_else(|| format!("Mission {id} not found"))?;
    if resume
        && !matches!(
            before.status,
            MissionStatus::Interrupted
                | MissionStatus::Blocked
                | MissionStatus::Failed
                | MissionStatus::Paused
        )
    {
        return Err(format!(
            "Mission {id} cannot be resumed (status: {})",
            before.status
        ));
    }
    let next =
        writer_reuse_or_conflict(&before, &admission.patch).map_err(|(_, message)| message)?;
    let mut after = before.clone();
    after.title = next.title;
    after.project.github_pr = next.github_pr;
    after.project.track = next.track;
    if before.project.github_pr != after.project.github_pr
        || before.project.track != after.project.track
    {
        require_quiescent(&admission.state, &store, &before, actor_busy).await?;
    }
    let wants_writer = mission_is_pr_writer_in_store(&store, &after).await?
        || admission
            .patch
            .work_hint
            .as_deref()
            .or(admission.internal_work_hint.as_deref())
            .is_some_and(|content| message_requests_pr_writer(&after, content));
    let _pr_guard = acquire_durable_pr_writer_lock(&admission.state.control).await?;
    if wants_writer {
        if let Some(pr) = &after.project.github_pr {
            if let Some(existing) =
                find_existing_pr_writer_global(&admission.state.control, pr, Some(id)).await?
            {
                return Err(format!(
                    "PR writer lease is already held by mission {}",
                    existing.id
                ));
            }
            after
                .project
                .tags
                .retain(|tag| tag != "pr-readonly" && tag != "pr-writer");
            after.project.tags.push("pr-writer".into());
        }
    }
    let mut lease_request = None;
    if let (Some(slug), Some(track)) = (&after.project.project, &after.project.track) {
        let outcome = admission.state.projects.absorb_track(
            slug,
            track,
            after.title.as_deref(),
            super::super::track_leases::pr_number(after.project.github_pr.as_deref()),
        )?;
        let mode = super::super::track_leases::lease_mode(
            wants_writer.then_some(true),
            &after.project.tags,
            after.project.intent.as_deref(),
        );
        // A fresh key makes promotion a checked writer acquisition, preserving
        // the old reader lease until acceptance/rollback has been resolved.
        let key = format!("admission:{}", Uuid::new_v4());
        let request = super::super::track_leases::lease_request(
            slug,
            &outcome.key,
            &id.to_string(),
            mode,
            Some(&key),
        );
        lease_request = Some(request);
        after.project.track = Some(outcome.key);
    }
    let changed = before.title != after.title
        || before.project.github_pr != after.project.github_pr
        || before.project.track != after.project.track
        || before.project.tags != after.project.tags;
    let mut receipt = AdmissionReceipt {
        _file_guard: file_guard,
        state: admission.state,
        store,
        before,
        after,
        acquired: Vec::new(),
        changed,
        actor_before,
        actor_may_have_started: false,
    };
    let lease = receipt
        .state
        .projects
        .begin_dispatch_admission(
            &id.to_string(),
            &receipt.journal("preparing"),
            lease_request.as_ref(),
        )
        .map_err(|error| match lease_request.as_ref() {
            Some(request) => {
                super::super::track_leases::owned_body(&request.slug, &request.track, &error)
                    .to_string()
            }
            None => error.to_string(),
        })?;
    receipt.acquired = lease.into_iter().map(|lease| lease.id).collect();
    #[cfg(test)]
    super::dispatch_admission_tests::before_admission_identity_write(id);
    if changed {
        if let Err(error) = receipt
            .store
            .update_mission_project(id, identity_patch(&receipt.after))
            .await
        {
            // The store's identity write is atomic. It failed without changing
            // metadata; retrying that same failing write is not compensation.
            receipt.changed = false;
            receipt.finish(false).await?;
            return Err(error);
        }
    }
    receipt.actor_may_have_started = true;
    // Once this marker is durable the actor may observe the command. A crash
    // before it is safe to roll back; afterwards an unknown outcome stays fenced.
    receipt
        .state
        .projects
        .save_dispatch_admission(&id.to_string(), &receipt.journal("pending"))?;
    Ok(receipt)
}

/// Replace the response sender so rollback/ownership cleanup precedes the HTTP
/// acknowledgement. The admission lock survives client cancellation and every
/// actor `continue`/early rejection. Lost responses keep a pending recovery fence.
#[cfg(test)]
pub(super) async fn admit_dispatch(
    admission: DispatchAdmission,
    command: ControlCommand,
    guard: tokio::sync::MutexGuard<'static, ()>,
) -> Option<ControlCommand> {
    admit_dispatch_with_lifetime(admission, command, guard, false, None).await
}

pub(super) async fn admit_dispatch_with_lifetime(
    admission: DispatchAdmission,
    mut command: ControlCommand,
    guard: tokio::sync::MutexGuard<'static, ()>,
    actor_busy: bool,
    actor_before: Option<serde_json::Value>,
) -> Option<ControlCommand> {
    let (id, resume) = match &command {
        ControlCommand::UserMessage {
            target_mission_id: Some(id),
            ..
        } => (*id, false),
        ControlCommand::UserMessage {
            target_mission_id: None,
            ..
        } => return Some(command),
        ControlCommand::ResumeMission { mission_id, .. } => (*mission_id, true),
        _ => unreachable!("admission supports only send and resume"),
    };
    if let ControlCommand::ResumeMission {
        content,
        skip_message,
        ..
    } = &mut command
    {
        *content = admission
            .patch
            .work_hint
            .clone()
            .filter(|hint| !hint.trim().is_empty());
        if content.is_some() {
            *skip_message = false;
        }
    }
    let receipt = prepare(admission, id, resume, actor_busy, actor_before).await;
    match command {
        ControlCommand::UserMessage {
            id,
            content,
            agent,
            target_mission_id,
            strict,
            source,
            respond,
        } => {
            let receipt = match receipt {
                Ok(receipt) => receipt,
                Err(error) => {
                    let _ = respond.send(UserMessageAck::Rejected(error));
                    return None;
                }
            };
            let (tx, rx) = oneshot::channel();
            tokio::spawn(async move {
                let _guard = guard;
                let ack = match rx.await {
                    Ok(ack) => ack,
                    Err(_) => {
                        // The actor could have enqueued or started work before
                        // losing its response. Preserve the pending journal and
                        // both claims; rollback would retag a possibly live runner.
                        let _ = respond.send(UserMessageAck::Rejected(
                            "dispatch_recovery_required: actor response lost; acceptance is unknown and both assignments remain fenced".into(),
                        ));
                        return;
                    }
                };
                let ack = if ack == UserMessageAck::Dropped {
                    UserMessageAck::Rejected("Actor refused delivery".into())
                } else {
                    ack
                };
                let accepted = matches!(ack, UserMessageAck::Delivered | UserMessageAck::Queued);
                let result = receipt.finish(accepted).await;
                let ack = match result {
                    Ok(()) => ack,
                    Err(error) if accepted => {
                        tracing::error!(%error, "Accepted dispatch cleanup pending; ownership retained");
                        ack
                    }
                    Err(error) => UserMessageAck::Rejected(format!(
                        "dispatch_recovery_required: ownership retained: {error}"
                    )),
                };
                let _ = respond.send(ack);
            });
            Some(ControlCommand::UserMessage {
                id,
                content,
                agent,
                target_mission_id,
                strict,
                source,
                respond: tx,
            })
        }
        ControlCommand::ResumeMission {
            mission_id,
            content,
            clean_workspace,
            skip_message,
            respond,
        } => {
            let receipt = match receipt {
                Ok(receipt) => receipt,
                Err(error) => {
                    let _ = respond.send(Err(error));
                    return None;
                }
            };
            let (tx, rx) = oneshot::channel();
            tokio::spawn(async move {
                let _guard = guard;
                let result: Result<Mission, String> = match rx.await {
                    Ok(result) => result,
                    Err(_) => {
                        let _ = respond.send(Err(
                            "dispatch_recovery_required: actor response lost; acceptance is unknown and both assignments remain fenced".into(),
                        ));
                        return;
                    }
                };
                let accepted = result.is_ok();
                let cleanup = receipt.finish(accepted).await;
                let result = match cleanup {
                    Ok(()) => result,
                    Err(error) if accepted => {
                        tracing::error!(%error, "Accepted resume cleanup pending; ownership retained");
                        result
                    }
                    Err(error) => Err(format!(
                        "dispatch_recovery_required: ownership retained: {error}"
                    )),
                };
                let _ = respond.send(result);
            });
            Some(ControlCommand::ResumeMission {
                mission_id,
                content,
                clean_workspace,
                skip_message,
                respond: tx,
            })
        }
        _ => unreachable!(),
    }
}

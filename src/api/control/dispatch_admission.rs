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
        tags: Some(mission.project.tags.clone()),
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
}

impl AdmissionReceipt {
    fn journal(&self, phase: &str) -> serde_json::Value {
        serde_json::json!({
            "phase": phase,
            "assignment_lifetime": "quiescent-retag-v1",
            "before": { "title": self.before.title, "project": self.before.project, "status": self.before.status },
            "after": { "title": self.after.title, "project": self.after.project },
            "acquired": self.acquired,
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
            if let Some(current) = self.store.get_mission(self.before.id).await? {
                if current.status == MissionStatus::Active
                    && self.before.status != MissionStatus::Active
                {
                    self.store
                        .update_mission_status(self.before.id, self.before.status)
                        .await?;
                }
            }
            for id in self.acquired {
                self.state.projects.expire_lease(&id)?;
            }
        }
        self.state.projects.clear_dispatch_admission(&id)?;
        Ok(())
    }
}

/// Retry a known outcome after a cross-store failure. An outcome lost to a
/// crash is deliberately quarantined: never guess that queued work was not
/// accepted, or release the old identity while a runner might still use it.
pub(super) async fn recover_dispatch(
    state: &Arc<AppState>,
    store: &Arc<dyn MissionStore>,
    id: Uuid,
) -> Result<(), String> {
    let key = id.to_string();
    let Some(journal) = state.projects.dispatch_admission(&key)? else {
        return Ok(());
    };
    match journal["phase"].as_str() {
        Some("rejected") => {
            let project: crate::api::mission_store::MissionProject = serde_json::from_value(journal["before"]["project"].clone()).map_err(|e| e.to_string())?;
            let title: Option<String> = serde_json::from_value(journal["before"]["title"].clone()).map_err(|e| e.to_string())?;
            store.update_mission_project(id, crate::api::mission_store::MissionProjectPatch {
                title: Some(title), project: Some(project.project), track: Some(project.track), github_pr: Some(project.github_pr), tags: Some(project.tags), ..Default::default()
            }).await?;
            let status: MissionStatus = serde_json::from_value(journal["before"]["status"].clone()).map_err(|e| e.to_string())?;
            if store.get_mission(id).await?.is_some_and(|mission| mission.status == MissionStatus::Active && status != MissionStatus::Active) {
                store.update_mission_status(id, status).await?;
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
    let mut acquired = Vec::new();
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
        let lease = admission
            .state
            .projects
            .acquire_track_lease(&request)
            .map_err(|error| {
                super::super::track_leases::owned_body(slug, &outcome.key, &error).to_string()
            })?;
        acquired.push(lease.id);
        after.project.track = Some(outcome.key);
    }
    let changed = before.title != after.title
        || before.project.github_pr != after.project.github_pr
        || before.project.track != after.project.track
        || before.project.tags != after.project.tags;
    let receipt = AdmissionReceipt {
        _file_guard: file_guard,
        state: admission.state,
        store,
        before,
        after,
        acquired,
        changed,
    };
    if let Err(error) = receipt
        .state
        .projects
        .save_dispatch_admission(&id.to_string(), &receipt.journal("pending"))
    {
        for lease in &receipt.acquired {
            receipt.state.projects.expire_lease(lease)?;
        }
        return Err(error);
    }
    if changed {
        if let Err(error) = receipt
            .store
            .update_mission_project(id, identity_patch(&receipt.after))
            .await
        {
            // Atomic identity persistence failed: the old assignment and all
            // its leases are still present. Drop only provisional acquisitions.
            for lease in &receipt.acquired {
                receipt.state.projects.expire_lease(lease)?;
            }
            receipt
                .state
                .projects
                .clear_dispatch_admission(&id.to_string())?;
            return Err(error);
        }
    }
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
    admit_dispatch_with_lifetime(admission, command, guard, false).await
}

pub(super) async fn admit_dispatch_with_lifetime(
    admission: DispatchAdmission,
    mut command: ControlCommand,
    guard: tokio::sync::MutexGuard<'static, ()>,
    actor_busy: bool,
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
    let receipt = prepare(admission, id, resume, actor_busy).await;
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
                        "Dispatch recovery required; ownership retained: {error}"
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
                        "Dispatch recovery required; ownership retained: {error}"
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

package sh.sandboxed.dashboard.ui.control

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.OpenInNew
import androidx.compose.material.icons.automirrored.filled.Send
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.AttachFile
import androidx.compose.material.icons.filled.Cancel
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Computer
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.Flag
import androidx.compose.material.icons.filled.History
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material.icons.filled.Schedule
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.FilterChip
import androidx.compose.material3.FilterChipDefaults
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TextFieldDefaults
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.core.net.toUri
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import sh.sandboxed.dashboard.data.AppContainer
import sh.sandboxed.dashboard.data.Backend
import sh.sandboxed.dashboard.data.BackendAgent
import sh.sandboxed.dashboard.data.BuiltinCommandsResponse
import sh.sandboxed.dashboard.data.ChatMessage
import sh.sandboxed.dashboard.data.ChatMessageKind
import sh.sandboxed.dashboard.data.Mission
import sh.sandboxed.dashboard.data.MissionStatus
import sh.sandboxed.dashboard.data.Provider
import sh.sandboxed.dashboard.data.QueuedMessage
import sh.sandboxed.dashboard.data.RunningMissionInfo
import sh.sandboxed.dashboard.data.SharedFile
import sh.sandboxed.dashboard.data.SlashCommand
import sh.sandboxed.dashboard.data.Workspace
import sh.sandboxed.dashboard.ui.components.ErrorBanner
import sh.sandboxed.dashboard.ui.components.GlassCard
import sh.sandboxed.dashboard.ui.components.StatusBadge
import sh.sandboxed.dashboard.ui.components.ToolUiWidget
import sh.sandboxed.dashboard.ui.theme.Palette
import sh.sandboxed.dashboard.util.Haptics

@Composable
fun ControlScreen(
    container: AppContainer,
    onOpenAutomations: (String) -> Unit,
    onOpenDesktop: (String) -> Unit,
) {
    val vm = remember { ControlViewModel(container) }
    val state by vm.state.collectAsState()
    val listState = rememberLazyListState()
    val haptics = remember { Haptics(container) }
    var showNewMission by remember { mutableStateOf(false) }
    var showMissionSwitcher by remember { mutableStateOf(false) }
    var showWorkers by remember { mutableStateOf(false) }
    val slashSuggestions = remember(state.draft, state.mission?.backend, state.slashCommands) {
        visibleSlashSuggestions(
            draft = state.draft,
            backend = state.mission?.backend,
            catalog = state.slashCommands,
        )
    }
    val slashPanelActive = isSlashPanelActive(state.draft)

    LaunchedEffect(state.messages.size) {
        if (state.messages.isNotEmpty()) listState.animateScrollToItem(state.messages.lastIndex)
    }

    LaunchedEffect(showMissionSwitcher) {
        if (showMissionSwitcher) vm.loadRecentMissions()
    }

    LaunchedEffect(state.desktopOpenRequest) {
        if (state.desktopOpenRequest > 0) onOpenDesktop(state.desktopDisplay)
    }

    Box(Modifier.fillMaxSize()) {
        Column(Modifier.fillMaxSize().imePadding()) {
            TopBar(
                mission = state.mission,
                connected = state.isConnected,
                canResume = state.mission?.let { it.status.canResume || it.resumable } == true,
                workerCount = state.childMissions.size,
                runningCount = state.parallel.size,
                runState = state.runState,
                progress = state.progress,
                onResume = { haptics.success(); vm.resume() },
                onAutomations = { state.mission?.id?.let(onOpenAutomations) },
                onNewMission = { showNewMission = true },
                onSwitchMissions = { showMissionSwitcher = true },
                onWorkers = { showWorkers = true },
                onDesktop = { onOpenDesktop(state.desktopDisplay) },
            )
            if (state.parallel.isNotEmpty()) {
                ParallelBar(state.parallel, state.mission?.id) { haptics.selection(); vm.switchMission(it) }
            }
            state.goalStatus?.takeIf { it.isNotBlank() }?.let { GoalBanner(it) }
            state.error?.let { Box(Modifier.padding(horizontal = 16.dp, vertical = 8.dp)) { ErrorBanner(it) } }
            if (state.queue.isNotEmpty()) QueueBar(state.queue, vm::deleteQueueItem, vm::clearQueue)
            LazyColumn(
                state = listState,
                modifier = Modifier.weight(1f).fillMaxWidth(),
                contentPadding = PaddingValues(16.dp),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                items(state.messages, key = { it.id }) { msg -> MessageRow(msg) }
            }
            if (slashPanelActive && (slashSuggestions.isNotEmpty() || state.slashCommandsLoading)) {
                SlashSuggestions(
                    commands = slashSuggestions,
                    loading = state.slashCommandsLoading && slashSuggestions.isEmpty(),
                    onSelect = {
                        haptics.selection()
                        vm.applySlashCommand(it)
                    },
                )
            }
            Composer(
                value = state.draft,
                onChange = vm::setDraft,
                onSend = { haptics.medium(); vm.send() },
                onCancel = { haptics.error(); vm.cancel() },
                isSending = state.isSending,
            )
        }
    }

    if (showNewMission) {
        NewMissionDialog(
            container = container,
            onDismiss = { showNewMission = false },
            onCreate = { options ->
                showNewMission = false
                haptics.success()
                vm.createMission(options)
            },
        )
    }
    if (showMissionSwitcher) {
        MissionSwitcherDialog(
            currentMissionId = state.mission?.id,
            running = state.parallel,
            recent = state.recentMissions,
            loading = state.loadingRecent,
            onDismiss = { showMissionSwitcher = false },
            onOpen = {
                showMissionSwitcher = false
                haptics.selection()
                vm.switchMission(it)
            },
            onResume = {
                showMissionSwitcher = false
                haptics.success()
                vm.resumeMission(it)
            },
            onFollowUp = {
                showMissionSwitcher = false
                haptics.selection()
                vm.createFollowUpMission(it)
            },
            onCancel = {
                haptics.error()
                vm.cancelMission(it)
            },
            onDelete = {
                haptics.error()
                vm.deleteMission(it)
            },
            onNewMission = {
                showMissionSwitcher = false
                showNewMission = true
            },
        )
    }
    if (showWorkers) {
        WorkerDialog(
            workers = state.childMissions,
            running = state.parallel,
            onDismiss = { showWorkers = false },
            onOpen = {
                showWorkers = false
                vm.switchMission(it)
            },
        )
    }
}

@Composable
private fun TopBar(
    mission: Mission?,
    connected: Boolean,
    canResume: Boolean,
    workerCount: Int,
    runningCount: Int,
    runState: ControlRunState,
    progress: ExecutionProgress?,
    onResume: () -> Unit,
    onAutomations: () -> Unit,
    onNewMission: () -> Unit,
    onSwitchMissions: () -> Unit,
    onWorkers: () -> Unit,
    onDesktop: () -> Unit,
) {
    Column(Modifier.fillMaxWidth().background(Palette.BackgroundSecondary).padding(horizontal = 16.dp, vertical = 12.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f)) {
                Text(mission?.title ?: "New mission", style = MaterialTheme.typography.titleMedium, color = Palette.TextPrimary, maxLines = 1)
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    Text(
                        if (connected) "Connected" else "Reconnecting…",
                        style = MaterialTheme.typography.bodySmall,
                        color = if (connected) Palette.Success else Palette.Warning,
                    )
                    Text("•", color = Palette.TextTertiary, style = MaterialTheme.typography.bodySmall)
                    Text(runState.label, color = runStateColor(runState), style = MaterialTheme.typography.bodySmall)
                    progress?.takeIf { it.total > 0 }?.let {
                        Text("•", color = Palette.TextTertiary, style = MaterialTheme.typography.bodySmall)
                        Text(it.displayText, color = Palette.Success, style = MaterialTheme.typography.bodySmall)
                    }
                }
            }
            mission?.status?.let { StatusBadge(it) }
            if (canResume) {
                Spacer(Modifier.width(8.dp))
                IconButton(onClick = onResume) { Icon(Icons.Filled.PlayArrow, "Resume", tint = Palette.Accent) }
            }
            if (mission != null) {
                IconButton(onClick = onAutomations) { Icon(Icons.Filled.Settings, "Automations", tint = Palette.TextSecondary) }
            }
            IconButton(onClick = onDesktop) { Icon(Icons.Filled.Computer, "Desktop", tint = Palette.TextSecondary) }
            if (workerCount > 0) {
                IconButton(onClick = onWorkers) {
                    Text("W$workerCount", color = Palette.AccentLight, style = MaterialTheme.typography.labelMedium)
                }
            }
            IconButton(onClick = onSwitchMissions) {
                Box(contentAlignment = Alignment.Center) {
                    Icon(Icons.Filled.History, "Missions", tint = if (runningCount > 0) Palette.Accent else Palette.TextSecondary)
                    if (runningCount > 0) {
                        Text(runningCount.toString(), color = Palette.TextPrimary, style = MaterialTheme.typography.labelSmall, modifier = Modifier.padding(top = 18.dp))
                    }
                }
            }
            IconButton(onClick = onNewMission) { Icon(Icons.Filled.Add, "New mission", tint = Palette.Accent) }
        }
        if (mission != null && (mission.metadataModel != null || mission.metadataSource != null || mission.workspaceName != null)) {
            Spacer(Modifier.height(4.dp))
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                mission.metadataModel?.let { Tag(it) }
                mission.metadataSource?.let { Tag(it) }
                mission.workspaceName?.let { Tag(it) }
            }
        }
    }
}

private fun runStateColor(runState: ControlRunState): Color = when (runState) {
    ControlRunState.IDLE -> Palette.TextSecondary
    ControlRunState.RUNNING -> Palette.Success
    ControlRunState.WAITING_FOR_TOOL -> Palette.Warning
}

@Composable
private fun Tag(text: String) {
    Text(
        text,
        color = Palette.TextTertiary,
        style = MaterialTheme.typography.labelSmall,
        modifier = Modifier
            .background(Palette.BackgroundTertiary, RoundedCornerShape(4.dp))
            .padding(horizontal = 6.dp, vertical = 2.dp),
    )
}

@Composable
private fun GoalBanner(status: String) {
    val color = when (status) {
        "complete" -> Palette.Success
        "paused", "budgetLimited" -> Palette.Warning
        "active" -> Palette.Info
        else -> Palette.TextTertiary
    }
    Row(
        Modifier.fillMaxWidth().background(color.copy(alpha = 0.12f)).padding(horizontal = 16.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Icon(Icons.Filled.Flag, null, tint = color, modifier = Modifier.size(16.dp))
        Spacer(Modifier.width(8.dp))
        Text("/goal · $status", color = color, style = MaterialTheme.typography.labelMedium)
    }
}

@Composable
private fun QueueBar(queue: List<QueuedMessage>, onDelete: (String) -> Unit, onClear: () -> Unit) {
    Column(Modifier.fillMaxWidth().background(Palette.BackgroundSecondary).padding(horizontal = 12.dp, vertical = 8.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Icon(Icons.Filled.Schedule, null, tint = Palette.AccentLight, modifier = Modifier.size(14.dp))
            Spacer(Modifier.width(6.dp))
            Text("Queued · ${queue.size}", color = Palette.AccentLight, style = MaterialTheme.typography.labelMedium, modifier = Modifier.weight(1f))
            IconButton(onClick = onClear) { Icon(Icons.Filled.Close, "Clear queue", tint = Palette.TextTertiary) }
        }
        LazyRow(horizontalArrangement = Arrangement.spacedBy(6.dp), modifier = Modifier.fillMaxWidth()) {
            items(queue, key = { it.id }) { q ->
                Row(
                    modifier = Modifier
                        .background(Palette.Card, RoundedCornerShape(8.dp))
                        .border(1.dp, Palette.Border, RoundedCornerShape(8.dp))
                        .padding(horizontal = 8.dp, vertical = 6.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Text(q.displayContent, color = Palette.TextPrimary, style = MaterialTheme.typography.bodySmall, maxLines = 1)
                    Spacer(Modifier.width(6.dp))
                    Icon(Icons.Filled.Close, "Remove", tint = Palette.TextTertiary, modifier = Modifier.size(14.dp).clickable { onDelete(q.id) })
                }
            }
        }
    }
}

@Composable
private fun ParallelBar(running: List<RunningMissionInfo>, currentId: String?, onSwitch: (String) -> Unit) {
    LazyRow(
        modifier = Modifier
            .fillMaxWidth()
            .background(Palette.BackgroundSecondary)
            .padding(horizontal = 12.dp, vertical = 8.dp),
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        items(running, key = { it.missionId }) { r ->
            val color = when {
                r.isSeverelyStalled -> Palette.Error
                r.isStalled -> Palette.Warning
                r.isRunning -> Palette.Success
                else -> Palette.TextTertiary
            }
            val active = r.missionId == currentId
            Row(
                modifier = Modifier
                    .background(if (active) Palette.Accent.copy(alpha = 0.16f) else Palette.Card, RoundedCornerShape(999.dp))
                    .border(1.dp, if (active) Palette.Accent else Palette.Border, RoundedCornerShape(999.dp))
                    .clickable { onSwitch(r.missionId) }
                    .padding(horizontal = 10.dp, vertical = 6.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Box(Modifier.size(8.dp).background(color, RoundedCornerShape(4.dp)))
                Spacer(Modifier.width(6.dp))
                Text(r.title?.take(20) ?: r.missionId.take(8), style = MaterialTheme.typography.labelMedium, color = Palette.TextPrimary)
            }
        }
    }
}

@Composable
private fun NewMissionDialog(
    container: AppContainer,
    onDismiss: () -> Unit,
    onCreate: (NewMissionOptions) -> Unit,
) {
    var workspaces by remember { mutableStateOf<List<Workspace>>(emptyList()) }
    var backends by remember { mutableStateOf<List<Backend>>(emptyList()) }
    var agentsByBackend by remember { mutableStateOf<Map<String, List<BackendAgent>>>(emptyMap()) }
    var providers by remember { mutableStateOf<List<Provider>>(emptyList()) }
    var selectedWorkspaceId by remember { mutableStateOf<String?>(null) }
    var selectedBackend by remember { mutableStateOf("") }
    var selectedAgent by remember { mutableStateOf("") }
    var selectedModel by remember { mutableStateOf("") }
    var loading by remember { mutableStateOf(true) }

    LaunchedEffect(Unit) {
        val settings = container.cached.value
        workspaces = runCatching { container.api.listWorkspaces() }.getOrNull().orEmpty()
        backends = runCatching { container.api.listBackends() }.getOrNull().orEmpty()
        agentsByBackend = backends.associate { backend ->
            backend.id to runCatching { container.api.listBackendAgents(backend.id) }.getOrNull().orEmpty()
        }
        providers = runCatching { container.api.listProviders() }.getOrNull()?.providers.orEmpty()

        selectedWorkspaceId = workspaces.firstOrNull { it.isDefault }?.id ?: workspaces.firstOrNull()?.id
        selectedBackend = settings.defaultBackend.takeIf { saved -> backends.any { it.id == saved } }
            ?: backends.firstOrNull()?.id.orEmpty()
        val selectedAgents = agentsByBackend[selectedBackend].orEmpty()
        selectedAgent = settings.defaultAgent.takeIf { saved -> selectedAgents.any { it.id == saved } }
            ?: selectedAgents.firstOrNull()?.id.orEmpty()
        loading = false
    }

    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("New mission", color = Palette.TextPrimary) },
        text = {
            if (loading) {
                Box(Modifier.fillMaxWidth().padding(24.dp), contentAlignment = Alignment.Center) {
                    CircularProgressIndicator(color = Palette.Accent)
                }
            } else {
                LazyColumn(
                    modifier = Modifier.fillMaxWidth().heightIn(max = 520.dp),
                    verticalArrangement = Arrangement.spacedBy(12.dp),
                ) {
                    item { DialogSection("Workspace") }
                    items(workspaces, key = { it.id }) { workspace ->
                        SelectRow(
                            title = workspace.name,
                            subtitle = "${workspace.workspaceType} · ${workspace.path}",
                            selected = selectedWorkspaceId == workspace.id,
                        ) { selectedWorkspaceId = workspace.id }
                    }

                    item { DialogSection("Agent") }
                    item {
                        LazyRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                            items(backends, key = { it.id }) { backend ->
                                FilterChip(
                                    selected = selectedBackend == backend.id,
                                    onClick = {
                                        selectedBackend = backend.id
                                        selectedAgent = agentsByBackend[backend.id].orEmpty().firstOrNull()?.id.orEmpty()
                                        selectedModel = ""
                                    },
                                    label = { Text(backend.name, style = MaterialTheme.typography.labelSmall) },
                                    colors = dialogChipColors(),
                                )
                            }
                        }
                    }
                    items(agentsByBackend[selectedBackend].orEmpty(), key = { it.id }) { agent ->
                        SelectRow(
                            title = agent.name,
                            subtitle = selectedBackend,
                            selected = selectedAgent == agent.id,
                        ) { selectedAgent = agent.id }
                    }

                    item { DialogSection("Model override") }
                    item {
                        SelectRow(
                            title = "Default",
                            subtitle = "Use the selected agent or server default",
                            selected = selectedModel.isBlank(),
                        ) { selectedModel = "" }
                    }
                    items(filteredProviders(providers, selectedBackend), key = { it.id }) { provider ->
                        Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                            Text(provider.name, color = Palette.TextTertiary, style = MaterialTheme.typography.labelMedium)
                            provider.models.take(12).forEach { model ->
                                val value = if (selectedBackend == "opencode") "${provider.id}/${model.id}" else model.id
                                SelectRow(
                                    title = model.name,
                                    subtitle = value,
                                    selected = selectedModel == value,
                                ) { selectedModel = value }
                            }
                        }
                    }
                }
            }
        },
        confirmButton = {
            Button(
                onClick = {
                    onCreate(
                        NewMissionOptions(
                            workspaceId = selectedWorkspaceId,
                            agent = selectedAgent.takeIf { it.isNotBlank() },
                            modelOverride = selectedModel.takeIf { it.isNotBlank() },
                            backend = selectedBackend.takeIf { it.isNotBlank() },
                        )
                    )
                },
                enabled = !loading && selectedWorkspaceId != null && selectedBackend.isNotBlank(),
                colors = ButtonDefaults.buttonColors(containerColor = Palette.Accent),
            ) { Text("Create") }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
        containerColor = Palette.Card,
    )
}

@Composable
private fun MissionSwitcherDialog(
    currentMissionId: String?,
    running: List<RunningMissionInfo>,
    recent: List<Mission>,
    loading: Boolean,
    onDismiss: () -> Unit,
    onOpen: (String) -> Unit,
    onResume: (String) -> Unit,
    onFollowUp: (Mission) -> Unit,
    onCancel: (String) -> Unit,
    onDelete: (String) -> Unit,
    onNewMission: () -> Unit,
) {
    var query by remember { mutableStateOf("") }
    val normalized = query.trim().lowercase()
    val runningIds = running.map { it.missionId }.toSet()
    val visibleRecent = recent.filter { m ->
        normalized.isBlank() ||
            (m.title ?: "").lowercase().contains(normalized) ||
            (m.shortDescription ?: "").lowercase().contains(normalized) ||
            (m.agent ?: "").lowercase().contains(normalized) ||
            m.id.lowercase().contains(normalized)
    }
    val byId = recent.associateBy { it.id }

    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Missions", color = Palette.TextPrimary) },
        text = {
            LazyColumn(
                modifier = Modifier.fillMaxWidth().heightIn(max = 540.dp),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                item {
                    OutlinedTextField(
                        value = query,
                        onValueChange = { query = it },
                        singleLine = true,
                        label = { Text("Search") },
                        modifier = Modifier.fillMaxWidth(),
                        colors = dialogFieldColors(),
                    )
                }
                if (loading) item { LinearLoading() }
                if (running.isNotEmpty()) item { DialogSection("Running") }
                items(running, key = { it.missionId }) { info ->
                    MissionSwitcherRunningRow(
                        info = info,
                        mission = byId[info.missionId],
                        current = currentMissionId == info.missionId,
                        onOpen = { onOpen(info.missionId) },
                        onCancel = { onCancel(info.missionId) },
                    )
                }

                val nonRunning = visibleRecent.filterNot { it.id in runningIds }
                if (nonRunning.any { it.status == MissionStatus.ACTIVE || it.status == MissionStatus.PENDING }) {
                    item { DialogSection("Active & pending") }
                    items(nonRunning.filter { it.status == MissionStatus.ACTIVE || it.status == MissionStatus.PENDING }, key = { it.id }) { m ->
                        MissionSwitcherMissionRow(m, currentMissionId == m.id, onOpen, onResume, onFollowUp, onCancel, onDelete)
                    }
                }
                val completed = nonRunning.filter { it.status == MissionStatus.COMPLETED }
                if (completed.isNotEmpty()) {
                    item { DialogSection("Completed") }
                    items(completed, key = { it.id }) { m ->
                        MissionSwitcherMissionRow(m, currentMissionId == m.id, onOpen, onResume, onFollowUp, onCancel, onDelete)
                    }
                }
                val failed = nonRunning.filter { it.status == MissionStatus.FAILED || it.status == MissionStatus.NOT_FEASIBLE }
                if (failed.isNotEmpty()) {
                    item { DialogSection("Failed") }
                    items(failed, key = { it.id }) { m ->
                        MissionSwitcherMissionRow(m, currentMissionId == m.id, onOpen, onResume, onFollowUp, onCancel, onDelete)
                    }
                }
                val interrupted = nonRunning.filter { it.status == MissionStatus.INTERRUPTED || it.status == MissionStatus.BLOCKED }
                if (interrupted.isNotEmpty()) {
                    item { DialogSection("Interrupted") }
                    items(interrupted, key = { it.id }) { m ->
                        MissionSwitcherMissionRow(m, currentMissionId == m.id, onOpen, onResume, onFollowUp, onCancel, onDelete)
                    }
                }
            }
        },
        confirmButton = {
            Button(onClick = onNewMission, colors = ButtonDefaults.buttonColors(containerColor = Palette.Accent)) {
                Text("New")
            }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Close") } },
        containerColor = Palette.Card,
    )
}

@Composable
private fun WorkerDialog(
    workers: List<Mission>,
    running: List<RunningMissionInfo>,
    onDismiss: () -> Unit,
    onOpen: (String) -> Unit,
) {
    val runningIds = running.map { it.missionId }.toSet()
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Workers", color = Palette.TextPrimary) },
        text = {
            LazyColumn(
                modifier = Modifier.fillMaxWidth().heightIn(max = 460.dp),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                val active = workers.filter { it.id in runningIds || it.status == MissionStatus.ACTIVE || it.status == MissionStatus.PENDING }
                val completed = workers.filter { it.status == MissionStatus.COMPLETED }
                val failed = workers.filter { it.status == MissionStatus.FAILED || it.status == MissionStatus.NOT_FEASIBLE || it.status == MissionStatus.INTERRUPTED }
                if (active.isNotEmpty()) item { DialogSection("Running") }
                items(active, key = { it.id }) { worker -> WorkerRow(worker, running.firstOrNull { it.missionId == worker.id }, onOpen) }
                if (completed.isNotEmpty()) item { DialogSection("Completed") }
                items(completed, key = { it.id }) { worker -> WorkerRow(worker, null, onOpen) }
                if (failed.isNotEmpty()) item { DialogSection("Failed") }
                items(failed, key = { it.id }) { worker -> WorkerRow(worker, null, onOpen) }
                if (workers.isEmpty()) item { Text("No workers yet", color = Palette.TextTertiary) }
            }
        },
        confirmButton = { TextButton(onClick = onDismiss) { Text("Close") } },
        containerColor = Palette.Card,
    )
}

@Composable
private fun WorkerRow(worker: Mission, running: RunningMissionInfo?, onOpen: (String) -> Unit) {
    GlassCard(modifier = Modifier.fillMaxWidth(), onClick = { onOpen(worker.id) }) {
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(worker.title ?: worker.shortDescription ?: worker.id.take(8), color = Palette.TextPrimary, style = MaterialTheme.typography.titleSmall, modifier = Modifier.weight(1f))
                StatusBadge(worker.status)
            }
            running?.currentActivity?.takeIf { it.isNotBlank() }?.let {
                Text(it, color = Palette.TextTertiary, style = MaterialTheme.typography.bodySmall, maxLines = 2)
            }
        }
    }
}

@Composable
private fun MissionSwitcherRunningRow(
    info: RunningMissionInfo,
    mission: Mission?,
    current: Boolean,
    onOpen: () -> Unit,
    onCancel: () -> Unit,
) {
    GlassCard(modifier = Modifier.fillMaxWidth(), onClick = onOpen) {
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                val color = when {
                    info.isSeverelyStalled -> Palette.Error
                    info.isStalled -> Palette.Warning
                    info.isRunning -> Palette.Success
                    else -> Palette.TextTertiary
                }
                Box(Modifier.size(10.dp).background(color, RoundedCornerShape(5.dp)))
                Spacer(Modifier.width(8.dp))
                Text(info.title ?: mission?.title ?: info.missionId.take(8), color = if (current) Palette.AccentLight else Palette.TextPrimary, style = MaterialTheme.typography.titleSmall, modifier = Modifier.weight(1f))
                IconButton(onClick = onCancel) { Icon(Icons.Filled.Cancel, "Cancel", tint = Palette.Warning) }
            }
            info.currentActivity?.takeIf { it.isNotBlank() }?.let {
                Text(it, color = Palette.TextTertiary, style = MaterialTheme.typography.bodySmall, maxLines = 2)
            }
            if (info.queueLen > 0) Text("${info.queueLen} queued", color = Palette.Warning, style = MaterialTheme.typography.labelSmall)
        }
    }
}

@Composable
private fun MissionSwitcherMissionRow(
    mission: Mission,
    current: Boolean,
    onOpen: (String) -> Unit,
    onResume: (String) -> Unit,
    onFollowUp: (Mission) -> Unit,
    onCancel: (String) -> Unit,
    onDelete: (String) -> Unit,
) {
    GlassCard(modifier = Modifier.fillMaxWidth(), onClick = { onOpen(mission.id) }) {
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(mission.title ?: mission.shortDescription ?: mission.id.take(8), color = if (current) Palette.AccentLight else Palette.TextPrimary, style = MaterialTheme.typography.titleSmall, modifier = Modifier.weight(1f))
                StatusBadge(mission.status)
            }
            Row(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalAlignment = Alignment.CenterVertically) {
                Text(mission.updatedAt.take(19).replace('T', ' '), color = Palette.TextTertiary, style = MaterialTheme.typography.bodySmall, modifier = Modifier.weight(1f))
                if (mission.status.canResume || mission.resumable || mission.status == MissionStatus.FAILED || mission.status == MissionStatus.NOT_FEASIBLE) {
                    TextButton(onClick = { onResume(mission.id) }) { Text(if (mission.status == MissionStatus.FAILED || mission.status == MissionStatus.NOT_FEASIBLE) "Retry" else "Resume") }
                }
                if (mission.status != MissionStatus.ACTIVE && mission.status != MissionStatus.PENDING) {
                    TextButton(onClick = { onFollowUp(mission) }) { Text("Follow up") }
                } else {
                    IconButton(onClick = { onCancel(mission.id) }) { Icon(Icons.Filled.Cancel, "Cancel", tint = Palette.Warning) }
                }
                if (mission.status != MissionStatus.ACTIVE && mission.status != MissionStatus.PENDING) {
                    IconButton(onClick = { onDelete(mission.id) }) { Icon(Icons.Filled.Delete, "Delete", tint = Palette.Error) }
                }
            }
        }
    }
}

@Composable
private fun DialogSection(title: String) {
    Text(title.uppercase(), color = Palette.TextTertiary, style = MaterialTheme.typography.labelMedium)
}

@Composable
private fun SelectRow(title: String, subtitle: String?, selected: Boolean, onClick: () -> Unit) {
    GlassCard(modifier = Modifier.fillMaxWidth(), onClick = onClick) {
        Row(Modifier.padding(10.dp), verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f)) {
                Text(title, color = if (selected) Palette.AccentLight else Palette.TextPrimary, style = MaterialTheme.typography.bodyMedium)
                subtitle?.takeIf { it.isNotBlank() }?.let {
                    Text(it, color = Palette.TextTertiary, style = MaterialTheme.typography.bodySmall, maxLines = 1)
                }
            }
            if (selected) Text("Selected", color = Palette.Accent, style = MaterialTheme.typography.labelSmall)
        }
    }
}

private fun filteredProviders(providers: List<Provider>, backend: String): List<Provider> = when (backend) {
    "claudecode", "amp" -> providers.filter { it.id == "anthropic" }
    "codex" -> providers.filter { it.id == "openai" }
    "gemini" -> providers.filter { it.id == "google" }
    else -> providers
}

@Composable
private fun dialogChipColors() = FilterChipDefaults.filterChipColors(
    containerColor = Palette.Card,
    selectedContainerColor = Palette.Accent.copy(alpha = 0.18f),
    labelColor = Palette.TextSecondary,
    selectedLabelColor = Palette.Accent,
)

@Composable
private fun dialogFieldColors() = TextFieldDefaults.colors(
    focusedContainerColor = Palette.Card,
    unfocusedContainerColor = Palette.Card,
    focusedTextColor = Palette.TextPrimary,
    unfocusedTextColor = Palette.TextPrimary,
    cursorColor = Palette.Accent,
)

@Composable
private fun LinearLoading() {
    Box(Modifier.fillMaxWidth().padding(8.dp), contentAlignment = Alignment.Center) {
        CircularProgressIndicator(strokeWidth = 2.dp, modifier = Modifier.height(20.dp), color = Palette.Accent)
    }
}

@Composable
private fun SlashSuggestions(
    commands: List<SlashCommand>,
    loading: Boolean,
    onSelect: (SlashCommand) -> Unit,
) {
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .background(Palette.BackgroundSecondary)
            .padding(horizontal = 12.dp, vertical = 8.dp),
        verticalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        if (loading) {
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .background(Palette.Card, RoundedCornerShape(10.dp))
                    .border(1.dp, Palette.Border, RoundedCornerShape(10.dp))
                    .padding(horizontal = 12.dp, vertical = 10.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                CircularProgressIndicator(strokeWidth = 2.dp, modifier = Modifier.size(14.dp), color = Palette.Accent)
                Spacer(Modifier.width(8.dp))
                Text("Loading commands…", color = Palette.TextSecondary, style = MaterialTheme.typography.bodySmall)
            }
        } else {
            commands.take(8).forEach { command ->
                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .background(Palette.Card, RoundedCornerShape(10.dp))
                        .border(1.dp, Palette.Border, RoundedCornerShape(10.dp))
                        .clickable { onSelect(command) }
                        .padding(horizontal = 12.dp, vertical = 10.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Text("/${command.name}", color = Palette.AccentLight, style = MaterialTheme.typography.labelLarge, modifier = Modifier.widthIn(min = 92.dp))
                    Column(Modifier.weight(1f)) {
                        command.description?.takeIf { it.isNotBlank() }?.let {
                            Text(it, color = Palette.TextSecondary, style = MaterialTheme.typography.bodySmall, maxLines = 2)
                        }
                        val hint = slashCommandHint(command)
                        if (hint.isNotBlank()) {
                            Text(hint, color = Palette.TextTertiary, style = MaterialTheme.typography.labelSmall)
                        }
                    }
                }
            }
        }
    }
}

private fun isSlashPanelActive(draft: String): Boolean {
    val trimmed = draft.trim()
    if (!trimmed.startsWith("/")) return false
    return !trimmed.drop(1).any { it.isWhitespace() }
}

private fun visibleSlashSuggestions(
    draft: String,
    backend: String?,
    catalog: BuiltinCommandsResponse?,
): List<SlashCommand> {
    catalog ?: return emptyList()
    val trimmed = draft.trim()
    if (!trimmed.startsWith("/")) return emptyList()
    val fragment = trimmed.drop(1)
    if (fragment.any { it.isWhitespace() }) return emptyList()
    val pool = when (backend) {
        "codex" -> catalog.codex
        "claudecode" -> catalog.claudecode
        "opencode" -> catalog.opencode
        else -> catalog.opencode + catalog.claudecode + catalog.codex
    }
    return pool
        .filter { command ->
            fragment.isBlank() ||
                command.name.startsWith(fragment, ignoreCase = true)
        }
        .distinctBy { "${it.path}:${it.name}" }
}

private fun slashCommandHint(command: SlashCommand): String =
    command.params.joinToString(" ") { param ->
        if (param.required) "<${param.name}>" else "[${param.name}]"
    }

@Composable
private fun MessageRow(msg: ChatMessage) {
    when (val k = msg.kind) {
        ChatMessageKind.User -> Bubble(msg.content, mine = true)
        is ChatMessageKind.Assistant -> AssistantBubble(msg.content, k)
        is ChatMessageKind.Thinking -> SystemNote(if (k.done) "thinking complete" else "thinking…", muted = true, body = msg.content)
        is ChatMessageKind.Phase -> SystemNote("phase: ${k.phase}${k.detail?.let { " — $it" } ?: ""}")
        is ChatMessageKind.ToolCall -> ToolCallRow(k.name, k.isActive, msg.content)
        is ChatMessageKind.ToolUi -> ToolUiWidget(k.content)
        is ChatMessageKind.Goal -> SystemNote("goal · iter ${k.iteration} · ${k.status}", body = k.objective.takeIf { it.isNotBlank() })
        ChatMessageKind.SystemNote -> SystemNote(msg.content)
        ChatMessageKind.ErrorMsg -> ErrorBanner(msg.content)
    }
}

@Composable
private fun Bubble(text: String, mine: Boolean) {
    val bg = if (mine) Palette.Accent else Palette.Card
    val fg = if (mine) Color(0xFFFFFFFF) else Palette.TextPrimary
    Row(Modifier.fillMaxWidth(), horizontalArrangement = if (mine) Arrangement.End else Arrangement.Start) {
        Column(
            Modifier
                .widthIn(max = 320.dp)
                .background(bg, RoundedCornerShape(16.dp))
                .padding(horizontal = 12.dp, vertical = 10.dp),
        ) {
            Text(text, color = fg, style = MaterialTheme.typography.bodyMedium)
        }
    }
}

@Composable
private fun AssistantBubble(text: String, a: ChatMessageKind.Assistant) {
    Column(Modifier.fillMaxWidth()) {
        Bubble(text, mine = false)
        if (a.sharedFiles.isNotEmpty()) {
            Spacer(Modifier.height(6.dp))
            LazyRow(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                items(a.sharedFiles, key = { it.url }) { f -> SharedFileChip(f) }
            }
        }
        formatAssistantFooter(a)?.let {
            Spacer(Modifier.height(4.dp))
            Row(verticalAlignment = Alignment.CenterVertically) {
                Icon(costSourceIcon(a.costSource), null, tint = Palette.TextTertiary, modifier = Modifier.size(12.dp))
                Spacer(Modifier.width(4.dp))
                Text(it, color = Palette.TextTertiary, style = MaterialTheme.typography.bodySmall)
            }
        }
    }
}

private fun costSourceIcon(source: String): ImageVector = when (source) {
    "actual" -> Icons.Filled.PlayArrow
    "estimated" -> Icons.Filled.Schedule
    else -> Icons.Filled.PlayArrow
}

@Composable
private fun SharedFileChip(f: SharedFile) {
    val ctx = LocalContext.current
    Row(
        modifier = Modifier
            .background(Palette.Card, RoundedCornerShape(8.dp))
            .border(1.dp, Palette.Border, RoundedCornerShape(8.dp))
            .padding(horizontal = 8.dp, vertical = 6.dp)
            .clickable {
                val intent = android.content.Intent(android.content.Intent.ACTION_VIEW, f.url.toUri())
                runCatching { ctx.startActivity(intent) }
            },
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Icon(Icons.Filled.AttachFile, null, tint = Palette.AccentLight, modifier = Modifier.size(14.dp))
        Spacer(Modifier.width(6.dp))
        Text(f.name.ifBlank { "file" }, color = Palette.TextPrimary, style = MaterialTheme.typography.labelMedium)
        Spacer(Modifier.width(6.dp))
        Icon(Icons.AutoMirrored.Filled.OpenInNew, null, tint = Palette.TextTertiary, modifier = Modifier.size(12.dp))
    }
}

private fun formatAssistantFooter(a: ChatMessageKind.Assistant): String? {
    val parts = buildList<String> {
        a.model?.let { add(it) }
        if (a.costCents > 0) add("$" + "%.2f".format(a.costCents / 100.0))
        if (a.costSource == "estimated") add("est.")
    }
    return parts.takeIf { it.isNotEmpty() }?.joinToString(" • ")
}

@Composable
private fun SystemNote(label: String, body: String? = null, muted: Boolean = false) {
    GlassCard(modifier = Modifier.fillMaxWidth()) {
        Column(Modifier.padding(12.dp)) {
            Text(label, color = if (muted) Palette.TextTertiary else Palette.TextSecondary, style = MaterialTheme.typography.labelMedium)
            if (!body.isNullOrBlank()) {
                Spacer(Modifier.height(4.dp))
                Text(body, color = Palette.TextSecondary, style = MaterialTheme.typography.bodySmall)
            }
        }
    }
}

@Composable
private fun ToolCallRow(name: String, active: Boolean, args: String) {
    GlassCard(modifier = Modifier.fillMaxWidth()) {
        Column(Modifier.padding(12.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                if (active) CircularProgressIndicator(strokeWidth = 2.dp, modifier = Modifier.size(14.dp), color = Palette.Accent)
                if (active) Spacer(Modifier.width(8.dp))
                Text("tool: $name", color = Palette.AccentLight, style = MaterialTheme.typography.labelLarge)
            }
            if (args.isNotBlank()) {
                Spacer(Modifier.height(4.dp))
                Text(args.take(400), color = Palette.TextTertiary, style = TextStyle(fontFamily = FontFamily.Monospace, fontSize = 12.sp))
            }
        }
    }
}

@Composable
private fun Composer(value: String, onChange: (String) -> Unit, onSend: () -> Unit, onCancel: () -> Unit, isSending: Boolean) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .background(Palette.BackgroundSecondary)
            .padding(12.dp),
        verticalAlignment = Alignment.Bottom,
    ) {
        Box(
            Modifier
                .weight(1f)
                .heightIn(min = 44.dp)
                .background(Palette.Card, RoundedCornerShape(20.dp))
                .border(1.dp, Palette.Border, RoundedCornerShape(20.dp))
                .padding(horizontal = 14.dp, vertical = 10.dp),
        ) {
            BasicTextField(
                value = value,
                onValueChange = onChange,
                cursorBrush = SolidColor(Palette.Accent),
                textStyle = MaterialTheme.typography.bodyMedium.copy(color = Palette.TextPrimary),
                modifier = Modifier.fillMaxWidth(),
            )
            if (value.isEmpty()) {
                Text("Message…", color = Palette.TextMuted, style = MaterialTheme.typography.bodyMedium)
            }
        }
        Spacer(Modifier.width(8.dp))
        IconButton(onClick = if (isSending) onCancel else onSend, enabled = isSending || value.isNotBlank()) {
            Icon(
                if (isSending) Icons.Filled.Cancel else Icons.AutoMirrored.Filled.Send,
                contentDescription = if (isSending) "Cancel" else "Send",
                tint = if (isSending) Palette.Error else Palette.Accent,
            )
        }
    }
}

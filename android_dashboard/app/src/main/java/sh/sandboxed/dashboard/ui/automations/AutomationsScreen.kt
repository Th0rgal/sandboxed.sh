package sh.sandboxed.dashboard.ui.automations

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.AssistChip
import androidx.compose.material3.AssistChipDefaults
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.FilterChip
import androidx.compose.material3.FilterChipDefaults
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Switch
import androidx.compose.material3.SwitchDefaults
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TextFieldDefaults
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import sh.sandboxed.dashboard.data.AppContainer
import sh.sandboxed.dashboard.data.Automation
import sh.sandboxed.dashboard.data.AutomationCommandSource
import sh.sandboxed.dashboard.data.AutomationTrigger
import sh.sandboxed.dashboard.data.CreateAutomationRequest
import sh.sandboxed.dashboard.data.UpdateAutomationRequest
import sh.sandboxed.dashboard.ui.components.ErrorBanner
import sh.sandboxed.dashboard.ui.components.GlassCard
import sh.sandboxed.dashboard.ui.theme.Palette

private data class AutoState(
    val items: List<Automation> = emptyList(),
    val loading: Boolean = false,
    val error: String? = null,
)

private class AutomationsViewModel(private val container: AppContainer, private val missionId: String) : ViewModel() {
    private val _state = MutableStateFlow(AutoState())
    val state: StateFlow<AutoState> = _state.asStateFlow()
    init { refresh() }

    fun refresh() {
        _state.update { it.copy(loading = true, error = null) }
        viewModelScope.launch {
            runCatching { container.api.listAutomations(missionId) }
                .onSuccess { list -> _state.update { it.copy(items = list, loading = false) } }
                .onFailure { e -> _state.update { it.copy(error = e.message, loading = false) } }
        }
    }

    fun create(content: String, triggerSeconds: Int?, triggerKind: String) {
        val req = CreateAutomationRequest(
            commandSource = AutomationCommandSource(kind = "inline", content = content),
            trigger = AutomationTrigger(kind = triggerKind, seconds = triggerSeconds),
            active = true,
        )
        viewModelScope.launch {
            runCatching { container.api.createAutomation(missionId, req) }
                .onSuccess { refresh() }
                .onFailure { e -> _state.update { it.copy(error = e.message) } }
        }
    }

    fun toggle(a: Automation) {
        viewModelScope.launch {
            runCatching { container.api.updateAutomation(a.id, UpdateAutomationRequest(active = !a.active)) }
                .onSuccess { refresh() }
                .onFailure { e -> _state.update { it.copy(error = e.message) } }
        }
    }

    fun delete(a: Automation) {
        viewModelScope.launch {
            runCatching { container.api.deleteAutomation(a.id) }
                .onSuccess { refresh() }
                .onFailure { e -> _state.update { it.copy(error = e.message) } }
        }
    }
}

@Composable
fun AutomationsScreen(container: AppContainer, missionId: String, onBack: () -> Unit) {
    val vm = remember(missionId) { AutomationsViewModel(container, missionId) }
    val state by vm.state.collectAsState()
    var showCreate by remember { mutableStateOf(false) }

    Column(Modifier.fillMaxSize()) {
        Row(Modifier.fillMaxWidth().padding(horizontal = 8.dp, vertical = 8.dp), verticalAlignment = Alignment.CenterVertically) {
            IconButton(onClick = onBack) { Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back", tint = Palette.TextPrimary) }
            Text("Automations", style = MaterialTheme.typography.titleLarge, color = Palette.TextPrimary, modifier = Modifier.weight(1f))
            IconButton(onClick = { showCreate = true }) { Icon(Icons.Filled.Add, "Add", tint = Palette.Accent) }
        }
        state.error?.let { Box(Modifier.padding(horizontal = 16.dp, vertical = 8.dp)) { ErrorBanner(it) } }
        if (state.loading && state.items.isEmpty()) {
            Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) { CircularProgressIndicator(color = Palette.Accent) }
        } else {
            LazyColumn(contentPadding = PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                items(state.items, key = { it.id }) { a -> AutomationRow(a, { vm.toggle(a) }, { vm.delete(a) }) }
            }
        }
    }

    if (showCreate) {
        CreateAutomationDialog(
            onCancel = { showCreate = false },
            onCreate = { content, sec, kind -> vm.create(content, sec, kind); showCreate = false }
        )
    }
}

@Composable
private fun AutomationRow(a: Automation, onToggle: () -> Unit, onDelete: () -> Unit) {
    GlassCard(modifier = Modifier.fillMaxWidth()) {
        Column(Modifier.padding(12.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(triggerLabel(a.trigger), color = Palette.AccentLight, style = MaterialTheme.typography.labelMedium, modifier = Modifier.weight(1f))
                Switch(
                    checked = a.active,
                    onCheckedChange = { onToggle() },
                    colors = SwitchDefaults.colors(checkedThumbColor = Palette.Accent),
                )
            }
            a.commandSource.content?.let {
                Spacer(Modifier.height(4.dp))
                Text(it, color = Palette.TextPrimary, style = MaterialTheme.typography.bodyMedium, maxLines = 4)
            }
            Spacer(Modifier.height(8.dp))
            Row(verticalAlignment = Alignment.CenterVertically) {
                a.lastTriggeredAt?.let {
                    Text("last: " + it.take(19).replace('T', ' '), color = Palette.TextTertiary, style = MaterialTheme.typography.bodySmall)
                }
                Spacer(Modifier.weight(1f))
                IconButton(onClick = onDelete) { Icon(Icons.Filled.Delete, "Delete", tint = Palette.Error) }
            }
        }
    }
}

private fun triggerLabel(t: AutomationTrigger): String = when (t.kind) {
    "interval" -> "every ${t.seconds ?: 0}s"
    "agentFinished", "agent_finished" -> "on agent finish"
    "webhook" -> "on webhook"
    else -> t.kind
}

@Composable
private fun CreateAutomationDialog(onCancel: () -> Unit, onCreate: (String, Int?, String) -> Unit) {
    var content by remember { mutableStateOf("") }
    var triggerKind by remember { mutableStateOf("interval") }
    var seconds by remember { mutableStateOf("60") }
    AlertDialog(
        onDismissRequest = onCancel,
        title = { Text("New automation") },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                OutlinedTextField(
                    value = content, onValueChange = { content = it },
                    label = { Text("Command (sent to agent)") },
                    modifier = Modifier.fillMaxWidth(),
                    colors = autoFieldColors(), maxLines = 4,
                )
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    listOf("interval" to "Interval", "agent_finished" to "On finish", "webhook" to "Webhook").forEach { (k, label) ->
                        FilterChip(
                            selected = triggerKind == k, onClick = { triggerKind = k },
                            label = { Text(label, style = MaterialTheme.typography.labelSmall) },
                            colors = FilterChipDefaults.filterChipColors(
                                containerColor = Palette.Card,
                                selectedContainerColor = Palette.Accent.copy(alpha = 0.18f),
                                labelColor = Palette.TextSecondary,
                                selectedLabelColor = Palette.Accent,
                            ),
                        )
                    }
                }
                if (triggerKind == "interval") {
                    OutlinedTextField(
                        value = seconds, onValueChange = { seconds = it.filter { c -> c.isDigit() } },
                        label = { Text("Seconds") }, singleLine = true,
                        colors = autoFieldColors(),
                    )
                }
            }
        },
        confirmButton = {
            Button(
                onClick = { onCreate(content, seconds.toIntOrNull(), triggerKind) },
                enabled = content.isNotBlank() && (triggerKind != "interval" || (seconds.toIntOrNull() ?: 0) > 0),
                colors = ButtonDefaults.buttonColors(containerColor = Palette.Accent),
            ) { Text("Create") }
        },
        dismissButton = { TextButton(onClick = onCancel) { Text("Cancel") } },
        containerColor = Palette.Card,
    )
}

@Composable
private fun autoFieldColors() = TextFieldDefaults.colors(
    focusedContainerColor = Palette.Card,
    unfocusedContainerColor = Palette.Card,
    focusedTextColor = Palette.TextPrimary,
    unfocusedTextColor = Palette.TextPrimary,
    cursorColor = Palette.Accent,
)

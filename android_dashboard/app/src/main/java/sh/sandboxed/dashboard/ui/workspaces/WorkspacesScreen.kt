package sh.sandboxed.dashboard.ui.workspaces

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
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.FilterChip
import androidx.compose.material3.FilterChipDefaults
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
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
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import sh.sandboxed.dashboard.data.AppContainer
import sh.sandboxed.dashboard.data.CreateWorkspaceRequest
import sh.sandboxed.dashboard.data.Workspace
import sh.sandboxed.dashboard.ui.components.ErrorBanner
import sh.sandboxed.dashboard.ui.components.GlassCard
import sh.sandboxed.dashboard.ui.theme.Palette

private data class WorkspacesState(
    val items: List<Workspace> = emptyList(),
    val loading: Boolean = false,
    val error: String? = null,
)

private class WorkspacesViewModel(private val container: AppContainer) : ViewModel() {
    private val _state = MutableStateFlow(WorkspacesState())
    val state: StateFlow<WorkspacesState> = _state.asStateFlow()

    init { refresh() }

    fun refresh() {
        _state.update { it.copy(loading = true, error = null) }
        viewModelScope.launch {
            runCatching { container.api.listWorkspaces() }
                .onSuccess { list -> _state.update { it.copy(items = list, loading = false) } }
                .onFailure { e -> _state.update { it.copy(error = e.message, loading = false) } }
        }
    }

    fun create(name: String, type: String, path: String?) {
        viewModelScope.launch {
            _state.update { it.copy(loading = true, error = null) }
            runCatching {
                container.api.createWorkspace(
                    CreateWorkspaceRequest(
                        name = name.trim(),
                        workspaceType = type,
                        path = path?.trim()?.takeIf { it.isNotBlank() },
                    )
                )
            }
                .onSuccess { refresh() }
                .onFailure { e -> _state.update { it.copy(error = e.message, loading = false) } }
        }
    }
}

@Composable
fun WorkspacesScreen(container: AppContainer) {
    val vm = remember { WorkspacesViewModel(container) }
    val state by vm.state.collectAsState()
    var showCreate by remember { mutableStateOf(false) }

    Column(Modifier.fillMaxSize()) {
        Row(
            Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 12.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text("Workspaces", style = MaterialTheme.typography.headlineSmall, color = Palette.TextPrimary, modifier = Modifier.weight(1f))
            IconButton(onClick = { showCreate = true }) { Icon(Icons.Filled.Add, "Create workspace", tint = Palette.Accent) }
            IconButton(onClick = vm::refresh) { Icon(Icons.Filled.Refresh, "Refresh", tint = Palette.TextSecondary) }
        }
        state.error?.let { Box(Modifier.padding(horizontal = 16.dp, vertical = 8.dp)) { ErrorBanner(it) } }
        if (state.loading && state.items.isEmpty()) {
            Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) { CircularProgressIndicator(color = Palette.Accent) }
        } else if (state.items.isEmpty()) {
            Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                Text("No workspaces", color = Palette.TextTertiary, style = MaterialTheme.typography.bodyMedium)
            }
        } else {
            LazyColumn(contentPadding = PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                items(state.items, key = { it.id }) { workspace -> WorkspaceRow(workspace) }
            }
        }
    }

    if (showCreate) {
        CreateWorkspaceDialog(
            onCancel = { showCreate = false },
            onCreate = { name, type, path ->
                vm.create(name, type, path)
                showCreate = false
            },
        )
    }
}

@Composable
private fun WorkspaceRow(workspace: Workspace) {
    GlassCard(modifier = Modifier.fillMaxWidth()) {
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    workspace.name,
                    color = Palette.TextPrimary,
                    style = MaterialTheme.typography.titleSmall,
                    modifier = Modifier.weight(1f),
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
                Text(workspace.workspaceType, color = Palette.AccentLight, style = MaterialTheme.typography.labelMedium)
            }
            Text(workspace.status, color = statusColor(workspace.status), style = MaterialTheme.typography.bodySmall)
            Text(workspace.path, color = Palette.TextTertiary, style = MaterialTheme.typography.bodySmall, maxLines = 2, overflow = TextOverflow.Ellipsis)
            workspace.errorMessage?.takeIf { it.isNotBlank() }?.let {
                Text(it, color = Palette.Error, style = MaterialTheme.typography.bodySmall, maxLines = 3, overflow = TextOverflow.Ellipsis)
            }
        }
    }
}

@Composable
private fun CreateWorkspaceDialog(onCancel: () -> Unit, onCreate: (String, String, String?) -> Unit) {
    var name by remember { mutableStateOf("") }
    var type by remember { mutableStateOf("container") }
    var path by remember { mutableStateOf("") }
    val hostNeedsPath = type == "host" && path.isBlank()

    AlertDialog(
        onDismissRequest = onCancel,
        title = { Text("New workspace") },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                OutlinedTextField(
                    value = name,
                    onValueChange = { name = it },
                    label = { Text("Name") },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth(),
                    colors = fieldColors(),
                )
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    listOf("container" to "Container", "host" to "Host").forEach { (value, label) ->
                        FilterChip(
                            selected = type == value,
                            onClick = { type = value },
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
                if (type == "host") {
                    OutlinedTextField(
                        value = path,
                        onValueChange = { path = it },
                        label = { Text("Host path") },
                        singleLine = true,
                        modifier = Modifier.fillMaxWidth(),
                        colors = fieldColors(),
                    )
                }
                if (hostNeedsPath) {
                    Text("Host workspaces require a path.", color = Palette.Warning, style = MaterialTheme.typography.bodySmall)
                }
            }
        },
        confirmButton = {
            Button(
                onClick = { onCreate(name, type, path) },
                enabled = name.isNotBlank() && !hostNeedsPath,
                colors = ButtonDefaults.buttonColors(containerColor = Palette.Accent),
            ) { Text("Create") }
        },
        dismissButton = { TextButton(onClick = onCancel) { Text("Cancel") } },
        containerColor = Palette.Card,
    )
}

private fun statusColor(status: String) = when (status.lowercase()) {
    "ready", "running" -> Palette.Success
    "building", "creating" -> Palette.Info
    "error", "failed" -> Palette.Error
    else -> Palette.TextTertiary
}

@Composable
private fun fieldColors() = TextFieldDefaults.colors(
    focusedContainerColor = Palette.Card,
    unfocusedContainerColor = Palette.Card,
    focusedTextColor = Palette.TextPrimary,
    unfocusedTextColor = Palette.TextPrimary,
    cursorColor = Palette.Accent,
)

package sh.sandboxed.dashboard.ui.tasks

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
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import sh.sandboxed.dashboard.data.AppContainer
import sh.sandboxed.dashboard.data.TaskState
import sh.sandboxed.dashboard.data.TaskStatus
import sh.sandboxed.dashboard.ui.components.ErrorBanner
import sh.sandboxed.dashboard.ui.components.GlassCard
import sh.sandboxed.dashboard.ui.theme.Palette
import sh.sandboxed.dashboard.util.boundedForText

private data class TasksState(val items: List<TaskState> = emptyList(), val loading: Boolean = false, val error: String? = null)

private class TasksViewModel(private val container: AppContainer) : ViewModel() {
    private val _state = MutableStateFlow(TasksState())
    val state: StateFlow<TasksState> = _state.asStateFlow()
    init { refresh() }
    fun refresh() {
        _state.update { it.copy(loading = true, error = null) }
        viewModelScope.launch {
            runCatching { container.api.listTasks() }
                .onSuccess { list -> _state.update { it.copy(items = list, loading = false) } }
                .onFailure { e -> _state.update { it.copy(error = e.message, loading = false) } }
        }
    }
}

@Composable
fun TasksScreen(container: AppContainer) {
    val vm = remember { TasksViewModel(container) }
    val state by vm.state.collectAsState()
    Column(Modifier.fillMaxSize()) {
        Row(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 12.dp), verticalAlignment = Alignment.CenterVertically) {
            Text("Tasks", style = MaterialTheme.typography.headlineSmall, color = Palette.TextPrimary, modifier = Modifier.weight(1f))
            IconButton(onClick = vm::refresh) { Icon(Icons.Filled.Refresh, "Refresh", tint = Palette.TextSecondary) }
        }
        state.error?.let { Box(Modifier.padding(horizontal = 16.dp, vertical = 8.dp)) { ErrorBanner(it) } }
        if (state.loading && state.items.isEmpty()) {
            Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) { CircularProgressIndicator(color = Palette.Accent) }
        } else if (state.items.isEmpty()) {
            EmptyState("No subtasks running")
        } else {
            LazyColumn(contentPadding = PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                items(state.items, key = { it.id }) { t -> TaskRow(t) }
            }
        }
    }
}

@Composable
private fun TaskRow(t: TaskState) {
    GlassCard(modifier = Modifier.fillMaxWidth()) {
        Column(Modifier.padding(12.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(t.task.ifBlank { t.id.take(8) }, color = Palette.TextPrimary, style = MaterialTheme.typography.titleSmall, modifier = Modifier.weight(1f))
                Text(t.status.name.lowercase(), color = statusColor(t.status), style = MaterialTheme.typography.labelMedium)
            }
            Spacer(Modifier.height(4.dp))
            Text(t.model, color = Palette.TextTertiary, style = MaterialTheme.typography.bodySmall)
            if (t.iterations > 0) Text("iterations: ${t.iterations}", color = Palette.TextTertiary, style = MaterialTheme.typography.bodySmall)
            t.result?.takeIf { it.isNotBlank() }?.let {
                Spacer(Modifier.height(4.dp))
                Text(it.boundedForText(), color = Palette.TextSecondary, style = MaterialTheme.typography.bodySmall, maxLines = 4)
            }
        }
    }
}

private fun statusColor(s: TaskStatus): Color = when (s) {
    TaskStatus.PENDING -> Palette.TextTertiary
    TaskStatus.RUNNING -> Palette.Info
    TaskStatus.COMPLETED -> Palette.Success
    TaskStatus.FAILED -> Palette.Error
    TaskStatus.CANCELLED -> Palette.Warning
}

@Composable
fun EmptyState(text: String) {
    Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
        Text(text, color = Palette.TextTertiary, style = MaterialTheme.typography.bodyMedium)
    }
}

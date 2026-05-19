package sh.sandboxed.dashboard.ui.runs

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
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import sh.sandboxed.dashboard.data.AppContainer
import sh.sandboxed.dashboard.data.Run
import sh.sandboxed.dashboard.ui.components.ErrorBanner
import sh.sandboxed.dashboard.ui.components.GlassCard
import sh.sandboxed.dashboard.ui.tasks.EmptyState
import sh.sandboxed.dashboard.ui.theme.Palette
import sh.sandboxed.dashboard.util.boundedForText

private data class RunsState(val items: List<Run> = emptyList(), val loading: Boolean = false, val error: String? = null)

private class RunsViewModel(private val container: AppContainer) : ViewModel() {
    private val _state = MutableStateFlow(RunsState())
    val state: StateFlow<RunsState> = _state.asStateFlow()
    init { refresh() }
    fun refresh() {
        _state.update { it.copy(loading = true, error = null) }
        viewModelScope.launch {
            runCatching { container.api.listRuns() }
                .onSuccess { resp -> _state.update { it.copy(items = resp.runs, loading = false) } }
                .onFailure { e -> _state.update { it.copy(error = e.message, loading = false) } }
        }
    }
}

@Composable
fun RunsScreen(container: AppContainer) {
    val vm = remember { RunsViewModel(container) }
    val state by vm.state.collectAsState()
    Column(Modifier.fillMaxSize()) {
        Row(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 12.dp), verticalAlignment = Alignment.CenterVertically) {
            Text("Runs", style = MaterialTheme.typography.headlineSmall, color = Palette.TextPrimary, modifier = Modifier.weight(1f))
            val total = state.items.sumOf { it.totalCostCents } / 100.0
            if (total > 0) Text("$" + "%.2f".format(total), color = Palette.AccentLight, style = MaterialTheme.typography.titleSmall)
            IconButton(onClick = vm::refresh) { Icon(Icons.Filled.Refresh, "Refresh", tint = Palette.TextSecondary) }
        }
        state.error?.let { Box(Modifier.padding(horizontal = 16.dp, vertical = 8.dp)) { ErrorBanner(it) } }
        if (state.loading && state.items.isEmpty()) {
            Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) { CircularProgressIndicator(color = Palette.Accent) }
        } else if (state.items.isEmpty()) {
            EmptyState("No runs recorded")
        } else {
            LazyColumn(contentPadding = PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                items(state.items, key = { it.id }) { r -> RunRow(r) }
            }
        }
    }
}

@Composable
private fun RunRow(r: Run) {
    GlassCard(modifier = Modifier.fillMaxWidth()) {
        Column(Modifier.padding(12.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(r.id.take(8), color = Palette.TextSecondary, style = MaterialTheme.typography.labelMedium, modifier = Modifier.weight(1f))
                Text(r.status, color = Palette.TextTertiary, style = MaterialTheme.typography.labelMedium)
                Spacer(Modifier.height(0.dp))
                Text("  $" + "%.2f".format(r.costDollars), color = Palette.AccentLight, style = MaterialTheme.typography.labelMedium)
            }
            Spacer(Modifier.height(4.dp))
            Text(r.inputText.boundedForText(maxChars = 1_000), color = Palette.TextPrimary, style = MaterialTheme.typography.bodyMedium, maxLines = 2)
            r.summaryText?.takeIf { it.isNotBlank() }?.let {
                Spacer(Modifier.height(4.dp))
                Text(it.boundedForText(maxChars = 1_500), color = Palette.TextSecondary, style = MaterialTheme.typography.bodySmall, maxLines = 3)
            }
            if (r.createdAt.isNotBlank()) {
                Spacer(Modifier.height(4.dp))
                Text(r.createdAt.take(19).replace('T', ' '), color = Palette.TextTertiary, style = MaterialTheme.typography.bodySmall)
            }
        }
    }
}

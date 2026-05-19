package sh.sandboxed.dashboard.ui.history

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
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.AutoAwesome
import androidx.compose.material.icons.filled.Cancel
import androidx.compose.material.icons.filled.CleaningServices
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material.icons.filled.Search
import androidx.compose.material3.AssistChip
import androidx.compose.material3.AssistChipDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.FilterChip
import androidx.compose.material3.FilterChipDefaults
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextFieldDefaults
import androidx.compose.material3.pulltorefresh.PullToRefreshBox
import androidx.compose.material3.pulltorefresh.rememberPullToRefreshState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
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
import sh.sandboxed.dashboard.data.Mission
import sh.sandboxed.dashboard.data.MissionMomentSearchResult
import sh.sandboxed.dashboard.data.MissionStatus
import sh.sandboxed.dashboard.ui.components.ErrorBanner
import sh.sandboxed.dashboard.ui.components.GlassCard
import sh.sandboxed.dashboard.ui.components.StatusBadge
import sh.sandboxed.dashboard.ui.theme.Palette
import sh.sandboxed.dashboard.util.Haptics
import sh.sandboxed.dashboard.util.boundedForText

private enum class HistoryFilter { ALL, ACTIVE, INTERRUPTED, COMPLETED, FAILED }

private data class HistoryState(
    val missions: List<Mission> = emptyList(),
    val filter: HistoryFilter = HistoryFilter.ALL,
    val loading: Boolean = false,
    val refreshing: Boolean = false,
    val error: String? = null,
    val query: String = "",
    val searching: Boolean = false,
    val searchHits: List<Mission> = emptyList(),
    val moments: List<MissionMomentSearchResult> = emptyList(),
)

private class HistoryViewModel(private val container: AppContainer) : ViewModel() {
    private val _state = MutableStateFlow(HistoryState())
    val state: StateFlow<HistoryState> = _state.asStateFlow()

    init { refresh() }

    fun setFilter(f: HistoryFilter) { _state.update { it.copy(filter = f) } }

    fun refresh(pullToRefresh: Boolean = false) {
        _state.update { it.copy(loading = !pullToRefresh, refreshing = pullToRefresh, error = null) }
        viewModelScope.launch {
            runCatching { container.api.listMissions() }
                .onSuccess { list -> _state.update { it.copy(missions = list, loading = false, refreshing = false) } }
                .onFailure { e -> _state.update { it.copy(error = e.message, loading = false, refreshing = false) } }
        }
    }

    fun setQuery(q: String) {
        _state.update { it.copy(query = q) }
        if (q.isBlank()) {
            _state.update { it.copy(searchHits = emptyList(), moments = emptyList()) }
            return
        }
        viewModelScope.launch {
            _state.update { it.copy(searching = true) }
            val missions = runCatching { container.api.searchMissions(q) }.getOrNull().orEmpty().map { it.mission }
            val moments = runCatching { container.api.searchMoments(q) }.getOrNull().orEmpty()
            _state.update { it.copy(searchHits = missions, moments = moments, searching = false) }
        }
    }

    fun resume(id: String) {
        viewModelScope.launch { runCatching { container.api.resumeMission(id) }; refresh() }
    }

    fun cancel(id: String) {
        viewModelScope.launch { runCatching { container.api.cancelMission(id) }; refresh() }
    }

    fun delete(id: String) {
        viewModelScope.launch { runCatching { container.api.deleteMission(id) }; refresh() }
    }

    fun cleanup() {
        viewModelScope.launch {
            val deleted = runCatching { container.api.cleanupMissions() }.getOrNull() ?: 0
            _state.update { it.copy(error = if (deleted > 0) null else "Nothing to clean") }
            refresh()
        }
    }
}

private fun HistoryFilter.matches(m: Mission): Boolean = when (this) {
    HistoryFilter.ALL -> true
    HistoryFilter.ACTIVE -> m.status == MissionStatus.ACTIVE || m.status == MissionStatus.PENDING
    HistoryFilter.INTERRUPTED -> m.status == MissionStatus.INTERRUPTED || m.status == MissionStatus.BLOCKED
    HistoryFilter.COMPLETED -> m.status == MissionStatus.COMPLETED
    HistoryFilter.FAILED -> m.status == MissionStatus.FAILED || m.status == MissionStatus.NOT_FEASIBLE
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun HistoryScreen(container: AppContainer, onOpen: (String) -> Unit) {
    val vm = remember { HistoryViewModel(container) }
    val state by vm.state.collectAsState()
    val haptics = remember { Haptics(container) }
    val pullState = rememberPullToRefreshState()

    Column(Modifier.fillMaxSize()) {
        Row(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 12.dp), verticalAlignment = Alignment.CenterVertically) {
            Text("Missions", style = MaterialTheme.typography.headlineSmall, color = Palette.TextPrimary, modifier = Modifier.weight(1f))
            IconButton(onClick = { haptics.light(); vm.cleanup() }) { Icon(Icons.Filled.CleaningServices, "Cleanup completed", tint = Palette.Warning) }
            IconButton(onClick = { haptics.light(); vm.refresh() }) { Icon(Icons.Filled.Refresh, "Refresh", tint = Palette.TextSecondary) }
        }
        OutlinedTextField(
            value = state.query, onValueChange = { vm.setQuery(it) },
            singleLine = true, modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp),
            placeholder = { Text("Search missions and moments…", color = Palette.TextMuted) },
            leadingIcon = { Icon(Icons.Filled.Search, null, tint = Palette.TextTertiary) },
            colors = TextFieldDefaults.colors(focusedContainerColor = Palette.Card, unfocusedContainerColor = Palette.Card, cursorColor = Palette.Accent, focusedTextColor = Palette.TextPrimary, unfocusedTextColor = Palette.TextPrimary),
        )
        if (state.query.isBlank()) {
            LazyRow(
                contentPadding = PaddingValues(horizontal = 16.dp, vertical = 8.dp),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
                modifier = Modifier.fillMaxWidth(),
            ) {
                items(HistoryFilter.entries.toList()) { f ->
                    FilterChip(
                        selected = state.filter == f,
                        onClick = { vm.setFilter(f) },
                        label = { Text(f.name.lowercase().replaceFirstChar { it.uppercase() }) },
                        colors = FilterChipDefaults.filterChipColors(
                            containerColor = Palette.Card,
                            selectedContainerColor = Palette.Accent.copy(alpha = 0.18f),
                            labelColor = Palette.TextSecondary,
                            selectedLabelColor = Palette.Accent,
                        ),
                    )
                }
            }
        }
        state.error?.let { Box(Modifier.padding(horizontal = 16.dp, vertical = 8.dp)) { ErrorBanner(it) } }

        if (state.loading && state.missions.isEmpty()) {
            Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) { CircularProgressIndicator(color = Palette.Accent) }
        } else {
            PullToRefreshBox(
                isRefreshing = state.refreshing,
                state = pullState,
                onRefresh = { vm.refresh(pullToRefresh = true) },
                modifier = Modifier.fillMaxSize(),
            ) {
                if (state.query.isNotBlank()) {
                    LazyColumn(contentPadding = PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp), modifier = Modifier.fillMaxSize()) {
                        if (state.searching) item { LinearLoading() }
                        if (state.searchHits.isNotEmpty()) item { Text("Missions", color = Palette.TextSecondary, style = MaterialTheme.typography.titleSmall) }
                        items(state.searchHits, key = { it.id }) { m ->
                            MissionRow(m, { onOpen(m.id) }, { vm.resume(m.id) }, { vm.cancel(m.id) }, { vm.delete(m.id) })
                        }
                        if (state.moments.isNotEmpty()) item { Text("Moments", color = Palette.TextSecondary, style = MaterialTheme.typography.titleSmall, modifier = Modifier.padding(top = 8.dp)) }
                        items(state.moments) { mm -> MomentRow(mm) { onOpen(mm.mission.id) } }
                    }
                } else {
                    val items = state.missions.filter { state.filter.matches(it) }
                    LazyColumn(contentPadding = PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp), modifier = Modifier.fillMaxSize()) {
                        items(items, key = { it.id }) { m ->
                            MissionRow(m, { onOpen(m.id) }, { vm.resume(m.id) }, { vm.cancel(m.id) }, { vm.delete(m.id) })
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun LinearLoading() {
    Box(Modifier.fillMaxWidth().padding(8.dp), contentAlignment = Alignment.Center) {
        CircularProgressIndicator(strokeWidth = 2.dp, modifier = Modifier.height(20.dp), color = Palette.Accent)
    }
}

@Composable
private fun MissionRow(mission: Mission, onOpen: () -> Unit, onResume: () -> Unit, onCancel: () -> Unit, onDelete: () -> Unit) {
    GlassCard(modifier = Modifier.fillMaxWidth(), onClick = onOpen) {
        Column(Modifier.padding(12.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    mission.title ?: mission.shortDescription ?: mission.id.take(8),
                    style = MaterialTheme.typography.titleSmall,
                    color = Palette.TextPrimary,
                    modifier = Modifier.weight(1f),
                )
                StatusBadge(mission.status)
            }
            mission.modelOverride?.let {
                Spacer(Modifier.height(4.dp))
                AssistChip(onClick = {}, label = { Text(it, style = MaterialTheme.typography.labelSmall) },
                    colors = AssistChipDefaults.assistChipColors(containerColor = Palette.BackgroundTertiary, labelColor = Palette.TextSecondary))
            }
            Spacer(Modifier.height(8.dp))
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(mission.updatedAt.take(19).replace('T', ' '), color = Palette.TextTertiary, style = MaterialTheme.typography.bodySmall)
                Spacer(Modifier.weight(1f))
                if (mission.status.canResume || mission.resumable) {
                    IconButton(onClick = onResume) { Icon(Icons.Filled.PlayArrow, "Resume", tint = Palette.Success) }
                }
                if (mission.status == MissionStatus.ACTIVE || mission.status == MissionStatus.PENDING) {
                    IconButton(onClick = onCancel) { Icon(Icons.Filled.Cancel, "Cancel", tint = Palette.Warning) }
                }
                IconButton(onClick = onDelete) { Icon(Icons.Filled.Delete, "Delete", tint = Palette.Error) }
            }
        }
    }
}

@Composable
private fun MomentRow(m: MissionMomentSearchResult, onOpen: () -> Unit) {
    GlassCard(modifier = Modifier.fillMaxWidth(), onClick = onOpen) {
        Column(Modifier.padding(12.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Icon(Icons.Filled.AutoAwesome, null, tint = Palette.AccentLight)
                Spacer(Modifier.height(0.dp))
                Text(
                    "  " + (m.mission.title ?: m.mission.id.take(8)) + "  ·  " + m.role,
                    color = Palette.TextSecondary,
                    style = MaterialTheme.typography.labelMedium,
                )
            }
            Spacer(Modifier.height(4.dp))
            Text(m.snippet.boundedForText(maxChars = 1_500), color = Palette.TextPrimary, style = MaterialTheme.typography.bodyMedium, maxLines = 3)
            if (m.rationale.isNotBlank()) {
                Spacer(Modifier.height(4.dp))
                Text(m.rationale.boundedForText(maxChars = 1_000), color = Palette.TextTertiary, style = MaterialTheme.typography.bodySmall, maxLines = 2)
            }
        }
    }
}

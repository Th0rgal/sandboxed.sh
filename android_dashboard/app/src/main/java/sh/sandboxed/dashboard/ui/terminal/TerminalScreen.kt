package sh.sandboxed.dashboard.ui.terminal

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.KeyboardReturn
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalWindowInfo
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import sh.sandboxed.dashboard.data.AppContainer
import sh.sandboxed.dashboard.data.Workspace
import sh.sandboxed.dashboard.data.api.TerminalEvent
import sh.sandboxed.dashboard.ui.theme.Palette
import sh.sandboxed.dashboard.util.Ansi

private data class TerminalState(
    val lines: List<String> = listOf("Connecting…"),
    val draft: String = "",
    val connected: Boolean = false,
    val workspaces: List<Workspace> = emptyList(),
    val selectedWorkspaceId: String? = null,
)

private class TerminalViewModel(private val container: AppContainer) : ViewModel() {
    private val _state = MutableStateFlow(TerminalState())
    val state: StateFlow<TerminalState> = _state.asStateFlow()
    private var job: Job? = null

    init {
        viewModelScope.launch {
            runCatching { container.api.listWorkspaces() }.onSuccess { ws -> _state.update { it.copy(workspaces = ws) } }
        }
    }

    fun selectWorkspace(id: String?) {
        _state.update { it.copy(selectedWorkspaceId = id, lines = listOf("Connecting…")) }
        connect()
    }

    fun connect() {
        job?.cancel()
        job = viewModelScope.launch {
            var attempt = 0
            while (true) {
                try {
                    container.terminal.connect(_state.value.selectedWorkspaceId).collect { evt ->
                        attempt = 0
                        when (evt) {
                            is TerminalEvent.Connected -> _state.update { it.copy(connected = true, lines = it.lines + "[connected]") }
                            is TerminalEvent.Output -> {
                                val chunks = evt.text.split('\n')
                                _state.update { st ->
                                    val merged = st.lines.toMutableList()
                                    if (chunks.isNotEmpty()) {
                                        if (merged.isEmpty()) merged += chunks.first()
                                        else merged[merged.lastIndex] = merged.last() + chunks.first()
                                        merged.addAll(chunks.drop(1))
                                    }
                                    st.copy(lines = merged.takeLast(2000))
                                }
                            }
                            is TerminalEvent.Closed -> _state.update { it.copy(connected = false, lines = it.lines + "[disconnected: ${evt.reason ?: "closed"}]") }
                            is TerminalEvent.Failure -> _state.update { it.copy(connected = false, lines = it.lines + "[error: ${evt.error.message ?: ""}]") }
                        }
                    }
                } catch (_: Throwable) {}
                attempt += 1
                delay((1000L shl minOf(attempt, 5)).coerceAtMost(30_000L))
                _state.update { it.copy(lines = it.lines + "[reconnecting…]") }
            }
        }
    }

    fun close() { job?.cancel(); job = null }

    fun setDraft(t: String) { _state.update { it.copy(draft = t) } }

    fun submit() {
        val cmd = _state.value.draft
        container.terminal.sendInput("$cmd\n")
        _state.update { it.copy(draft = "") }
    }

    fun resize(cols: Int, rows: Int) { container.terminal.sendResize(cols, rows) }
}

@Composable
fun TerminalScreen(container: AppContainer) {
    val vm = remember { TerminalViewModel(container) }
    val state by vm.state.collectAsState()
    val listState = rememberLazyListState()
    val window = LocalWindowInfo.current
    val density = LocalDensity.current

    DisposableEffect(Unit) {
        vm.connect()
        onDispose { vm.close() }
    }
    LaunchedEffect(state.lines.size) {
        if (state.lines.isNotEmpty()) listState.animateScrollToItem(state.lines.lastIndex)
    }
    LaunchedEffect(window.containerSize, state.connected) {
        if (!state.connected) return@LaunchedEffect
        val charW = with(density) { 7.5.sp.toPx() }
        val charH = with(density) { 16.sp.toPx() }
        val cols = (window.containerSize.width / charW).toInt().coerceAtLeast(40)
        val rows = (window.containerSize.height / charH).toInt().coerceAtLeast(12)
        vm.resize(cols, rows)
    }

    var menu by remember { mutableStateOf(false) }
    val activeWorkspace = state.workspaces.firstOrNull { it.id == state.selectedWorkspaceId }

    Column(Modifier.fillMaxSize().background(Palette.TerminalBackground)) {
        Row(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 12.dp), verticalAlignment = Alignment.CenterVertically) {
            Text("Terminal", style = MaterialTheme.typography.titleMedium, color = Palette.TextPrimary)
            Spacer(Modifier.width(8.dp))
            Box {
                TextButton(onClick = { menu = true }) {
                    Text(activeWorkspace?.name ?: "default", color = Palette.AccentLight, style = MaterialTheme.typography.labelMedium)
                }
                DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
                    DropdownMenuItem(text = { Text("default (host)") }, onClick = { menu = false; vm.selectWorkspace(null) })
                    state.workspaces.forEach { w ->
                        DropdownMenuItem(text = { Text(w.name + " (" + w.workspaceType + ")") }, onClick = { menu = false; vm.selectWorkspace(w.id) })
                    }
                }
            }
            Spacer(Modifier.weight(1f))
            Text(
                if (state.connected) "● connected" else "○ offline",
                color = if (state.connected) Palette.Success else Palette.Warning,
                style = MaterialTheme.typography.labelMedium,
            )
        }
        LazyColumn(
            state = listState,
            modifier = Modifier.weight(1f).fillMaxWidth().padding(horizontal = 12.dp),
        ) {
            items(state.lines) { line ->
                val styled: AnnotatedString = remember(line) { Ansi.parse(line, Palette.TextPrimary) }
                Text(
                    styled,
                    style = TextStyle(fontFamily = FontFamily.Monospace, fontSize = 12.sp, lineHeight = 16.sp),
                )
            }
        }
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(12.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Box(
                Modifier
                    .weight(1f)
                    .heightIn(min = 40.dp)
                    .background(Palette.Card, RoundedCornerShape(8.dp))
                    .border(1.dp, Palette.Border, RoundedCornerShape(8.dp))
                    .padding(horizontal = 10.dp, vertical = 8.dp),
            ) {
                BasicTextField(
                    value = state.draft,
                    onValueChange = vm::setDraft,
                    cursorBrush = SolidColor(Palette.Accent),
                    textStyle = TextStyle(fontFamily = FontFamily.Monospace, color = Palette.TextPrimary, fontSize = 13.sp),
                    modifier = Modifier.fillMaxWidth(),
                    singleLine = true,
                )
                if (state.draft.isEmpty()) {
                    Text("$ ", color = Palette.TextMuted, style = TextStyle(fontFamily = FontFamily.Monospace, fontSize = 13.sp))
                }
            }
            IconButton(onClick = vm::submit) { Icon(Icons.AutoMirrored.Filled.KeyboardReturn, "Send", tint = Palette.Accent) }
        }
    }
}

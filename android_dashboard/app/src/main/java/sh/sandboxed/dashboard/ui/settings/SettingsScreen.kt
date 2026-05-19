package sh.sandboxed.dashboard.ui.settings

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Switch
import androidx.compose.material3.SwitchDefaults
import androidx.compose.material3.Text
import androidx.compose.material3.TextFieldDefaults
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.launch
import sh.sandboxed.dashboard.data.AppContainer
import sh.sandboxed.dashboard.data.Backend
import sh.sandboxed.dashboard.data.BackendAgent
import sh.sandboxed.dashboard.data.BuiltinCommandsResponse
import sh.sandboxed.dashboard.data.Provider
import sh.sandboxed.dashboard.data.SlashCommand
import sh.sandboxed.dashboard.ui.components.GlassCard
import sh.sandboxed.dashboard.ui.theme.Palette

@Composable
fun SettingsScreen(container: AppContainer) {
    val scope = rememberCoroutineScope()
    val settings by container.cached.collectAsState()

    var url by remember(settings.baseUrl) { mutableStateOf(settings.baseUrl) }
    var skip by remember(settings.skipAgentSelection) { mutableStateOf(settings.skipAgentSelection) }
    var status by remember { mutableStateOf<String?>(null) }
    var backends by remember { mutableStateOf<List<Backend>>(emptyList()) }
    var agents by remember { mutableStateOf<List<BackendAgent>>(emptyList()) }
    var providers by remember { mutableStateOf<List<Provider>>(emptyList()) }
    var commands by remember { mutableStateOf<BuiltinCommandsResponse?>(null) }
    var selectedBackend by remember(settings.defaultBackend) { mutableStateOf(settings.defaultBackend) }
    var selectedAgent by remember(settings.defaultAgent) { mutableStateOf(settings.defaultAgent) }

    LaunchedEffect(settings.baseUrl, settings.jwtToken) {
        if (settings.isConfigured && settings.jwtToken != null) {
            runCatching { container.api.listBackends() }.onSuccess { backends = it }
            runCatching { container.api.listProviders() }.onSuccess { providers = it.providers }
            runCatching { container.api.listBuiltinCommands() }.onSuccess { commands = it }
        }
    }
    LaunchedEffect(selectedBackend) {
        if (selectedBackend.isNotBlank()) {
            runCatching { container.api.listBackendAgents(selectedBackend) }.onSuccess { agents = it }
        } else agents = emptyList()
    }

    LazyColumn(
        modifier = Modifier.fillMaxSize(),
        contentPadding = PaddingValues(16.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        item { Text("Settings", style = MaterialTheme.typography.headlineSmall, color = Palette.TextPrimary) }

        item {
            GlassCard(modifier = Modifier.fillMaxWidth()) {
                Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text("Server", style = MaterialTheme.typography.titleSmall, color = Palette.TextSecondary)
                    OutlinedTextField(
                        value = url, onValueChange = { url = it },
                        label = { Text("Base URL") }, singleLine = true,
                        modifier = Modifier.fillMaxWidth(),
                        colors = fieldColors(),
                    )
                    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        Button(
                            onClick = {
                                scope.launch {
                                    container.settings.setBaseUrl(url.trim())
                                    status = runCatching { container.api.health() }
                                        .map { "Connected (${it.authMode ?: "ok"})" }
                                        .getOrElse { "Failed: ${it.message}" }
                                }
                            },
                            colors = ButtonDefaults.buttonColors(containerColor = Palette.Accent),
                        ) { Text("Test & save") }
                        OutlinedButton(onClick = {
                            scope.launch { container.settings.setToken(null) }
                        }) { Text("Sign out") }
                    }
                    status?.let { Text(it, color = Palette.TextTertiary, style = MaterialTheme.typography.bodySmall) }
                }
            }
        }

        item {
            GlassCard(modifier = Modifier.fillMaxWidth()) {
                Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text("Defaults", style = MaterialTheme.typography.titleSmall, color = Palette.TextSecondary)
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Text("Skip agent picker", color = Palette.TextPrimary, modifier = Modifier.weight(1f))
                        Switch(
                            checked = skip,
                            onCheckedChange = {
                                skip = it
                                scope.launch { container.settings.setSkipAgentSelection(it) }
                            },
                            colors = SwitchDefaults.colors(checkedThumbColor = Palette.Accent),
                        )
                    }
                    HorizontalDivider(color = Palette.Border)
                    Text("Backend", color = Palette.TextSecondary, style = MaterialTheme.typography.labelMedium)
                    backends.forEach { b ->
                        BackendRow(b.name, selectedBackend == b.id) {
                            selectedBackend = b.id
                            scope.launch { container.settings.setDefaultBackend(b.id) }
                        }
                    }
                    if (agents.isNotEmpty()) {
                        HorizontalDivider(color = Palette.Border)
                        Text("Agent", color = Palette.TextSecondary, style = MaterialTheme.typography.labelMedium)
                        agents.forEach { a ->
                            BackendRow(a.name, selectedAgent == a.id) {
                                selectedAgent = a.id
                                scope.launch { container.settings.setDefaultAgent(a.id) }
                            }
                        }
                    }
                }
            }
        }

        if (providers.isNotEmpty()) item {
            GlassCard(modifier = Modifier.fillMaxWidth()) {
                Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text("Providers", style = MaterialTheme.typography.titleSmall, color = Palette.TextSecondary)
                    providers.forEach { p ->
                        Column(Modifier.fillMaxWidth().padding(vertical = 4.dp)) {
                            Row {
                                Text(p.name, color = Palette.TextPrimary, style = MaterialTheme.typography.bodyMedium, modifier = Modifier.weight(1f))
                                Text(p.billing, color = Palette.AccentLight, style = MaterialTheme.typography.labelSmall)
                            }
                            if (p.description.isNotBlank()) Text(p.description, color = Palette.TextTertiary, style = MaterialTheme.typography.bodySmall)
                            if (p.models.isNotEmpty()) Text("${p.models.size} models", color = Palette.TextTertiary, style = MaterialTheme.typography.labelSmall)
                        }
                    }
                }
            }
        }

        commands?.let { cmds ->
            val groups = listOfNotNull(
                "claudecode".takeIf { cmds.claudecode.isNotEmpty() }?.let { it to cmds.claudecode },
                "opencode".takeIf { cmds.opencode.isNotEmpty() }?.let { it to cmds.opencode },
                "codex".takeIf { cmds.codex.isNotEmpty() }?.let { it to cmds.codex },
            )
            if (groups.isNotEmpty()) item {
                GlassCard(modifier = Modifier.fillMaxWidth()) {
                    Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                        Text("Slash commands", style = MaterialTheme.typography.titleSmall, color = Palette.TextSecondary)
                        groups.forEach { (backend, list) ->
                            Text(backend, color = Palette.AccentLight, style = MaterialTheme.typography.labelMedium)
                            list.forEach { sc -> SlashCommandRow(sc) }
                            Spacer(Modifier.height(4.dp))
                        }
                    }
                }
            }
        }

        item {
            GlassCard(modifier = Modifier.fillMaxWidth()) {
                Column(Modifier.padding(16.dp)) {
                    Text("About", style = MaterialTheme.typography.titleSmall, color = Palette.TextSecondary)
                    Spacer(Modifier.height(4.dp))
                    Text("Sandboxed.sh Android Dashboard", color = Palette.TextPrimary)
                    Text("v0.2.0", color = Palette.TextTertiary, style = MaterialTheme.typography.bodySmall)
                }
            }
        }
    }
}

@Composable
private fun SlashCommandRow(sc: SlashCommand) {
    Column(Modifier.fillMaxWidth().padding(vertical = 2.dp)) {
        Row {
            Text("/" + sc.name, color = Palette.TextPrimary, style = MaterialTheme.typography.labelLarge, modifier = Modifier.weight(1f))
            if (sc.params.any { it.required }) Text("required args", color = Palette.Warning, style = MaterialTheme.typography.labelSmall)
        }
        sc.description?.takeIf { it.isNotBlank() }?.let { Text(it, color = Palette.TextTertiary, style = MaterialTheme.typography.bodySmall, maxLines = 2) }
    }
}

@Composable
private fun BackendRow(name: String, selected: Boolean, onClick: () -> Unit) {
    Row(
        modifier = Modifier.fillMaxWidth().padding(vertical = 6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(name, color = if (selected) Palette.Accent else Palette.TextPrimary, modifier = Modifier.weight(1f))
        OutlinedButton(onClick = onClick) {
            Text(if (selected) "Selected" else "Select")
        }
    }
}

@Composable
private fun fieldColors() = TextFieldDefaults.colors(
    focusedContainerColor = Palette.Card,
    unfocusedContainerColor = Palette.Card,
    focusedTextColor = Palette.TextPrimary,
    unfocusedTextColor = Palette.TextPrimary,
    cursorColor = Palette.Accent,
)

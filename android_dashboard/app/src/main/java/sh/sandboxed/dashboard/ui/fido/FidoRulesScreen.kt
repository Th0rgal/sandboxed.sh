package sh.sandboxed.dashboard.ui.fido

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
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.Fingerprint
import androidx.compose.material.icons.filled.Lock
import androidx.compose.material.icons.filled.Public
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
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
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.launch
import sh.sandboxed.dashboard.data.AppContainer
import sh.sandboxed.dashboard.data.AutoApprovalRule
import sh.sandboxed.dashboard.data.AutoApprovalRuleType
import sh.sandboxed.dashboard.ui.components.GlassCard
import sh.sandboxed.dashboard.ui.theme.Palette
import java.util.UUID

@Composable
fun FidoRulesScreen(container: AppContainer, onBack: () -> Unit) {
    val scope = rememberCoroutineScope()
    val settings by container.cached.collectAsState()
    var showAdd by remember { mutableStateOf(false) }

    Column(Modifier.fillMaxSize()) {
        Row(Modifier.fillMaxWidth().padding(horizontal = 8.dp, vertical = 8.dp), verticalAlignment = Alignment.CenterVertically) {
            IconButton(onClick = onBack) { Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back", tint = Palette.TextPrimary) }
            Text("FIDO approvals", style = MaterialTheme.typography.titleLarge, color = Palette.TextPrimary, modifier = Modifier.weight(1f))
            IconButton(onClick = { showAdd = true }) { Icon(Icons.Filled.Add, "Add rule", tint = Palette.Accent) }
        }

        LazyColumn(contentPadding = PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            item {
                GlassCard(modifier = Modifier.fillMaxWidth()) {
                    Column(Modifier.padding(16.dp)) {
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            Column(Modifier.weight(1f)) {
                                Text("Always require biometric", color = Palette.TextPrimary, style = MaterialTheme.typography.titleSmall)
                                Text("Apply biometric prompt to every approval, even when a rule matches.", color = Palette.TextTertiary, style = MaterialTheme.typography.bodySmall)
                            }
                            Switch(
                                checked = settings.fidoRequireBiometricAll,
                                onCheckedChange = { v -> scope.launch { container.settings.setFidoRequireBiometricAll(v) } },
                                colors = SwitchDefaults.colors(checkedThumbColor = Palette.Accent),
                            )
                        }
                    }
                }
            }

            if (settings.fidoRules.isEmpty()) {
                item {
                    GlassCard(modifier = Modifier.fillMaxWidth()) {
                        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                            Text("No rules", color = Palette.TextSecondary, style = MaterialTheme.typography.titleSmall)
                            Text("Sign requests will always prompt. Add a rule to auto-approve matching requests.", color = Palette.TextTertiary, style = MaterialTheme.typography.bodySmall)
                        }
                    }
                }
            } else {
                items(settings.fidoRules, key = { it.id }) { r ->
                    RuleRow(r) {
                        scope.launch {
                            container.settings.setFidoRules(settings.fidoRules.filterNot { it.id == r.id })
                        }
                    }
                }
            }
        }
    }

    if (showAdd) {
        AddRuleDialog(
            onCancel = { showAdd = false },
            onAdd = { type, value, requireBio, expiryHours ->
                val nowSec = System.currentTimeMillis() / 1000
                val rule = AutoApprovalRule(
                    id = UUID.randomUUID().toString(),
                    ruleType = type,
                    value = value.takeIf { it.isNotBlank() },
                    expiresAtEpochSec = expiryHours?.let { nowSec + it * 3600L },
                    requireBiometric = requireBio,
                    createdAtEpochSec = nowSec,
                )
                scope.launch { container.settings.setFidoRules(settings.fidoRules + rule) }
                showAdd = false
            }
        )
    }
}

@Composable
private fun RuleRow(r: AutoApprovalRule, onDelete: () -> Unit) {
    val nowSec = System.currentTimeMillis() / 1000
    val expired = r.isExpired(nowSec)
    GlassCard(modifier = Modifier.fillMaxWidth()) {
        Column(Modifier.padding(12.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Icon(ruleIcon(r.ruleType), null, tint = if (expired) Palette.TextTertiary else Palette.AccentLight)
                Spacer(Modifier.height(0.dp))
                Column(Modifier.padding(start = 12.dp).weight(1f)) {
                    Text(ruleLabel(r.ruleType), color = if (expired) Palette.TextTertiary else Palette.TextPrimary, style = MaterialTheme.typography.titleSmall)
                    r.value?.takeIf { it.isNotBlank() }?.let {
                        Text(it, color = Palette.TextSecondary, style = MaterialTheme.typography.bodySmall.copy(fontFamily = FontFamily.Monospace), maxLines = 1)
                    }
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        if (r.requireBiometric) Text("biometric · ", color = Palette.AccentLight, style = MaterialTheme.typography.labelSmall)
                        Text(expiryLabel(r, nowSec), color = if (expired) Palette.Error else Palette.TextTertiary, style = MaterialTheme.typography.labelSmall)
                    }
                }
                IconButton(onClick = onDelete) { Icon(Icons.Filled.Delete, "Delete", tint = Palette.Error) }
            }
        }
    }
}

private fun ruleIcon(t: AutoApprovalRuleType): ImageVector = when (t) {
    AutoApprovalRuleType.ALL_SSH -> Icons.Filled.Lock
    AutoApprovalRuleType.HOSTNAME -> Icons.Filled.Public
    AutoApprovalRuleType.KEY_FINGERPRINT -> Icons.Filled.Fingerprint
}

private fun ruleLabel(t: AutoApprovalRuleType): String = when (t) {
    AutoApprovalRuleType.ALL_SSH -> "Any SSH key"
    AutoApprovalRuleType.HOSTNAME -> "Match hostname"
    AutoApprovalRuleType.KEY_FINGERPRINT -> "Match fingerprint"
}

private fun expiryLabel(r: AutoApprovalRule, nowSec: Long): String {
    val exp = r.expiresAtEpochSec ?: return "no expiry"
    if (exp <= nowSec) return "expired"
    val secsLeft = exp - nowSec
    val hours = secsLeft / 3600
    val days = hours / 24
    return when {
        days > 1 -> "$days days left"
        hours > 1 -> "$hours hrs left"
        else -> "${(secsLeft / 60).coerceAtLeast(1)} min left"
    }
}

@Composable
private fun AddRuleDialog(
    onCancel: () -> Unit,
    onAdd: (AutoApprovalRuleType, String, Boolean, Int?) -> Unit,
) {
    var type by remember { mutableStateOf(AutoApprovalRuleType.ALL_SSH) }
    var value by remember { mutableStateOf("") }
    var requireBio by remember { mutableStateOf(false) }
    var expiry by remember { mutableStateOf<Int?>(24) }

    AlertDialog(
        onDismissRequest = onCancel,
        title = { Text("Add approval rule") },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Text("Match", color = Palette.TextSecondary, style = MaterialTheme.typography.labelMedium)
                Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    AutoApprovalRuleType.entries.forEach { t ->
                        FilterChip(
                            selected = type == t, onClick = { type = t },
                            label = { Text(when (t) {
                                AutoApprovalRuleType.ALL_SSH -> "All SSH"
                                AutoApprovalRuleType.HOSTNAME -> "Host"
                                AutoApprovalRuleType.KEY_FINGERPRINT -> "Fingerprint"
                            }, style = MaterialTheme.typography.labelSmall) },
                            colors = chipColors(),
                        )
                    }
                }
                if (type != AutoApprovalRuleType.ALL_SSH) {
                    OutlinedTextField(
                        value = value, onValueChange = { value = it }, singleLine = true,
                        label = { Text(if (type == AutoApprovalRuleType.HOSTNAME) "Hostname" else "Key fingerprint") },
                        modifier = Modifier.fillMaxWidth(),
                        colors = ruleFieldColors(),
                    )
                }
                Spacer(Modifier.height(2.dp))
                Text("Expiry", color = Palette.TextSecondary, style = MaterialTheme.typography.labelMedium)
                Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    listOf(1 to "1h", 24 to "24h", 24 * 7 to "7d", null to "never").forEach { (h, label) ->
                        FilterChip(
                            selected = expiry == h, onClick = { expiry = h },
                            label = { Text(label, style = MaterialTheme.typography.labelSmall) },
                            colors = chipColors(),
                        )
                    }
                }
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Column(Modifier.weight(1f)) {
                        Text("Require biometric", color = Palette.TextPrimary, style = MaterialTheme.typography.bodyMedium)
                        Text("Show prompt + biometric even when this rule matches.", color = Palette.TextTertiary, style = MaterialTheme.typography.bodySmall)
                    }
                    Switch(
                        checked = requireBio, onCheckedChange = { requireBio = it },
                        colors = SwitchDefaults.colors(checkedThumbColor = Palette.Accent),
                    )
                }
            }
        },
        confirmButton = {
            Button(
                onClick = { onAdd(type, value, requireBio, expiry) },
                enabled = type == AutoApprovalRuleType.ALL_SSH || value.isNotBlank(),
                colors = ButtonDefaults.buttonColors(containerColor = Palette.Accent),
            ) { Text("Add") }
        },
        dismissButton = { TextButton(onClick = onCancel) { Text("Cancel") } },
        containerColor = Palette.Card,
    )
}

@Composable
private fun chipColors() = FilterChipDefaults.filterChipColors(
    containerColor = Palette.Card,
    selectedContainerColor = Palette.Accent.copy(alpha = 0.18f),
    labelColor = Palette.TextSecondary,
    selectedLabelColor = Palette.Accent,
)

@Composable
private fun ruleFieldColors() = TextFieldDefaults.colors(
    focusedContainerColor = Palette.Card,
    unfocusedContainerColor = Palette.Card,
    focusedTextColor = Palette.TextPrimary,
    unfocusedTextColor = Palette.TextPrimary,
    cursorColor = Palette.Accent,
)

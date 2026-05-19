package sh.sandboxed.dashboard.ui.more

import android.net.Uri
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.KeyboardArrowRight
import androidx.compose.material.icons.filled.Computer
import androidx.compose.material.icons.filled.Fingerprint
import androidx.compose.material.icons.filled.MonetizationOn
import androidx.compose.material.icons.filled.Schedule
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.unit.dp
import sh.sandboxed.dashboard.ui.components.GlassCard
import sh.sandboxed.dashboard.ui.theme.Palette

@Composable
fun MoreScreen(onNavigate: (String) -> Unit) {
    LazyColumn(
        modifier = Modifier.fillMaxSize(),
        contentPadding = PaddingValues(16.dp),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        item { Text("More", style = MaterialTheme.typography.headlineSmall, color = Palette.TextPrimary, modifier = Modifier.padding(bottom = 8.dp)) }
        item { Tile("Workspaces", "Hosts and containers", Icons.Filled.Computer) { onNavigate("workspaces") } }
        item { Tile("Desktop", "Live stream and remote input", Icons.Filled.Computer) { onNavigate("desktop/${Uri.encode(":101")}") } }
        item { Tile("Tasks", "Subtasks running on the agent", Icons.Filled.Schedule) { onNavigate("tasks") } }
        item { Tile("Runs", "Cost-tracked invocations", Icons.Filled.MonetizationOn) { onNavigate("runs") } }
        item { Tile("FIDO approvals", "Auto-approve signing requests", Icons.Filled.Fingerprint) { onNavigate("fido_rules") } }
        item { Spacer(Modifier.size(8.dp)) }
        item { Tile("Settings", "Server, defaults, sign-out", Icons.Filled.Settings) { onNavigate("settings") } }
    }
}

@Composable
private fun Tile(title: String, subtitle: String, icon: ImageVector, onClick: () -> Unit) {
    GlassCard(modifier = Modifier.fillMaxWidth(), onClick = onClick) {
        Row(Modifier.padding(16.dp), verticalAlignment = Alignment.CenterVertically) {
            Icon(icon, null, tint = Palette.AccentLight)
            Spacer(Modifier.width(12.dp))
            Column(Modifier.weight(1f)) {
                Text(title, color = Palette.TextPrimary, style = MaterialTheme.typography.titleSmall)
                Text(subtitle, color = Palette.TextTertiary, style = MaterialTheme.typography.bodySmall)
            }
            Icon(Icons.AutoMirrored.Filled.KeyboardArrowRight, null, tint = Palette.TextTertiary)
        }
    }
}

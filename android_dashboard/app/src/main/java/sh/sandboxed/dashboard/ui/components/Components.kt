package sh.sandboxed.dashboard.ui.components

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.CheckCircle
import androidx.compose.material.icons.filled.Error
import androidx.compose.material.icons.filled.HourglassEmpty
import androidx.compose.material.icons.filled.PauseCircle
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material.icons.filled.QuestionMark
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.unit.dp
import sh.sandboxed.dashboard.data.MissionStatus
import sh.sandboxed.dashboard.ui.theme.Palette

@Composable
fun GlassCard(
    modifier: Modifier = Modifier,
    onClick: (() -> Unit)? = null,
    content: @Composable () -> Unit,
) {
    val base = Modifier
        .background(Palette.Card, RoundedCornerShape(16.dp))
        .border(BorderStroke(1.dp, Palette.Border), RoundedCornerShape(16.dp))
    Box(modifier = modifier.then(base).then(if (onClick != null) Modifier.clickable { onClick() } else Modifier)) {
        content()
    }
}

private data class StatusVisual(val color: Color, val icon: ImageVector, val label: String)

private fun MissionStatus.visual(): StatusVisual = when (this) {
    MissionStatus.PENDING -> StatusVisual(Palette.TextTertiary, Icons.Filled.HourglassEmpty, "Pending")
    MissionStatus.ACTIVE -> StatusVisual(Palette.Info, Icons.Filled.PlayArrow, "Active")
    MissionStatus.COMPLETED -> StatusVisual(Palette.Success, Icons.Filled.CheckCircle, "Completed")
    MissionStatus.FAILED -> StatusVisual(Palette.Error, Icons.Filled.Error, "Failed")
    MissionStatus.INTERRUPTED -> StatusVisual(Palette.Warning, Icons.Filled.PauseCircle, "Interrupted")
    MissionStatus.BLOCKED -> StatusVisual(Palette.Warning, Icons.Filled.PauseCircle, "Blocked")
    MissionStatus.NOT_FEASIBLE -> StatusVisual(Palette.Error, Icons.Filled.Error, "Not feasible")
    MissionStatus.UNKNOWN -> StatusVisual(Palette.TextTertiary, Icons.Filled.QuestionMark, "Unknown")
}

@Composable
fun StatusBadge(status: MissionStatus, modifier: Modifier = Modifier) {
    val v = status.visual()
    Row(
        modifier = modifier
            .background(v.color.copy(alpha = 0.12f), RoundedCornerShape(999.dp))
            .border(BorderStroke(1.dp, v.color.copy(alpha = 0.32f)), RoundedCornerShape(999.dp))
            .padding(horizontal = 8.dp, vertical = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        Icon(v.icon, contentDescription = null, tint = v.color, modifier = Modifier.size(14.dp))
        Text(v.label, color = v.color, style = MaterialTheme.typography.labelMedium)
    }
}

@Composable
fun SectionHeader(text: String) {
    Text(
        text,
        style = MaterialTheme.typography.titleMedium,
        color = Palette.TextPrimary,
        modifier = Modifier.fillMaxWidth().padding(horizontal = 4.dp, vertical = 8.dp),
    )
}

@Composable
fun ErrorBanner(message: String) {
    Box(
        Modifier
            .fillMaxWidth()
            .background(Palette.Error.copy(alpha = 0.12f), RoundedCornerShape(12.dp))
            .border(BorderStroke(1.dp, Palette.Error.copy(alpha = 0.32f)), RoundedCornerShape(12.dp))
            .padding(12.dp)
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Icon(Icons.Filled.Error, null, tint = Palette.Error, modifier = Modifier.size(18.dp))
            Spacer(Modifier.width(8.dp))
            Text(message, color = Palette.Error, style = MaterialTheme.typography.bodyMedium)
        }
    }
}

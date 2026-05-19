package sh.sandboxed.dashboard.ui.components

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import sh.sandboxed.dashboard.data.ToolUiContent
import sh.sandboxed.dashboard.ui.theme.Palette

@Composable
fun ToolUiWidget(content: ToolUiContent) {
    when (content) {
        is ToolUiContent.DataTable -> DataTableView(content)
        is ToolUiContent.OptionList -> OptionListView(content)
        is ToolUiContent.Progress -> ProgressView(content)
        is ToolUiContent.Alert -> AlertView(content)
        is ToolUiContent.CodeBlock -> CodeBlockView(content)
        is ToolUiContent.Unknown -> UnknownToolUi(content)
    }
}

@Composable
private fun DataTableView(t: ToolUiContent.DataTable) {
    GlassCard(modifier = Modifier.fillMaxWidth()) {
        Column(Modifier.padding(12.dp)) {
            t.title?.let { Text(it, color = Palette.AccentLight, style = MaterialTheme.typography.labelLarge); Spacer(Modifier.height(8.dp)) }
            val scroll = rememberScrollState()
            Column(Modifier.horizontalScroll(scroll)) {
                Row {
                    t.columns.forEach { c ->
                        Text(c.label, color = Palette.TextSecondary, style = MaterialTheme.typography.labelMedium, modifier = Modifier.widthIn(min = 80.dp).padding(end = 12.dp))
                    }
                }
                Spacer(Modifier.height(4.dp))
                t.rows.forEach { row ->
                    Row {
                        t.columns.forEach { c ->
                            Text(row[c.id] ?: "", color = Palette.TextPrimary, style = MaterialTheme.typography.bodySmall, modifier = Modifier.widthIn(min = 80.dp).padding(end = 12.dp))
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun OptionListView(o: ToolUiContent.OptionList) {
    GlassCard(modifier = Modifier.fillMaxWidth()) {
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
            Text(if (o.multiSelect) "Choose options" else "Choose one", color = Palette.AccentLight, style = MaterialTheme.typography.labelLarge)
            o.options.forEach { opt ->
                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .background(Palette.BackgroundTertiary, RoundedCornerShape(8.dp))
                        .padding(10.dp),
                ) {
                    Column(Modifier.weight(1f)) {
                        Text(opt.label, color = if (opt.disabled) Palette.TextMuted else Palette.TextPrimary, style = MaterialTheme.typography.bodyMedium)
                        opt.description?.let { Text(it, color = Palette.TextTertiary, style = MaterialTheme.typography.bodySmall) }
                    }
                }
            }
        }
    }
}

@Composable
private fun ProgressView(p: ToolUiContent.Progress) {
    GlassCard(modifier = Modifier.fillMaxWidth()) {
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
            p.title?.let { Text(it, color = Palette.AccentLight, style = MaterialTheme.typography.labelLarge) }
            LinearProgressIndicator(
                progress = { p.percentage.coerceIn(0f, 1f) },
                modifier = Modifier.fillMaxWidth(),
                color = Palette.Accent,
                trackColor = Palette.BackgroundTertiary,
            )
            Row {
                Text("${p.current} / ${p.total}", color = Palette.TextSecondary, style = MaterialTheme.typography.bodySmall, modifier = Modifier.weight(1f))
                p.status?.let { Text(it, color = Palette.TextTertiary, style = MaterialTheme.typography.bodySmall) }
            }
        }
    }
}

@Composable
private fun AlertView(a: ToolUiContent.Alert) {
    val color = when (a.severity) {
        "success" -> Palette.Success
        "warning" -> Palette.Warning
        "error" -> Palette.Error
        else -> Palette.Info
    }
    Box(
        Modifier
            .fillMaxWidth()
            .background(color.copy(alpha = 0.12f), RoundedCornerShape(12.dp))
            .border(1.dp, color.copy(alpha = 0.32f), RoundedCornerShape(12.dp))
            .padding(12.dp)
    ) {
        Column {
            Text(a.title, color = color, style = MaterialTheme.typography.titleSmall)
            a.message?.let { Spacer(Modifier.height(4.dp)); Text(it, color = Palette.TextSecondary, style = MaterialTheme.typography.bodyMedium) }
        }
    }
}

@Composable
private fun CodeBlockView(c: ToolUiContent.CodeBlock) {
    GlassCard(modifier = Modifier.fillMaxWidth()) {
        Column(Modifier.padding(12.dp)) {
            Row(verticalAlignment = androidx.compose.ui.Alignment.CenterVertically) {
                c.title?.let { Text(it, color = Palette.AccentLight, style = MaterialTheme.typography.labelLarge, modifier = Modifier.weight(1f)) }
                c.language?.let { Text(it, color = Palette.TextTertiary, style = MaterialTheme.typography.labelSmall) }
            }
            Spacer(Modifier.height(6.dp))
            val scroll = rememberScrollState()
            Box(
                Modifier
                    .fillMaxWidth()
                    .background(Palette.TerminalBackground, RoundedCornerShape(8.dp))
                    .horizontalScroll(scroll)
                    .padding(10.dp)
            ) {
                Text(
                    c.code,
                    color = Palette.TextPrimary,
                    style = TextStyle(fontFamily = FontFamily.Monospace, fontSize = 12.sp, lineHeight = 16.sp),
                )
            }
        }
    }
}

@Composable
private fun UnknownToolUi(u: ToolUiContent.Unknown) {
    GlassCard(modifier = Modifier.fillMaxWidth()) {
        Column(Modifier.padding(12.dp)) {
            Text("UI: ${u.name}", color = Palette.Info, style = MaterialTheme.typography.labelLarge)
            Spacer(Modifier.height(4.dp))
            Text(
                u.rawArgs.take(400),
                color = Palette.TextSecondary,
                style = TextStyle(fontFamily = FontFamily.Monospace, fontSize = 12.sp),
            )
        }
    }
}

private val unused: Color = Color.Transparent

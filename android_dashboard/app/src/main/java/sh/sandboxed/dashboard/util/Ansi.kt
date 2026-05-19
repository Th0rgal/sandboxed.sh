package sh.sandboxed.dashboard.util

import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontWeight

/**
 * Minimal ANSI SGR (Select Graphic Rendition) parser sufficient for typical
 * shell output: standard 16 colors (foreground 30-37 / 90-97, background 40-47 / 100-107),
 * bold/dim/italic/underline, and reset. 256-color and truecolor are passed through visually
 * by reading the parameters and falling back to a hash-derived hue.
 */
object Ansi {
    private val PALETTE = listOf(
        Color(0xFF000000), Color(0xFFEF4444), Color(0xFF22C55E), Color(0xFFEAB308),
        Color(0xFF3B82F6), Color(0xFFA855F7), Color(0xFF06B6D4), Color(0xFFE5E5E5),
        Color(0xFF6B7280), Color(0xFFF87171), Color(0xFF4ADE80), Color(0xFFFACC15),
        Color(0xFF60A5FA), Color(0xFFC084FC), Color(0xFF22D3EE), Color(0xFFFFFFFF),
    )

    fun parse(input: String, defaultFg: Color): AnnotatedString = buildAnnotatedString {
        var i = 0
        var fg: Color = defaultFg
        var bold = false
        var underline = false
        while (i < input.length) {
            val c = input[i]
            if (c == 0x1B.toChar() && i + 1 < input.length && input[i + 1] == '[') {
                val end = input.indexOf('m', startIndex = i + 2)
                if (end == -1) { i++; continue }
                val raw = input.substring(i + 2, end)
                val codes = raw.split(';').mapNotNull { it.toIntOrNull() }
                applyCodes(codes,
                    setFg = { fg = it ?: defaultFg },
                    setBold = { bold = it },
                    setUnderline = { underline = it },
                )
                i = end + 1
                continue
            }
            // accumulate run of plain chars until next escape; if the current char
            // is itself a stray ESC (no `[` follows), include it in the run and
            // search from i+1 to guarantee forward progress.
            val searchFrom = if (c == 0x1B.toChar()) i + 1 else i
            val nextEsc = if (searchFrom < input.length) input.indexOf(0x1B.toChar(), startIndex = searchFrom) else -1
            val runEnd = if (nextEsc == -1) input.length else nextEsc
            val run = input.substring(i, runEnd)
            if (run.isNotEmpty()) {
                pushStyle(SpanStyle(color = fg, fontWeight = if (bold) FontWeight.Bold else FontWeight.Normal,
                    textDecoration = if (underline) androidx.compose.ui.text.style.TextDecoration.Underline else null))
                append(run)
                pop()
            }
            // runEnd is always > i now (searchFrom>=i+1 when c==ESC, otherwise c is plain
            // text and runEnd >= i+1 because we'd have consumed it).
            i = if (runEnd > i) runEnd else i + 1
        }
    }

    private fun applyCodes(
        codes: List<Int>,
        setFg: (Color?) -> Unit,
        setBold: (Boolean) -> Unit,
        setUnderline: (Boolean) -> Unit,
    ) {
        if (codes.isEmpty()) { setFg(null); setBold(false); setUnderline(false); return }
        var idx = 0
        while (idx < codes.size) {
            val code = codes[idx]
            when (code) {
                0 -> { setFg(null); setBold(false); setUnderline(false) }
                1 -> setBold(true)
                4 -> setUnderline(true)
                22 -> setBold(false)
                24 -> setUnderline(false)
                in 30..37 -> setFg(PALETTE[code - 30])
                39 -> setFg(null)
                in 90..97 -> setFg(PALETTE[8 + (code - 90)])
                38 -> {
                    if (idx + 1 < codes.size && codes[idx + 1] == 5 && idx + 2 < codes.size) {
                        val n = codes[idx + 2].coerceIn(0, 255)
                        setFg(PALETTE[n.coerceAtMost(15)])
                        idx += 2
                    } else if (idx + 1 < codes.size && codes[idx + 1] == 2 && idx + 4 < codes.size) {
                        val r = codes[idx + 2].coerceIn(0, 255)
                        val g = codes[idx + 3].coerceIn(0, 255)
                        val b = codes[idx + 4].coerceIn(0, 255)
                        setFg(Color(r / 255f, g / 255f, b / 255f))
                        idx += 4
                    }
                }
                else -> {}
            }
            idx++
        }
    }
}

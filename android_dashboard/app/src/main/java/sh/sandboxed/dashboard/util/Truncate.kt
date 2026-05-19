package sh.sandboxed.dashboard.util

/**
 * Bound a string passed into a [androidx.compose.material3.Text] so Compose's
 * paragraph layout doesn't grind on multi-megabyte agent outputs on the main
 * thread. Compose uses `maxLines` for visual clipping but still measures the
 * full string before clipping, so very long inputs (or pathological line
 * structures) can ANR a list row.
 *
 * The cutoff defaults to 4 KB which comfortably exceeds what fits in a typical
 * card row and is fast to lay out.
 */
fun String.boundedForText(maxChars: Int = 4_000): String =
    if (length <= maxChars) this else substring(0, maxChars) + "…"

/**
 * Format a byte count into a human-readable string (e.g. "1.5 MB").
 * Handles up to petabyte scale.
 */
export function formatBytes(bytes: number, decimals = 1): string {
  if (!Number.isFinite(bytes) || bytes < 0) return "-";
  if (bytes === 0) return "0 B";
  if (bytes < 1024) return `${bytes} B`;

  const units = ["KB", "MB", "GB", "TB", "PB"] as const;
  let value = bytes / 1024;
  let unitIndex = 0;

  while (value >= 1024 && unitIndex < units.length - 1) {
    value /= 1024;
    unitIndex++;
  }

  return `${value.toFixed(value >= 10 ? 0 : decimals)} ${units[unitIndex]}`;
}

/**
 * Format bytes per second into a human-readable string (e.g. "1.5 MB/s").
 */
export function formatBytesPerSec(bytes: number): string {
  return formatBytes(bytes) + "/s";
}

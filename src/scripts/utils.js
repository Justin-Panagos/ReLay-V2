/**
 * Shared utility functions used across multiple modules.
 */

/**
 * Formats a byte count into a human-readable string (B, KB, MB, GB).
 *
 * Args:
 *   bytes: Number of bytes.
 *
 * Returns:
 *   Formatted string like "4.2 MB".
 */
export function formatBytes(bytes) {
  if (bytes < 1024) return `${Math.round(bytes)} B`
  if (bytes < 1_048_576) return `${(bytes / 1024).toFixed(1)} KB`
  if (bytes < 1_073_741_824) return `${(bytes / 1_048_576).toFixed(1)} MB`
  return `${(bytes / 1_073_741_824).toFixed(2)} GB`
}

/**
 * Escapes a string for safe insertion into HTML attribute values or text content.
 *
 * Args:
 *   str: The string to escape.
 *
 * Returns:
 *   HTML-safe string.
 */
export function escHtml(str) {
  return String(str)
    .replace(/&/g, '&amp;')
    .replace(/"/g, '&quot;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
}

/**
 * History tab — renders completed and failed downloads from the database.
 */

import { invoke } from '@tauri-apps/api/tauri'
import { open } from '@tauri-apps/api/shell'

/**
 * Loads recent downloads from the database and renders them in the History tab.
 * Clears existing content before re-rendering. Only shows terminal statuses
 * (complete, failed) — active downloads are shown in the Downloads tab.
 */
export async function loadHistory() {
  const container = document.getElementById('history-list')
  if (!container) return

  let records
  try {
    records = await invoke('get_downloads', { limit: 50 })
  } catch {
    return
  }

  const done = records.filter(r => r.status === 'complete' || r.status === 'failed')

  if (done.length === 0) {
    container.innerHTML = '<div class="empty-state">No download history yet</div>'
    return
  }

  container.innerHTML = done.map(renderHistoryCard).join('')

  container.querySelectorAll('.history-open-btn').forEach(btn => {
    btn.addEventListener('click', async (e) => {
      e.stopPropagation()
      await open(e.currentTarget.dataset.path)
    })
  })
}

/**
 * Returns an HTML string for a single history card row.
 *
 * Args:
 *   record: A DownloadRecord object returned from the backend.
 *
 * Returns:
 *   HTML string for the card element.
 */
function renderHistoryCard(record) {
  const isComplete = record.status === 'complete'
  const iconClass = isComplete ? 'status-complete' : 'status-error'
  const iconChar = isComplete ? '&#10003;' : '&#10005;'
  const statusText = isComplete ? 'Complete' : 'Failed'
  const sizeText = record.size_bytes ? ' \u00b7 ' + formatBytes(record.size_bytes) : ''
  const dateText = formatDate(record.completed_at || record.created_at)
  const safeName = escHtml(record.filename)
  const safeDest = escHtml(record.destination)

  return `
    <div class="download-card">
      <span class="card-type-icon ${iconClass}">${iconChar}</span>
      <div class="card-body">
        <div class="card-name" title="${safeName}">${safeName}</div>
        <div class="card-meta">
          <span>${statusText}${sizeText}</span>
          <span>${dateText}</span>
        </div>
      </div>
      <div class="card-actions">
        <button class="btn-icon history-open-btn" title="Open folder" data-path="${safeDest}">&#8599;</button>
      </div>
    </div>
  `
}

/**
 * Formats a byte count into a human-readable string (B, KB, MB, GB).
 *
 * Args:
 *   bytes: Number of bytes.
 *
 * Returns:
 *   Formatted string like "4.2 MB".
 */
function formatBytes(bytes) {
  if (bytes < 1024) return `${Math.round(bytes)} B`
  if (bytes < 1_048_576) return `${(bytes / 1024).toFixed(1)} KB`
  if (bytes < 1_073_741_824) return `${(bytes / 1_048_576).toFixed(1)} MB`
  return `${(bytes / 1_073_741_824).toFixed(2)} GB`
}

/**
 * Formats a timestamp from the database into a short human-readable date+time.
 * Handles both Unix seconds (numeric string) and ISO date strings.
 *
 * Args:
 *   ts: Timestamp string from the database, or null.
 *
 * Returns:
 *   Formatted string like "Mar 25, 14:30", or empty string if ts is null.
 */
function formatDate(ts) {
  if (!ts) return ''
  const n = Number(ts)
  const d = isNaN(n) ? new Date(ts) : new Date(n * 1000)
  return d.toLocaleDateString(undefined, {
    month: 'short',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
  })
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
function escHtml(str) {
  return String(str)
    .replace(/&/g, '&amp;')
    .replace(/"/g, '&quot;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
}

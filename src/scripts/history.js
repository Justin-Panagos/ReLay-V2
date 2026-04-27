/**
 * History tab — renders completed and failed downloads from the database.
 */

import { invoke } from '@tauri-apps/api/tauri'
import { open } from '@tauri-apps/api/shell'
import { formatBytes, escHtml } from './utils.js'
import { showErrorToast, showInfoToast } from './toast.js'
import { t } from './i18n.js'

/** Maximum number of history records to fetch. */
const HISTORY_LIMIT = 50

/**
 * Loads recent downloads from the database and renders them in the History tab.
 * Clears existing content before re-rendering. Only shows terminal statuses
 * (complete, failed) — active downloads are shown in the Downloads tab.
 * Also wires the Clear History button on first call.
 */
export async function loadHistory() {
  const container = document.getElementById('history-list')
  if (!container) return

  container.innerHTML = `<div class="empty-state">${t('history.loading')}</div>`

  const clearBtn = document.getElementById('history-clear-btn')
  if (clearBtn && !clearBtn.dataset.wired) {
    clearBtn.dataset.wired = '1'
    clearBtn.addEventListener('click', async () => {
      try {
        await invoke('clear_history')
        showInfoToast(t('history.cleared'))
        await loadHistory()
      } catch (err) {
        console.error('clear_history failed:', err)
        showErrorToast(`${t('history.clear_failed')}: ${err}`)
      }
    })
  }

  let records
  try {
    records = await invoke('get_downloads', { limit: HISTORY_LIMIT })
  } catch {
    container.innerHTML = `<div class="empty-state">${t('history.load_failed')}</div>`
    return
  }

  const done = records.filter(r => r.status === 'complete' || r.status === 'failed')

  if (done.length === 0) {
    container.innerHTML = `<div class="empty-state">${t('history.empty')}</div>`
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
  const statusText = isComplete ? t('history.status_complete') : t('history.status_failed')
  const sizeText = record.size_bytes ? ' · ' + formatBytes(record.size_bytes) : ''
  const durationText = (isComplete && record.completed_at && record.created_at)
    ? ' · ' + formatDuration(record.created_at, record.completed_at)
    : ''
  const dateText = formatDate(record.completed_at || record.created_at)
  const safeName = escHtml(record.filename)
  const safeDest = escHtml(record.destination)

  return `
    <div class="download-card">
      <span class="card-type-icon ${iconClass}">${iconChar}</span>
      <div class="card-body">
        <div class="card-name" title="${safeName}">${safeName}</div>
        <div class="card-meta">
          <span>${statusText}${sizeText}${durationText}</span>
          <span>${dateText}</span>
        </div>
      </div>
      <div class="card-actions">
        <button class="btn-icon history-open-btn" title="${t('history.open_folder')}" data-path="${safeDest}">&#8599;</button>
      </div>
    </div>
  `
}

/**
 * Formats the elapsed seconds between two Unix timestamp strings into a human-readable duration.
 *
 * Args:
 *   startTs: Unix seconds string (created_at).
 *   endTs:   Unix seconds string (completed_at).
 *
 * Returns:
 *   String like "34s", "2m 5s", or "14m".
 */
function formatDuration(startTs, endTs) {
  const s = Math.max(0, Number(endTs) - Number(startTs))
  if (s < 60) return `${s}s`
  const m = Math.floor(s / 60)
  const rem = s % 60
  return rem > 0 ? `${m}m ${rem}s` : `${m}m`
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

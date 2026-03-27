/**
 * Quarantine tab — renders and manages quarantine entries.
 * Mirrors the structure of history.js: loads records from the backend,
 * renders cards with Restore and Delete actions.
 */

import { invoke } from '@tauri-apps/api/tauri'
import { formatBytes, escHtml } from './utils.js'

/**
 * Loads all quarantine entries from the backend and renders them in the
 * Quarantine tab. Shows or hides the empty state accordingly.
 */
export async function loadQuarantine() {
  let records
  try {
    records = await invoke('get_quarantine', { limit: 200 })
  } catch {
    return
  }

  const list = document.getElementById('quarantine-list')
  const empty = document.getElementById('quarantine-empty')
  if (!list || !empty) return

  list.innerHTML = ''

  if (records.length === 0) {
    empty.classList.remove('hidden')
    return
  }

  empty.classList.add('hidden')
  for (const record of records) {
    list.appendChild(buildQuarantineCard(record))
  }
}

/**
 * Builds a DOM element representing a single quarantine entry.
 * Wires Restore and Delete button click handlers.
 *
 * Args:
 *   record: A QuarantineRecord object from the backend.
 *
 * Returns:
 *   The constructed card HTMLElement.
 */
function buildQuarantineCard(record) {
  const card = document.createElement('div')
  card.className = 'download-card'
  card.dataset.quarantineId = record.id

  const date = record.quarantined_at
    ? new Date(Number(record.quarantined_at) * 1000).toLocaleDateString()
    : ''

  card.innerHTML = `
    <span class="card-type-icon status-error">&#9888;</span>
    <div class="card-body">
      <div class="card-name" title="${escHtml(record.filename)}">${escHtml(record.filename)}</div>
      <div class="card-meta">
        <span class="card-status status-error">Quarantined</span>
        <span class="card-speed">${escHtml(date)}</span>
      </div>
      <div class="card-quarantine-reason">${escHtml(record.threat_reason)}</div>
    </div>
    <div class="card-actions">
      <button class="btn-icon quar-restore" title="Restore to original location">&#8617;</button>
      <button class="btn-icon quar-delete" title="Delete permanently">&#128465;</button>
    </div>
  `

  card.querySelector('.quar-restore').addEventListener('click', async (e) => {
    e.stopPropagation()
    try {
      await invoke('restore_quarantine', { id: record.id })
      card.remove()
      checkEmpty()
    } catch (err) {
      console.error('restore_quarantine failed:', err)
    }
  })

  card.querySelector('.quar-delete').addEventListener('click', async (e) => {
    e.stopPropagation()
    try {
      await invoke('delete_quarantine', { id: record.id })
      card.remove()
      checkEmpty()
    } catch (err) {
      console.error('delete_quarantine failed:', err)
    }
  })

  return card
}

/**
 * Shows the empty state if no quarantine cards remain in the list.
 */
function checkEmpty() {
  const list = document.getElementById('quarantine-list')
  const empty = document.getElementById('quarantine-empty')
  if (list && empty && list.children.length === 0) {
    empty.classList.remove('hidden')
  }
}

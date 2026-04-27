/**
 * Quarantine tab — renders and manages quarantine entries.
 * Mirrors the structure of history.js: loads records from the backend,
 * renders cards with Restore and Delete actions.
 */

import { invoke } from '@tauri-apps/api/tauri'
import { formatBytes, escHtml } from './utils.js'
import { showErrorToast, showInfoToast } from './toast.js'
import { t } from './i18n.js'

/** Maximum number of quarantine records to fetch. */
const QUARANTINE_LIMIT = 200

/**
 * Loads all quarantine entries from the backend and renders them in the
 * Quarantine tab. Shows or hides the empty state accordingly.
 */
export async function loadQuarantine() {
  const list = document.getElementById('quarantine-list')
  const empty = document.getElementById('quarantine-empty')
  if (!list || !empty) return

  list.innerHTML = `<div class="empty-state">${t('quarantine.loading')}</div>`
  empty.classList.add('hidden')

  let records
  try {
    records = await invoke('get_quarantine', { limit: QUARANTINE_LIMIT })
  } catch {
    list.innerHTML = `<div class="empty-state">${t('quarantine.load_failed')}</div>`
    return
  }

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
        <span class="card-status status-error">${t('downloads.status.quarantined')}</span>
        <span class="card-speed">${escHtml(date)}</span>
      </div>
      <div class="card-quarantine-reason">${escHtml(record.threat_reason)}</div>
    </div>
    <div class="card-actions">
      <button class="btn-icon quar-submit" title="${t('quarantine.btn_submit')}">&#9873;</button>
      <button class="btn-icon quar-restore" title="${t('quarantine.btn_restore')}">&#8617;</button>
      <button class="btn-icon quar-delete" title="${t('quarantine.btn_delete')}">&#128465;</button>
    </div>
  `

  card.querySelector('.quar-submit').addEventListener('click', async (e) => {
    e.stopPropagation()
    const btn = e.target
    const originalHtml = btn.innerHTML
    btn.disabled = true
    btn.textContent = '…'
    try {
      await invoke('submit_zero_day', { quarantineId: record.id })
      showInfoToast(t('quarantine.submitted'))
      btn.innerHTML = originalHtml
      btn.disabled = true
      btn.title = t('quarantine.already_submitted')
    } catch (err) {
      console.error('submit_zero_day failed:', err)
      showErrorToast(`Submit failed: ${err}`)
      btn.innerHTML = originalHtml
      btn.disabled = false
    }
  })

  card.querySelector('.quar-restore').addEventListener('click', async (e) => {
    e.stopPropagation()
    try {
      await invoke('restore_quarantine', { id: record.id })
      card.remove()
      checkEmpty()
    } catch (err) {
      console.error('restore_quarantine failed:', err)
      showErrorToast(`Restore failed: ${err}`)
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
      showErrorToast(`Delete failed: ${err}`)
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

/**
 * Downloads tab — live card management.
 * Creates, updates, and removes download cards in response to Tauri events.
 */

import { invoke } from '@tauri-apps/api/tauri'
import { open } from '@tauri-apps/api/shell'
import { formatBytes, escHtml } from './utils.js'

/** @type {Map<number, { url: string, filename: string, destination: string, path?: string }>} */
const cardData = new Map()

/**
 * Creates and inserts a download card into the Downloads tab.
 * Hides the empty state. Stores card metadata for speed calculation and detail panel.
 * Wires pause, resume, and cancel button handlers.
 *
 * Args:
 *   id:          The download id from the backend.
 *   filename:    The filename being downloaded.
 *   url:         The source URL.
 *   destination: The destination directory path.
 *   options:     Optional config — isPaused (bool) and onResume (function) for paused cards.
 */
export function addDownloadCard(id, filename, url, destination, { isPaused = false, onResume = null } = {}) {
  document.getElementById('downloads-empty').classList.add('hidden')

  const card = document.createElement('div')
  card.className = 'download-card'
  card.dataset.id = id
  card.innerHTML = `
    <span class="card-type-icon ${isPaused ? 'status-paused' : 'status-downloading'}">${isPaused ? '&#8759;' : '&#8595;'}</span>
    <div class="card-body">
      <div class="card-name" title="${escHtml(filename)}">${escHtml(filename)}</div>
      <div class="progress-bar"><div class="progress-fill" style="width:0%"></div></div>
      <div class="card-meta">
        <span class="card-status">${isPaused ? 'Paused' : 'Starting\u2026'}</span>
        <span class="card-speed"></span>
      </div>
    </div>
    <div class="card-actions">
      <button class="btn-icon card-pause" title="Pause" ${isPaused ? 'style="display:none"' : ''}>&#9646;&#9646;</button>
      <button class="btn-icon card-resume" title="Resume" ${isPaused ? '' : 'style="display:none"'}>&#9654;</button>
      <button class="btn-icon card-cancel" title="Cancel">&#10005;</button>
    </div>
  `

  card.querySelector('.card-pause').addEventListener('click', async (e) => {
    e.stopPropagation()
    try { await invoke('pause_download', { id }) } catch { /* no-op if already done */ }
  })

  card.querySelector('.card-resume').addEventListener('click', async (e) => {
    e.stopPropagation()
    if (onResume) onResume(id)
  })

  card.querySelector('.card-cancel').addEventListener('click', async (e) => {
    e.stopPropagation()
    try { await invoke('cancel_download', { id }) } catch { /* no-op if already done */ }
  })

  card.addEventListener('click', () => showDetail(id))
  document.getElementById('downloads-list').prepend(card)
  cardData.set(id, { url, filename, destination })
}

/**
 * Updates the progress bar, status text, and speed display for a download card.
 * Speed is provided directly by the backend — no client-side delta calculation needed.
 *
 * Args:
 *   id:         The download id.
 *   downloaded: Total bytes received so far.
 *   total:      Total file size in bytes, or null if unknown.
 *   speedBps:   Current download speed in bytes per second from the backend.
 */
export function updateProgress(id, downloaded, total, speedBps) {
  const card = document.querySelector(`.download-card[data-id="${id}"]`)
  if (!card) return

  const fill = card.querySelector('.progress-fill')
  const statusEl = card.querySelector('.card-status')
  const speedEl = card.querySelector('.card-speed')

  if (total) {
    fill.classList.remove('progress-fill--indeterminate')
    fill.style.width = `${Math.round((downloaded / total) * 100)}%`
    statusEl.textContent = `${Math.round((downloaded / total) * 100)}% of ${formatBytes(total)}`
  } else {
    fill.style.width = ''
    fill.classList.add('progress-fill--indeterminate')
    statusEl.textContent = formatBytes(downloaded)
  }
  speedEl.textContent = speedBps > 1024 ? `\u2193 ${formatBytes(speedBps)}/s` : ''
}

/**
 * Marks a download card as complete — green icon, hides progress bar and action buttons.
 *
 * Args:
 *   id:   The download id.
 *   path: Absolute path to the downloaded file on disk.
 */
export function setCardComplete(id, path) {
  const card = document.querySelector(`.download-card[data-id="${id}"]`)
  if (!card) return

  const icon = card.querySelector('.card-type-icon')
  icon.className = 'card-type-icon status-complete'
  icon.innerHTML = '&#10003;'

  card.querySelector('.progress-bar').style.display = 'none'
  card.querySelector('.card-status').textContent = 'Complete'
  card.querySelector('.card-speed').textContent = ''
  card.querySelector('.card-pause').style.display = 'none'
  card.querySelector('.card-resume').style.display = 'none'
  card.querySelector('.card-cancel').style.display = 'none'

  const data = cardData.get(id)
  if (data) data.path = path
}

/**
 * Marks a download card as failed — red icon, shows error message, hides action buttons.
 *
 * Args:
 *   id:      The download id.
 *   message: Human-readable error description.
 */
export function setCardError(id, message) {
  const card = document.querySelector(`.download-card[data-id="${id}"]`)
  if (!card) return

  const icon = card.querySelector('.card-type-icon')
  icon.className = 'card-type-icon status-error'
  icon.innerHTML = '&#10005;'

  card.querySelector('.progress-bar').style.display = 'none'
  card.querySelector('.card-status').textContent = `Error: ${message}`
  card.querySelector('.card-speed').textContent = ''
  card.querySelector('.card-pause').style.display = 'none'
  card.querySelector('.card-resume').style.display = 'none'
  card.querySelector('.card-cancel').style.display = 'none'
}

/**
 * Updates a download card to the paused state — amber icon, "Paused" status text,
 * swaps the pause button for the resume button.
 *
 * Args:
 *   id: The download id.
 */
export function setCardPaused(id) {
  const card = document.querySelector(`.download-card[data-id="${id}"]`)
  if (!card) return

  const icon = card.querySelector('.card-type-icon')
  icon.className = 'card-type-icon status-paused'
  icon.innerHTML = '&#8759;'

  card.querySelector('.card-status').textContent = 'Paused'
  card.querySelector('.card-speed').textContent = ''
  card.querySelector('.card-pause').style.display = 'none'
  card.querySelector('.card-resume').style.display = ''
}

/**
 * Updates a download card from paused back to the downloading state —
 * restores the downloading icon and swaps resume back to pause.
 *
 * Args:
 *   id: The download id.
 */
export function setCardResuming(id) {
  const card = document.querySelector(`.download-card[data-id="${id}"]`)
  if (!card) return

  const icon = card.querySelector('.card-type-icon')
  icon.className = 'card-type-icon status-downloading'
  icon.innerHTML = '&#8595;'

  card.querySelector('.card-status').textContent = 'Resuming\u2026'
  card.querySelector('.card-pause').style.display = ''
  card.querySelector('.card-resume').style.display = 'none'
}

/**
 * Updates a download card to the scanning state — amber indeterminate progress bar,
 * "Scanning…" status text, and hidden action buttons.
 *
 * Args:
 *   id: The download id.
 */
export function setCardScanning(id) {
  const card = document.querySelector(`.download-card[data-id="${id}"]`)
  if (!card) return

  const icon = card.querySelector('.card-type-icon')
  icon.className = 'card-type-icon status-scanning'
  icon.innerHTML = '&#128737;'

  const fill = card.querySelector('.progress-fill')
  fill.classList.add('progress-fill--indeterminate')
  fill.style.width = ''
  card.querySelector('.progress-bar').style.display = ''

  card.querySelector('.card-status').textContent = 'Scanning\u2026'
  card.querySelector('.card-speed').textContent = ''
  card.querySelector('.card-pause').style.display = 'none'
  card.querySelector('.card-resume').style.display = 'none'
  card.querySelector('.card-cancel').style.display = 'none'
}

/**
 * Marks a download card as quarantined — red icon, "Quarantined" status,
 * and an inline reason line.
 *
 * Args:
 *   id:     The download id.
 *   reason: Human-readable threat description from the Shield pipeline.
 */
export function setCardQuarantined(id, reason) {
  const card = document.querySelector(`.download-card[data-id="${id}"]`)
  if (!card) return

  const icon = card.querySelector('.card-type-icon')
  icon.className = 'card-type-icon status-error'
  icon.innerHTML = '&#9888;'

  card.querySelector('.progress-bar').style.display = 'none'
  card.querySelector('.card-status').textContent = 'Quarantined'
  card.querySelector('.card-speed').textContent = ''
  card.querySelector('.card-pause').style.display = 'none'
  card.querySelector('.card-resume').style.display = 'none'
  card.querySelector('.card-cancel').style.display = 'none'

  // Append quarantine reason below status if not already present.
  const meta = card.querySelector('.card-meta')
  if (meta && !meta.querySelector('.card-quarantine-reason')) {
    const reasonEl = document.createElement('span')
    reasonEl.className = 'card-quarantine-reason'
    reasonEl.textContent = reason
    meta.appendChild(reasonEl)
  }
}

/**
 * Populates the right detail panel with metadata for the selected download card.
 * Fetches the full DB record to include scan results (SHA-256, threat, sandbox report).
 * Does nothing if the panel is not expanded.
 *
 * Args:
 *   id: The download id to show details for.
 */
async function showDetail(id) {
  const data = cardData.get(id)
  if (!data) return

  const body = document.querySelector('.detail-body')
  if (!body) return

  // Fetch fresh DB record for scan fields (sha256, scan_threat, sandbox_report).
  let record = null
  try { record = await invoke('get_download_by_id', { id }) } catch { /* no-op */ }

  let scanHtml = ''
  if (record?.sha256) {
    scanHtml += `<dt>SHA-256</dt><dd class="detail-hash">${escHtml(record.sha256)}</dd>`
  }
  if (record?.scan_threat) {
    scanHtml += `<dt>Threat</dt><dd class="status-error">${escHtml(record.scan_threat)}</dd>`
  }
  if (record?.sandbox_report) {
    try {
      const r = JSON.parse(record.sandbox_report)
      scanHtml += `
        <dt>Sandbox</dt><dd>${escHtml(r.verdict)}</dd>
        <dt>Network</dt><dd>${r.network_attempts} attempt(s)</dd>
        <dt>Writes</dt><dd>${r.file_writes} attempt(s)</dd>
        <dt>Blocked</dt><dd>${r.syscalls_blocked} syscall(s)</dd>
      `
    } catch { /* malformed JSON — skip */ }
  }

  body.innerHTML = `
    <dl class="detail-list">
      <dt>File</dt>
      <dd title="${escHtml(data.filename)}">${escHtml(data.filename)}</dd>
      <dt>URL</dt>
      <dd class="detail-url" title="${escHtml(data.url)}">${escHtml(data.url)}</dd>
      <dt>Folder</dt>
      <dd>
        <button class="detail-open-btn" data-path="${escHtml(data.destination)}">
          Open folder &#8599;
        </button>
      </dd>
      ${scanHtml}
    </dl>
  `

  body.querySelector('.detail-open-btn')?.addEventListener('click', async (e) => {
    await open(e.currentTarget.dataset.path)
  })
}

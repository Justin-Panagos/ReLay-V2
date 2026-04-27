/**
 * Downloads tab — live card management.
 * Creates, updates, and removes download cards in response to Tauri events.
 */

import { invoke } from '@tauri-apps/api/tauri'
import { open } from '@tauri-apps/api/shell'
import { formatBytes, escHtml } from './utils.js'
import { t } from './i18n.js'

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

  const statusKey = isPaused ? 'downloads.status.paused' : 'downloads.status.starting'

  const card = document.createElement('div')
  card.className = 'download-card'
  card.dataset.id = id
  card.innerHTML = `
    <span class="card-type-icon ${isPaused ? 'status-paused' : 'status-downloading'}">${isPaused ? '&#8759;' : '&#8595;'}</span>
    <div class="card-body">
      <div class="card-name" title="${escHtml(filename)}">${escHtml(filename)}</div>
      <div class="progress-bar"><div class="progress-fill" style="width:0%"></div></div>
      <div class="card-meta">
        <span class="card-status" data-i18n-key="${statusKey}">${t(statusKey)}</span>
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
    delete statusEl.dataset.i18nKey
  } else {
    fill.style.width = ''
    fill.classList.add('progress-fill--indeterminate')
    statusEl.textContent = formatBytes(downloaded)
    delete statusEl.dataset.i18nKey
  }
  speedEl.textContent = speedBps > 1024 ? `↓ ${formatBytes(speedBps)}/s` : ''
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
  const statusEl = card.querySelector('.card-status')
  statusEl.textContent = t('downloads.status.complete')
  statusEl.dataset.i18nKey = 'downloads.status.complete'
  delete statusEl.dataset.i18nVars
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
  const statusEl = card.querySelector('.card-status')
  statusEl.textContent = t('downloads.status.error', { message })
  statusEl.dataset.i18nKey = 'downloads.status.error'
  statusEl.dataset.i18nVars = JSON.stringify({ message })
  card.querySelector('.card-speed').textContent = ''
  card.querySelector('.card-pause').style.display = 'none'
  card.querySelector('.card-resume').style.display = 'none'
  card.querySelector('.card-cancel').style.display = 'none'
}

/**
 * Updates a download card to the paused state — amber icon, paused status text,
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

  const statusEl = card.querySelector('.card-status')
  statusEl.textContent = t('downloads.status.paused')
  statusEl.dataset.i18nKey = 'downloads.status.paused'
  delete statusEl.dataset.i18nVars
  card.querySelector('.card-speed').textContent = ''
  card.querySelector('.card-pause').style.display = 'none'
  card.querySelector('.card-resume').style.display = ''
}

/**
 * Updates a download card to the retrying state — amber icon, retrying status,
 * and shows a small retry attempt badge on the card.
 *
 * Args:
 *   id:      The download id.
 *   attempt: Which retry attempt is starting (1–4).
 */
export function setCardRetrying(id, attempt) {
  const card = document.querySelector(`.download-card[data-id="${id}"]`)
  if (!card) return

  const icon = card.querySelector('.card-type-icon')
  icon.className = 'card-type-icon status-paused'
  icon.innerHTML = '&#8635;'

  const statusEl = card.querySelector('.card-status')
  statusEl.textContent = t('downloads.status.retrying', { attempt })
  statusEl.dataset.i18nKey = 'downloads.status.retrying'
  statusEl.dataset.i18nVars = JSON.stringify({ attempt })
  card.querySelector('.card-speed').textContent = ''
  card.querySelector('.card-pause').style.display = 'none'
  card.querySelector('.card-resume').style.display = 'none'

  let badge = card.querySelector('.card-retry-badge')
  if (!badge) {
    badge = document.createElement('span')
    badge.className = 'card-retry-badge'
    card.querySelector('.card-body').appendChild(badge)
  }
  badge.textContent = attempt
}

/**
 * Removes the retry badge from a download card.
 * Called on complete or final error to clear the retry indicator.
 *
 * Args:
 *   id: The download id.
 */
export function clearRetryBadge(id) {
  const card = document.querySelector(`.download-card[data-id="${id}"]`)
  if (!card) return
  card.querySelector('.card-retry-badge')?.remove()
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

  const statusEl = card.querySelector('.card-status')
  statusEl.textContent = t('downloads.status.resuming')
  statusEl.dataset.i18nKey = 'downloads.status.resuming'
  delete statusEl.dataset.i18nVars
  card.querySelector('.card-pause').style.display = ''
  card.querySelector('.card-resume').style.display = 'none'
}

/**
 * Updates a download card to the scanning state — amber indeterminate progress bar,
 * scanning status text, and hidden action buttons.
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

  const statusEl = card.querySelector('.card-status')
  statusEl.textContent = t('downloads.status.scanning')
  statusEl.dataset.i18nKey = 'downloads.status.scanning'
  delete statusEl.dataset.i18nVars
  card.querySelector('.card-speed').textContent = ''
  card.querySelector('.card-pause').style.display = 'none'
  card.querySelector('.card-resume').style.display = 'none'
  card.querySelector('.card-cancel').style.display = 'none'
}

/**
 * Marks a download card as quarantined — red icon, quarantined status,
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
  const statusEl = card.querySelector('.card-status')
  statusEl.textContent = t('downloads.status.quarantined')
  statusEl.dataset.i18nKey = 'downloads.status.quarantined'
  delete statusEl.dataset.i18nVars
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
 * Makes a download card draggable (Pro queue reorder).
 * Adds draggable="true" and a visual drag handle indicator.
 *
 * Args:
 *   id: The download id.
 */
export function enableCardDrag(id) {
  const card = document.querySelector(`.download-card[data-id="${id}"]`)
  if (!card) return
  card.setAttribute('draggable', 'true')
  card.classList.add('card-draggable')
}

/**
 * Injects schedule time inputs into a download card (Pro scheduling).
 * Shows start/end time fields; invokes set_download_schedule on change.
 *
 * Args:
 *   id:       The download id.
 *   startVal: Pre-populated start time "HH:MM", or empty string.
 *   endVal:   Pre-populated end time "HH:MM", or empty string.
 */
export function addScheduleInputs(id, startVal = '', endVal = '') {
  const card = document.querySelector(`.download-card[data-id="${id}"]`)
  if (!card || card.querySelector('.card-schedule')) return

  const row = document.createElement('div')
  row.className = 'card-schedule'
  row.innerHTML = `
    <span class="card-schedule-label">Schedule <span class="card-schedule-utc">(UTC)</span></span>
    <input type="time" class="card-sched-start" title="Start time (UTC)">
    <span class="card-schedule-sep">–</span>
    <input type="time" class="card-sched-end" title="End time (UTC)">
  `

  const startEl = row.querySelector('.card-sched-start')
  const endEl   = row.querySelector('.card-sched-end')
  if (startVal) startEl.value = startVal
  if (endVal)   endEl.value   = endVal

  let schedTimer = null

  function onChange() {
    clearTimeout(schedTimer)
    schedTimer = setTimeout(async () => {
      const s = startEl.value || null
      const e = endEl.value || null
      try { await invoke('set_download_schedule', { id, start: s, end: e }) } catch { /* no-op */ }
    }, 500)
  }

  startEl.addEventListener('change', onChange)
  endEl.addEventListener('change', onChange)

  card.querySelector('.card-body').appendChild(row)
}

/**
 * Injects a bandwidth limit slider into a download card (Pro only).
 * Slider range: 0 (unlimited) to 50 000 kbps. Debounced 400 ms.
 * Invokes set_download_bandwidth on change; shows current value as a label.
 *
 * Args:
 *   id:          The download id.
 *   initialKbps: Pre-populated kbps value (0 = unlimited).
 */
export function addBandwidthSlider(id, initialKbps = 0) {
  const card = document.querySelector(`.download-card[data-id="${id}"]`)
  if (!card || card.querySelector('.card-bandwidth')) return

  const row = document.createElement('div')
  row.className = 'card-bandwidth'
  row.innerHTML = `
    <span class="card-bandwidth-label">${t('downloads.bandwidth_label')}</span>
    <input type="range" class="card-bw-slider" min="0" max="50000" step="100" value="${initialKbps}">
    <span class="card-bw-value">${initialKbps === 0 ? t('downloads.bandwidth_unlimited') : initialKbps + ' kbps'}</span>
  `

  const slider = row.querySelector('.card-bw-slider')
  const valueEl = row.querySelector('.card-bw-value')
  let bwTimer = null

  slider.addEventListener('input', () => {
    const kbps = parseInt(slider.value, 10)
    valueEl.textContent = kbps === 0 ? t('downloads.bandwidth_unlimited') : `${kbps} kbps`
    clearTimeout(bwTimer)
    bwTimer = setTimeout(async () => {
      try { await invoke('set_download_bandwidth', { id, kbps }) } catch { /* no-op */ }
    }, 400)
  })

  card.querySelector('.card-body').appendChild(row)
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

  let record = null
  try { record = await invoke('get_download_by_id', { id }) } catch { /* no-op */ }

  let scanHtml = ''
  if (record?.sha256) {
    scanHtml += `<dt>${t('detail.sha256')}</dt><dd class="detail-hash">${escHtml(record.sha256)}</dd>`
  }
  if (record?.scan_threat) {
    scanHtml += `<dt>${t('detail.threat')}</dt><dd class="status-error">${escHtml(record.scan_threat)}</dd>`
  }
  if (record?.sandbox_report) {
    try {
      const r = JSON.parse(record.sandbox_report)
      scanHtml += `
        <dt>${t('detail.sandbox')}</dt><dd>${escHtml(r.verdict)}</dd>
        <dt>${t('detail.network')}</dt><dd>${r.network_attempts} ${t('detail.attempts')}</dd>
        <dt>${t('detail.writes')}</dt><dd>${r.file_writes} ${t('detail.attempts')}</dd>
        <dt>${t('detail.blocked')}</dt><dd>${r.syscalls_blocked} ${t('detail.syscalls')}</dd>
      `
    } catch { /* malformed JSON — skip */ }
  }

  body.innerHTML = `
    <dl class="detail-list">
      <dt>${t('detail.file')}</dt>
      <dd title="${escHtml(data.filename)}">${escHtml(data.filename)}</dd>
      <dt>${t('detail.url')}</dt>
      <dd class="detail-url" title="${escHtml(data.url)}">${escHtml(data.url)}</dd>
      <dt>${t('detail.folder')}</dt>
      <dd>
        <button class="detail-open-btn" data-path="${escHtml(data.destination)}">
          ${t('detail.open_folder')}
        </button>
      </dd>
      ${scanHtml}
    </dl>
  `

  body.querySelector('.detail-open-btn')?.addEventListener('click', async (e) => {
    await open(e.currentTarget.dataset.path)
  })
}

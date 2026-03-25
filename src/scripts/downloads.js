/**
 * Downloads tab — live card management.
 * Creates, updates, and removes download cards in response to Tauri events.
 */

import { open } from '@tauri-apps/api/shell'
import { formatBytes, escHtml } from './utils.js'

/** @type {Map<number, { url: string, filename: string, destination: string, path?: string }>} */
const cardData = new Map()

/**
 * Creates and inserts a download card into the Downloads tab.
 * Hides the empty state. Stores card metadata for speed calculation and detail panel.
 *
 * Args:
 *   id:          The download id from the backend.
 *   filename:    The filename being downloaded.
 *   url:         The source URL.
 *   destination: The destination directory path.
 */
export function addDownloadCard(id, filename, url, destination) {
  document.getElementById('downloads-empty').classList.add('hidden')

  const card = document.createElement('div')
  card.className = 'download-card'
  card.dataset.id = id
  card.innerHTML = `
    <span class="card-type-icon status-downloading">&#8595;</span>
    <div class="card-body">
      <div class="card-name" title="${escHtml(filename)}">${escHtml(filename)}</div>
      <div class="progress-bar"><div class="progress-fill" style="width:0%"></div></div>
      <div class="card-meta">
        <span class="card-status">Starting&#8230;</span>
        <span class="card-speed"></span>
      </div>
    </div>
    <div class="card-actions">
      <button class="btn-icon card-cancel" title="Cancel">&#10005;</button>
    </div>
  `

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
 * Marks a download card as complete — green icon, hides progress bar.
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
  card.querySelector('.card-cancel').style.display = 'none'

  const data = cardData.get(id)
  if (data) data.path = path
}

/**
 * Marks a download card as failed — red icon, shows error message.
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
  card.querySelector('.card-cancel').style.display = 'none'
}

/**
 * Populates the right detail panel with metadata for the selected download card.
 * Does nothing if the panel is not expanded.
 *
 * Args:
 *   id: The download id to show details for.
 */
function showDetail(id) {
  const data = cardData.get(id)
  if (!data) return

  const body = document.querySelector('.detail-body')
  if (!body) return

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
    </dl>
  `

  body.querySelector('.detail-open-btn')?.addEventListener('click', async (e) => {
    await open(e.currentTarget.dataset.path)
  })
}


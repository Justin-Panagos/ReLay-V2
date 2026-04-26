/**
 * Torrent tab — card management and .torrent file modal.
 * Creates, updates, and transitions torrent cards in response to Tauri events.
 * Handles drag-and-drop of .torrent files and the file-selection modal.
 */

import { invoke } from '@tauri-apps/api/tauri'
import { appWindow } from '@tauri-apps/api/window'
import { formatBytes, escHtml } from './utils.js'
import { showErrorToast } from './toast.js'

/** Maximum number of torrent records to fetch from the database on load. */
const TORRENT_LOAD_LIMIT = 50

/** @type {Map<number, { name: string, destination: string, fileBytes?: number[] }>} */
const torrentCardData = new Map()

/** Raw bytes of the .torrent file currently shown in the modal.
 * @type {number[] | null} */
let pendingFileBytes = null

// ── Card lifecycle ────────────────────────────────────────────────────────────

/**
 * Creates and inserts a torrent card into the Torrent tab.
 * Hides the empty state. Stores card metadata. Wires pause, resume, cancel handlers.
 *
 * Args:
 *   id:          The DB download id.
 *   name:        Display name for the torrent (magnet URI or resolved torrent name).
 *   destination: Save directory path.
 */
export function addTorrentCard(id, name, destination = '') {
  document.getElementById('torrents-empty').classList.add('hidden')

  const shortName = name.startsWith('magnet:') ? 'Resolving magnet\u2026' : escHtml(name)

  const card = document.createElement('div')
  card.className = 'download-card'
  card.dataset.id = id
  card.dataset.type = 'torrent'
  card.innerHTML = `
    <span class="card-type-icon status-downloading">&#8595;</span>
    <div class="card-body">
      <div class="card-name" title="${escHtml(name)}">${shortName}</div>
      <div class="progress-bar"><div class="progress-fill progress-fill--indeterminate"></div></div>
      <div class="card-meta">
        <span class="card-status">Initializing\u2026</span>
        <span class="card-speed"></span>
      </div>
      <div class="card-torrent-meta">
        <span class="card-peers">0 peers</span>
        <span class="card-ratio">ratio: 0.00</span>
      </div>
    </div>
    <div class="card-actions">
      <button class="btn-icon card-pause" title="Pause">&#9646;&#9646;</button>
      <button class="btn-icon card-resume" title="Resume" style="display:none">&#9654;</button>
      <button class="btn-icon card-cancel" title="Cancel">&#10005;</button>
      <div class="card-seed-menu hidden">
        <button class="btn-icon card-seed-toggle" title="Options">&#8942;</button>
        <div class="card-seed-dropdown hidden">
          <button class="card-seed-stop">Stop seeding</button>
        </div>
      </div>
    </div>
  `

  card.querySelector('.card-pause').addEventListener('click', async (e) => {
    e.stopPropagation()

    try {
      await invoke('pause_torrent', { id })
    } catch (err) {
      console.error(`pause_torrent(${id}) failed:`, err)
      showErrorToast(`Pause failed: ${err}`)
    }
  })

  card.querySelector('.card-resume').addEventListener('click', async (e) => {
    e.stopPropagation()

    try {
      await invoke('resume_torrent', { id })
      setTorrentCardResuming(id)
    } catch (err) {
      console.error(`resume_torrent(${id}) failed:`, err)
      showErrorToast(`Resume failed: ${err}`)
    }
  })

  card.querySelector('.card-cancel').addEventListener('click', async (e) => {
    e.stopPropagation()
    try {
      await invoke('cancel_torrent', { id })
    } catch (err) {
      console.error(`cancel_torrent(${id}) failed:`, err)
      showErrorToast(`Cancel failed: ${err}`)
    }
  })

  card.querySelector('.card-seed-toggle').addEventListener('click', (e) => {
    e.stopPropagation()
    card.querySelector('.card-seed-dropdown').classList.toggle('hidden')
  })

  card.querySelector('.card-seed-stop').addEventListener('click', async (e) => {
    e.stopPropagation()
    card.querySelector('.card-seed-dropdown').classList.add('hidden')

    try {
      await invoke('cancel_torrent', { id })
    } catch (err) {
      console.error(`cancel_torrent(stop-seed)(${id}) failed:`, err)
      showErrorToast(`Stop seeding failed: ${err}`)
    }
  })

  document.getElementById('torrents-list').prepend(card)
  torrentCardData.set(id, { name, destination })
}

/**
 * Updates the progress bar, speed, peer count, ratio, and name for a torrent card.
 *
 * Args:
 *   id:         The DB download id.
 *   downloaded: Bytes downloaded and verified so far.
 *   total:      Total bytes (0 if unknown).
 *   peers:      Number of fully connected live peers.
 *   connecting: Number of peers currently connecting (queued + connecting).
 *   ratio:      Upload ratio (uploaded / downloaded).
 *   speedBps:   Current download speed in bytes per second.
 *   state:      Torrent state string ("initializing" | "live" | "paused" | "error").
 *   name:       Resolved torrent name, or null while resolving.
 */
export function updateTorrentProgress(id, downloaded, total, peers, connecting, ratio, speedBps, state, name) {
  const card = document.querySelector(`.download-card[data-id="${id}"][data-type="torrent"]`)
  if (!card) return

  const fill = card.querySelector('.progress-fill')
  const statusEl = card.querySelector('.card-status')
  const speedEl = card.querySelector('.card-speed')
  const peersEl = card.querySelector('.card-peers')
  const ratioEl = card.querySelector('.card-ratio')
  const nameEl = card.querySelector('.card-name')

  // Update card name once metadata resolves.
  if (name && nameEl && nameEl.textContent === 'Resolving magnet\u2026') {
    nameEl.textContent = escHtml(name)
    nameEl.title = escHtml(name)
  }

  if (total > 0) {
    fill.classList.remove('progress-fill--indeterminate')
    fill.style.width = `${Math.round((downloaded / total) * 100)}%`
    statusEl.textContent = `${Math.round((downloaded / total) * 100)}% of ${formatBytes(total)}`
  } else {
    fill.style.width = ''
    fill.classList.add('progress-fill--indeterminate')
    if (state === 'initializing') {
      statusEl.textContent = 'Resolving metadata\u2026'
    } else if (state === 'live') {
      statusEl.textContent = 'Connecting\u2026'
    } else {
      statusEl.textContent = formatBytes(downloaded)
    }
  }

  speedEl.textContent = speedBps > 1024 ? `\u2193 ${formatBytes(speedBps)}/s` : ''

  // Show connecting count during init so user can see DHT activity.
  const totalPeerActivity = peers + connecting
  if (peers > 0) {
    peersEl.textContent = `${peers} peer${peers !== 1 ? 's' : ''}`
  } else if (connecting > 0) {
    peersEl.textContent = `${connecting} connecting\u2026`
  } else {
    peersEl.textContent = totalPeerActivity === 0 ? 'finding peers\u2026' : `${totalPeerActivity} peers`
  }

  ratioEl.textContent = `ratio: ${ratio.toFixed(2)}`
}

/**
 * Marks a torrent card as complete (seeding state) — green icon, "Seeding" status.
 * Hides pause/cancel buttons, shows a stop-seeding option.
 *
 * Args:
 *   id:   The DB download id.
 *   path: Destination folder path.
 */
export function setTorrentCardComplete(id, path) {
  const card = document.querySelector(`.download-card[data-id="${id}"][data-type="torrent"]`)
  if (!card) return

  const icon = card.querySelector('.card-type-icon')
  icon.className = 'card-type-icon status-complete'
  icon.innerHTML = '&#8593;'

  card.querySelector('.progress-bar').style.display = 'none'
  card.querySelector('.card-status').textContent = 'Seeding'
  card.querySelector('.card-speed').textContent = ''
  card.querySelector('.card-pause').style.display = 'none'
  card.querySelector('.card-resume').style.display = 'none'
  card.querySelector('.card-cancel').style.display = 'none'
  card.querySelector('.card-seed-menu').classList.remove('hidden')

  const data = torrentCardData.get(id)
  if (data) data.destination = path
}

/**
 * Marks a torrent card as failed — red icon, error message, hides all action buttons.
 *
 * Args:
 *   id:      The DB download id.
 *   message: Human-readable error description.
 */
export function setTorrentCardError(id, message) {
  const card = document.querySelector(`.download-card[data-id="${id}"][data-type="torrent"]`)
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
 * Transitions a torrent card to the paused state — amber icon, "Paused" status,
 * swaps the pause button for the resume button.
 *
 * Args:
 *   id: The DB download id.
 */
export function setTorrentCardPaused(id) {
  const card = document.querySelector(`.download-card[data-id="${id}"][data-type="torrent"]`)
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
 * Transitions a paused torrent card back to the downloading state after resume.
 *
 * Args:
 *   id: The DB download id.
 */
function setTorrentCardResuming(id) {
  const card = document.querySelector(`.download-card[data-id="${id}"][data-type="torrent"]`)
  if (!card) return

  const icon = card.querySelector('.card-type-icon')
  icon.className = 'card-type-icon status-downloading'
  icon.innerHTML = '&#8595;'

  card.querySelector('.card-status').textContent = 'Resuming\u2026'
  card.querySelector('.card-pause').style.display = ''
  card.querySelector('.card-resume').style.display = 'none'
}

// ── Event subscriptions ───────────────────────────────────────────────────────

/**
 * Wires up Tauri event listeners for a torrent's progress, complete, and error events.
 * Each listener is unlisten-able; listeners for complete and error self-clean after firing.
 *
 * Args:
 *   id: The DB download id to subscribe to.
 */
export async function subscribeToTorrentEvents(id) {
  const unlistenProgress = await appWindow.listen(
    `torrent://progress/${id}`,
    ({ payload }) => {
      updateTorrentProgress(
        id,
        payload.downloaded,
        payload.total,
        payload.peers,
        payload.connecting,
        payload.ratio,
        payload.speed_bps,
        payload.state,
        payload.name ?? null,
      )
      if (payload.state === 'paused') setTorrentCardPaused(id)
    },
  )

  const unlistenComplete = await appWindow.listen(
    `torrent://complete/${id}`,
    ({ payload }) => {
      setTorrentCardComplete(id, payload.path)
      unlistenProgress()
      unlistenComplete()
    },
  )

  const unlistenError = await appWindow.listen(
    `torrent://error/${id}`,
    ({ payload }) => {
      setTorrentCardError(id, payload.message)
      unlistenProgress()
      unlistenComplete()
      unlistenError()
    },
  )
}

// ── Load persisted torrents on startup ───────────────────────────────────────

/**
 * Loads paused torrent rows from the database and renders them as cards.
 * Called during app init alongside loadPausedDownloads().
 */
export async function loadTorrents() {
  let rows
  try {
    rows = await invoke('get_torrents', { limit: TORRENT_LOAD_LIMIT })
  } catch {
    return
  }

  for (const row of rows) {
    if (row.status === 'paused' || row.status === 'downloading') {
      addTorrentCard(row.id, row.filename, row.destination)
      if (row.status === 'paused') setTorrentCardPaused(row.id)
      await subscribeToTorrentEvents(row.id)
    }
  }
}

// ── .torrent file drag-and-drop + modal ──────────────────────────────────────

/**
 * Reads a File object as a byte array and opens the torrent file-selection modal.
 * Calls list_torrent_files to populate the file list from the backend.
 *
 * Args:
 *   file: A browser File object from a drag-and-drop event.
 */
export async function openTorrentModal(file) {
  const buffer = await file.arrayBuffer()
  pendingFileBytes = Array.from(new Uint8Array(buffer))

  let entries
  try {
    entries = await invoke('list_torrent_files', { fileBytes: pendingFileBytes })
  } catch (err) {
    console.error('list_torrent_files failed:', err)
    pendingFileBytes = null
    return
  }

  const listEl = document.getElementById('torrent-file-list')
  listEl.innerHTML = ''

  for (const entry of entries) {
    const item = document.createElement('label')
    item.className = 'torrent-file-item'
    item.innerHTML = `
      <input type="checkbox" class="torrent-file-check" value="${entry.index}" checked />
      <span class="torrent-file-name" title="${escHtml(entry.name)}">${escHtml(entry.name)}</span>
      <span class="torrent-file-size">${formatBytes(entry.size_bytes)}</span>
    `
    listEl.appendChild(item)
  }

  // Pre-fill destination with default folder.
  let defaultFolder = '.'
  try {
    defaultFolder = (await invoke('get_setting', { key: 'default_folder' })) ?? '.'
  } catch { /* use fallback */ }
  document.getElementById('torrent-dest-input').value = defaultFolder

  document.getElementById('torrent-modal').classList.remove('hidden')
}

/**
 * Initialises drag-and-drop listeners on the Torrent tab and wires the modal
 * confirm / cancel buttons. Called once on app init.
 */
export function initTorrentDrop() {
  const tabEl = document.getElementById('tab-torrent')

  tabEl.addEventListener('dragover', (e) => {
    e.preventDefault()
    tabEl.classList.add('drag-over')
  })

  tabEl.addEventListener('dragleave', () => {
    tabEl.classList.remove('drag-over')
  })

  tabEl.addEventListener('drop', async (e) => {
    e.preventDefault()
    tabEl.classList.remove('drag-over')
    const file = e.dataTransfer?.files?.[0]
    if (!file || !file.name.endsWith('.torrent')) return
    await openTorrentModal(file)
  })

  // Modal close / cancel.
  document.getElementById('torrent-modal-close').addEventListener('click', closeModal)
  document.getElementById('torrent-modal-cancel').addEventListener('click', closeModal)

  // Modal confirm — start the torrent download.
  document.getElementById('torrent-modal-confirm').addEventListener('click', async () => {
    if (!pendingFileBytes) return

    const checks = document.querySelectorAll('.torrent-file-check:checked')
    const onlyFiles = Array.from(checks).map((c) => Number(c.value))
    const destination = document.getElementById('torrent-dest-input').value.trim() || '.'

    let id
    try {
      id = await invoke('add_torrent_file', {
        fileBytes: pendingFileBytes,
        destination,
        onlyFiles,
      })
    } catch (err) {
      console.error('add_torrent_file failed:', err)
      closeModal()
      return
    }

    closeModal()

    // Switch to torrent tab.
    document.querySelectorAll('.tab').forEach((t) => t.classList.remove('active'))
    document.querySelectorAll('.tab-pane').forEach((p) => p.classList.remove('active'))
    document.querySelector('.tab[data-tab="torrent"]').classList.add('active')
    document.getElementById('tab-torrent').classList.add('active')

    addTorrentCard(id, 'Torrent download', destination)
    await subscribeToTorrentEvents(id)
  })
}

/**
 * Closes the torrent file-selection modal and clears pending state.
 */
function closeModal() {
  document.getElementById('torrent-modal').classList.add('hidden')
  document.getElementById('torrent-file-list').innerHTML = ''
  pendingFileBytes = null
}

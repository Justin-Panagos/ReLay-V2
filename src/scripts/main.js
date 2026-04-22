/**
 * ReLay app entry point.
 * Coordinates download lifecycle, tab switching, expand/collapse, and slider drag.
 * Delegates card management to downloads.js and history rendering to history.js.
 */

import { invoke } from '@tauri-apps/api/tauri'
import { listen } from '@tauri-apps/api/event'
import { getCurrent, PhysicalSize } from '@tauri-apps/api/window'
import { showErrorToast } from './toast.js'
import {
  addDownloadCard,
  updateProgress,
  setCardComplete,
  setCardError,
  setCardPaused,
  setCardResuming,
  setCardScanning,
  setCardQuarantined,
} from './downloads.js'
import { loadHistory } from './history.js'
import {
  addTorrentCard,
  updateTorrentProgress,
  setTorrentCardComplete,
  setTorrentCardError,
  setTorrentCardPaused,
  subscribeToTorrentEvents,
  loadTorrents,
  initTorrentDrop,
} from './torrent.js'
import { loadQuarantine } from './quarantine.js'
import { initSettings } from './settings.js'
import { initCommunity, loadCommunity } from './community.js'
import { initDeveloper } from './developer.js'

const urlInput = document.getElementById('url-input')
const startBtn = document.getElementById('start-btn')
const proPrompt = document.getElementById('pro-prompt')

/** Minimum and maximum width of the resizable detail panel in pixels. */
const PANEL_MIN_WIDTH = 180
const PANEL_MAX_WIDTH = 500

/** Maximum number of downloads to fetch when restoring paused state on startup. */
const PAUSED_LOAD_LIMIT = 200

/** Set of download IDs currently active (downloading or queued) in this session. */
const activeDownloads = new Set()

/** Holds the unlisten function for the icp://status event. Stored at module scope
 *  so it can be called if init() is ever re-invoked, preventing duplicate listeners. */
let icpStatusUnlisten = () => {}

// ── Startup ──────────────────────────────────────────────────────────────────

/**
 * Resets downloads interrupted by the previous session, then loads history
 * and any downloads that were paused when the app was last closed.
 * reset_stale_downloads is non-fatal — app continues even if it fails.
 */
async function init() {
  try {
    await invoke('reset_stale_downloads')
  } catch { /* non-fatal */ }

  // Wire the ICP offline banner.
  const banner    = document.getElementById('icp-banner')
  const bannerMsg = document.getElementById('icp-banner-msg')
  document.getElementById('icp-banner-close')?.addEventListener('click', () => {
    banner?.classList.add('hidden')
  })
  // Store the unlisten handle at module scope — prevents duplicate listeners
  // if init() is ever called more than once, and allows explicit cleanup.
  try {
    icpStatusUnlisten = await listen('icp://status', ({ payload }) => {
      if (!banner || !bannerMsg) return
      if (payload.connected) {
        banner.classList.add('hidden')
      } else {
        bannerMsg.textContent = payload.message ?? 'ICP network unreachable — licence and pattern updates paused.'
        banner.classList.remove('hidden')
      }
    })
  } catch { /* non-fatal if event system not ready */ }

  initTorrentDrop()
  initCommunity()
  initDeveloper()
  await Promise.all([loadHistory(), loadPausedDownloads(), loadTorrents(), loadQuarantine(), initSettings(), loadCommunity()])
}

init()

// ── Event subscription helper ─────────────────────────────────────────────────

/**
 * Subscribes to all backend events for a download id and wires up card updates.
 * Returns a cleanup function that removes all active listeners.
 * All unlisten variables are initialised as no-ops so cleanup() is always safe
 * to call even if some listen() calls failed partway through.
 *
 * Args:
 *   id: The download id to subscribe to.
 */
async function subscribeToDownloadEvents(id) {
  let unlistenProgress   = () => {}
  let unlistenComplete   = () => {}
  let unlistenError      = () => {}
  let unlistenPaused     = () => {}
  let unlistenScan       = () => {}
  let unlistenQuarantine = () => {}

  /** Removes all event listeners for this download. */
  function cleanup() {
    unlistenProgress()
    unlistenComplete()
    unlistenError()
    unlistenPaused()
    unlistenScan()
    unlistenQuarantine()
  }

  try {
    unlistenProgress = await listen(`download://progress/${id}`, (e) => {
      updateProgress(id, e.payload.downloaded, e.payload.total ?? null, e.payload.speed_bps ?? 0)
    })

    unlistenComplete = await listen(`download://complete/${id}`, async (e) => {
      setCardComplete(id, e.payload.path)
      activeDownloads.delete(id)
      cleanup()
      await loadHistory()
    })

    unlistenError = await listen(`download://error/${id}`, async (e) => {
      setCardError(id, e.payload.message)
      activeDownloads.delete(id)
      cleanup()
      await loadHistory()
    })

    unlistenPaused = await listen(`download://paused/${id}`, async () => {
      setCardPaused(id)
      activeDownloads.delete(id)
      cleanup()
    })

    unlistenScan = await listen(`download://scan/${id}`, () => {
      setCardScanning(id)
    })

    unlistenQuarantine = await listen(`download://quarantine/${id}`, async (e) => {
      setCardQuarantined(id, e.payload.reason)
      activeDownloads.delete(id)
      cleanup()
      // Switch to the Quarantine tab so the user sees the threat immediately.
      document.querySelectorAll('.tab').forEach(t => t.classList.remove('active'))
      document.querySelectorAll('.tab-pane').forEach(p => p.classList.remove('active'))
      const quarTab = document.querySelector('.tab[data-tab="quarantine"]')
      if (quarTab) quarTab.classList.add('active')
      const quarPane = document.getElementById('tab-quarantine')
      if (quarPane) quarPane.classList.add('active')
      await loadQuarantine()
    })
  } catch (err) {
    console.error(`Failed to subscribe to download events for ${id}:`, err)
    // Any listeners that did register before the failure are cleaned up correctly.
  }

  return cleanup
}

// ── Download start ────────────────────────────────────────────────────────────

/**
 * Starts a download for the given URL.
 * The backend handles queuing — if a slot is free the download starts immediately,
 * otherwise it is queued and auto-starts when the current download finishes.
 *
 * Args:
 *   url: The URL string entered by the user.
 */
async function startDownload(url) {
  // Route magnet links to the torrent engine.
  if (url.startsWith('magnet:')) {
    await startMagnet(url)
    return
  }

  proPrompt.classList.add('hidden')

  // Derive filename client-side so the card is shown before the backend responds.
  const filename = url.split('/').filter(Boolean).pop()?.split('?')[0] || 'download'
  const destination = (await invoke('get_setting', { key: 'default_folder' })) ?? ''

  let id
  try {
    id = await invoke('start_download', { url })
  } catch (err) {
    console.error('start_download failed:', err)
    showErrorToast(`Download failed to start: ${err}`)
    return
  }

  activeDownloads.add(id)
  addDownloadCard(id, filename, url, destination, { onResume: resumeDownload })
  await subscribeToDownloadEvents(id)
}

/**
 * Starts a magnet link download via the torrent engine.
 * Switches to the Torrent tab and adds a card that tracks progress.
 *
 * Args:
 *   magnet: The magnet URI string.
 */
async function startMagnet(magnet) {
  let id
  try {
    id = await invoke('add_magnet', { magnet })
  } catch (err) {
    console.error('add_magnet failed:', err)
    return
  }

  // Switch to the Torrent tab.
  document.querySelectorAll('.tab').forEach((t) => t.classList.remove('active'))
  document.querySelectorAll('.tab-pane').forEach((p) => p.classList.remove('active'))
  document.querySelector('.tab[data-tab="torrent"]').classList.add('active')
  document.getElementById('tab-torrent').classList.add('active')

  addTorrentCard(id, magnet)
  await subscribeToTorrentEvents(id)
}

// ── Download resume ───────────────────────────────────────────────────────────

/**
 * Resumes a previously paused download.
 * Tells the backend to restart the task and re-subscribes to progress events.
 *
 * Args:
 *   id: The download id to resume.
 */
async function resumeDownload(id) {
  try {
    await invoke('resume_download', { id })
  } catch (err) {
    console.error('resume_download failed:', err)
    showErrorToast(`Resume failed: ${err}`)
    return
  }

  setCardResuming(id)
  activeDownloads.add(id)
  await subscribeToDownloadEvents(id)
}

// ── Load paused downloads ─────────────────────────────────────────────────────

/**
 * Loads all paused downloads from the database and renders them in the active
 * downloads list with the resume button visible. Called during app init so
 * paused downloads survive app restarts.
 */
async function loadPausedDownloads() {
  let records
  try {
    records = await invoke('get_downloads', { limit: PAUSED_LOAD_LIMIT })
  } catch {
    return
  }

  const paused = records.filter(r => r.status === 'paused')
  for (const r of paused) {
    addDownloadCard(r.id, r.filename, r.url, r.destination, {
      isPaused: true,
      onResume: resumeDownload,
    })
    await subscribeToDownloadEvents(r.id)
  }
}

// ── UI event bindings ─────────────────────────────────────────────────────────

// ── Theme ─────────────────────────────────────────────────────────────────────

/**
 * Applies the given theme to the document root.
 * "auto" removes the data-theme attribute so CSS @media prefers-color-scheme takes over.
 * "light" or "dark" set the attribute to force the chosen scheme.
 *
 * Args:
 *   theme: "auto" | "light" | "dark"
 */
export function applyTheme(theme) {
  if (theme === 'auto') {
    document.documentElement.removeAttribute('data-theme')
  } else {
    document.documentElement.setAttribute('data-theme', theme)
  }
}

invoke('get_setting', { key: 'theme' }).then(t => applyTheme(t ?? 'auto')).catch(() => {})

// ── Error reporting ───────────────────────────────────────────────────────────

/**
 * Shows an error toast and appends the failure to the on-disk error log.
 * Use instead of showErrorToast for user-triggered action failures.
 *
 * Args:
 *   action: Short label of the action that failed (e.g. "resume_download").
 *   err:    The caught error value.
 */
export async function reportError(action, err) {
  showErrorToast(`${action} failed: ${err}`)
  invoke('log_error', { action, message: String(err) }).catch(() => {})
}

// ── UI event bindings ─────────────────────────────────────────────────────────

startBtn.addEventListener('click', () => {
  const url = urlInput.value.trim()
  if (url) startDownload(url)
})

urlInput.addEventListener('keydown', (e) => {
  if (e.key === 'Enter') {
    const url = urlInput.value.trim()
    if (url) startDownload(url)
  }
})

document.getElementById('folder-btn').addEventListener('click', async () => {
  const btn = document.getElementById('folder-btn')
  const original = btn.textContent
  btn.disabled = true
  btn.textContent = 'Choosing\u2026'
  const path = await invoke('pick_folder').catch(() => null)
  btn.disabled = false
  btn.textContent = original
  if (!path) return
  await invoke('set_setting', { key: 'default_folder', value: path }).catch(() => {})
  btn.textContent = '\u2713 Saved'
  setTimeout(() => { btn.textContent = original }, 2000)
})

proPrompt.querySelector('.pro-prompt-dismiss')?.addEventListener('click', () => {
  proPrompt.classList.add('hidden')
})

proPrompt.querySelector('.pro-prompt-upgrade')?.addEventListener('click', async () => {
  const email = document.getElementById('pro-email-input')?.value.trim() ?? ''
  if (!email) {
    // Route to Settings so the user can enter their email first.
    document.querySelectorAll('.tab').forEach(t => t.classList.remove('active'))
    document.querySelectorAll('.tab-pane').forEach(p => p.classList.remove('active'))
    document.querySelector('.tab[data-tab="settings"]')?.classList.add('active')
    document.getElementById('tab-settings')?.classList.add('active')
    document.getElementById('pro-email-input')?.focus()
    proPrompt.classList.add('hidden')
    return
  }
  await invoke('open_upgrade_page', { email }).catch(err => {
    console.error('open_upgrade_page:', err)
    showErrorToast(`Could not open upgrade page: ${err}`)
  })
})

// ── Tab switching ─────────────────────────────────────────────────────────────

/**
 * Switches the visible tab pane by toggling the active class on the button and pane.
 */
document.querySelectorAll('.tab').forEach(tab => {
  tab.addEventListener('click', () => {
    document.querySelectorAll('.tab').forEach(t => t.classList.remove('active'))
    document.querySelectorAll('.tab-pane').forEach(p => p.classList.remove('active'))
    document.getElementById('settings-btn')?.classList.remove('active')
    tab.classList.add('active')
    document.getElementById(`tab-${tab.dataset.tab}`).classList.add('active')
  })
})

document.getElementById('settings-btn').addEventListener('click', () => {
  document.querySelectorAll('.tab').forEach(t => t.classList.remove('active'))
  document.querySelectorAll('.tab-pane').forEach(p => p.classList.remove('active'))
  document.getElementById('settings-btn').classList.add('active')
  document.getElementById('tab-settings').classList.add('active')
})

// ── Panel tab switching ───────────────────────────────────────────────────────

document.querySelectorAll('.panel-tab').forEach(tab => {
  tab.addEventListener('click', () => {
    document.querySelectorAll('.panel-tab').forEach(t => t.classList.remove('active'))
    document.querySelectorAll('.panel-pane').forEach(p => p.classList.remove('active'))
    tab.classList.add('active')
    document.getElementById(`panel-${tab.dataset.panelTab}`)?.classList.add('active')
  })
})

// ── Expand / collapse detail panel ────────────────────────────────────────────

const expandBtn = document.getElementById('expand-btn')
const panelSlider = document.getElementById('panel-slider')
const detailPanel = document.getElementById('detail-panel')
let expanded = false
let originalWindowWidth = 0

/**
 * Toggles the detail panel open or closed.
 * Restores the saved panel width from settings when expanding.
 * Resizes the app window to double its width (+80px for the divider) when expanding,
 * and restores the original width when collapsing.
 */
expandBtn.addEventListener('click', async () => {
  expanded = !expanded
  expandBtn.textContent = expanded ? '\u2212' : '+'
  if (expanded) {
    const saved = await invoke('get_setting', { key: 'panel_width' })
    detailPanel.style.width = saved ? `${saved}px` : '240px'
    panelSlider.classList.remove('hidden')
    try {
      const win = getCurrent()
      const outer = await win.outerSize()
      const scale = await win.scaleFactor()
      originalWindowWidth = outer.width
      const panelWidth = parseInt(detailPanel.style.width, 10)
      // panelWidth is in CSS logical pixels; multiply by scaleFactor to convert
      // to physical pixels so the window grows correctly on Retina/HiDPI displays.
      await win.setSize(new PhysicalSize(
        outer.width + Math.round((panelWidth + 4) * scale),
        outer.height
      ))
    } catch { /* non-fatal — window resize fails gracefully */ }
    detailPanel.classList.remove('hidden')
  } else {
    detailPanel.classList.add('hidden')
    panelSlider.classList.add('hidden')
    if (originalWindowWidth > 0) {
      try {
        const win = getCurrent()
        const size = await win.outerSize()
        await win.setSize(new PhysicalSize(originalWindowWidth, size.height))
      } catch { /* non-fatal */ }
      originalWindowWidth = 0
    }
  }
})

// ── Slider drag ───────────────────────────────────────────────────────────────

let dragging = false
let startX = 0
let startWidth = 0

/**
 * Begins a drag operation to resize the detail panel.
 *
 * Args:
 *   e: The mousedown MouseEvent.
 */
panelSlider.addEventListener('mousedown', (e) => {
  dragging = true
  startX = e.clientX
  startWidth = detailPanel.offsetWidth
  document.body.style.cursor = 'col-resize'
  document.body.style.userSelect = 'none'
})

/**
 * Updates the detail panel width while dragging. Clamped between 180 and 500px.
 *
 * Args:
 *   e: The mousemove MouseEvent.
 */
document.addEventListener('mousemove', (e) => {
  if (!dragging) return
  const delta = startX - e.clientX
  detailPanel.style.width = `${Math.max(PANEL_MIN_WIDTH, Math.min(PANEL_MAX_WIDTH, startWidth + delta))}px`
})

/**
 * Ends the drag operation and persists the new panel width to settings.
 */
document.addEventListener('mouseup', async () => {
  if (!dragging) return
  dragging = false
  document.body.style.cursor = ''
  document.body.style.userSelect = ''
  await invoke('set_setting', { key: 'panel_width', value: String(detailPanel.offsetWidth) })
})

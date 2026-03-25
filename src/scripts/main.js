/**
 * ReLay app entry point.
 * Coordinates download lifecycle, tab switching, expand/collapse, and slider drag.
 * Delegates card management to downloads.js and history rendering to history.js.
 */

import { invoke } from '@tauri-apps/api/tauri'
import { listen } from '@tauri-apps/api/event'
import { addDownloadCard, updateProgress, setCardComplete, setCardError } from './downloads.js'
import { loadHistory } from './history.js'

const urlInput = document.getElementById('url-input')
const startBtn = document.getElementById('start-btn')
const proPrompt = document.getElementById('pro-prompt')

/** Set of download IDs currently active in this session. */
const activeDownloads = new Set()

// ── Startup ──────────────────────────────────────────────────────────────────

/**
 * Resets downloads interrupted by the previous session, then loads history.
 * reset_stale_downloads is non-fatal — app continues even if it fails.
 */
async function init() {
  try {
    await invoke('reset_stale_downloads')
  } catch { /* non-fatal */ }
  await loadHistory()
}

init()

// ── Download start ────────────────────────────────────────────────────────────

/**
 * Starts a download for the given URL.
 * Shows the Pro prompt and aborts if the free-tier limit (1 concurrent) is reached.
 * On success, creates a live card and subscribes to all lifecycle events.
 *
 * Args:
 *   url: The URL string entered by the user.
 */
async function startDownload(url) {
  if (activeDownloads.size >= 1) {
    proPrompt.classList.remove('hidden')
    return
  }
  proPrompt.classList.add('hidden')

  let id
  try {
    id = await invoke('start_download', { url })
  } catch (err) {
    console.error('start_download failed:', err)
    return
  }

  // Derive filename client-side to match what the backend stored
  const filename = url.split('/').filter(Boolean).pop()?.split('?')[0] || 'download'
  const destination = (await invoke('get_setting', { key: 'default_folder' })) ?? ''

  activeDownloads.add(id)
  addDownloadCard(id, filename, url, destination)

  const unlistenProgress = await listen(`download://progress/${id}`, (e) => {
    updateProgress(id, e.payload.downloaded, e.payload.total ?? null)
  })

  const unlistenComplete = await listen(`download://complete/${id}`, async (e) => {
    setCardComplete(id, e.payload.path)
    activeDownloads.delete(id)
    cleanup()
    await loadHistory()
  })

  const unlistenError = await listen(`download://error/${id}`, async (e) => {
    setCardError(id, e.payload.message)
    activeDownloads.delete(id)
    cleanup()
    await loadHistory()
  })

  /** Removes all event listeners for this download. */
  function cleanup() {
    unlistenProgress()
    unlistenComplete()
    unlistenError()
  }
}

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

proPrompt.querySelector('.pro-prompt-dismiss')?.addEventListener('click', () => {
  proPrompt.classList.add('hidden')
})

// ── Tab switching ─────────────────────────────────────────────────────────────

/**
 * Switches the visible tab pane by toggling the active class on the button and pane.
 */
document.querySelectorAll('.tab').forEach(tab => {
  tab.addEventListener('click', () => {
    document.querySelectorAll('.tab').forEach(t => t.classList.remove('active'))
    document.querySelectorAll('.tab-pane').forEach(p => p.classList.remove('active'))
    tab.classList.add('active')
    document.getElementById(`tab-${tab.dataset.tab}`).classList.add('active')
  })
})

// ── Expand / collapse detail panel ────────────────────────────────────────────

const expandBtn = document.getElementById('expand-btn')
const panelSlider = document.getElementById('panel-slider')
const detailPanel = document.getElementById('detail-panel')
let expanded = false

/**
 * Toggles the detail panel open or closed.
 * Restores the saved panel width from settings when expanding.
 */
expandBtn.addEventListener('click', async () => {
  expanded = !expanded
  expandBtn.textContent = expanded ? '\u2212' : '+'
  panelSlider.classList.toggle('hidden', !expanded)
  detailPanel.classList.toggle('hidden', !expanded)
  if (expanded) {
    const saved = await invoke('get_setting', { key: 'panel_width' })
    detailPanel.style.width = saved ? `${saved}px` : '240px'
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
  detailPanel.style.width = `${Math.max(180, Math.min(500, startWidth + delta))}px`
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

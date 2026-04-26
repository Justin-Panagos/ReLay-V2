/**
 * ReLay popup script.
 *
 * On open: checks whether the desktop app is installed, then renders active
 * downloads or the install prompt. Polls for status every 2 seconds while
 * the popup is open.
 */

/** GitHub Releases URL — users are sent here when the app is not installed. */
const RELEASES_URL = 'https://github.com/Justin-Panangos/ReLay/releases/latest'

/** How often to refresh download status while the popup is visible (ms). */
const POLL_INTERVAL_MS = 2000

let pollTimer = null

// ── Init ─────────────────────────────────────────────────────────────────────

/**
 * Entry point. Wires install-prompt links for the current OS, checks for
 * the stored install-prompt flag, then loads live status from the background
 * service worker.
 */
async function init() {
  wireInstallLinks()
  wireOpenAppButton()
  wirePopupBlockToggle()

  // If a previous download attempt set the install prompt flag, show it.
  const { showInstallPrompt } = await chrome.storage.local.get('showInstallPrompt')
  if (showInstallPrompt) {
    showInstall()
    return
  }

  await refreshStatus()

  // Poll while the popup is open.
  pollTimer = setInterval(refreshStatus, POLL_INTERVAL_MS)
}

/**
 * Sets the GitHub Releases href on the correct OS-specific install button
 * and un-hides it.
 */
function wireInstallLinks() {
  const platform = navigator.platform.toLowerCase()
  const isMac  = platform.includes('mac')
  const isWin  = platform.includes('win')

  document.getElementById('install-btn-mac').href   = RELEASES_URL
  document.getElementById('install-btn-win').href   = RELEASES_URL
  document.getElementById('install-btn-linux').href = RELEASES_URL

  if (isMac) {
    document.getElementById('install-btn-mac').classList.remove('hidden')
  } else if (isWin) {
    document.getElementById('install-btn-win').classList.remove('hidden')
  } else {
    document.getElementById('install-btn-linux').classList.remove('hidden')
  }
}

/**
 * Wires the "Open ReLay" footer link to send a get_status request (which
 * triggers bring_gui_to_front on the Rust side as a side effect of connecting).
 * Falls back gracefully if the message fails.
 */
function wireOpenAppButton() {
  document.getElementById('open-app-btn').addEventListener('click', (e) => {
    e.preventDefault()
    chrome.runtime.sendMessage({ type: 'get_status' }).catch(() => {})
  })
}

// ── Settings ──────────────────────────────────────────────────────────────────

/**
 * Reads popup_blocking_enabled from storage and wires the toggle checkbox.
 * Changes are written back to sync storage immediately and picked up by
 * content.js on the next page load.
 */
function wirePopupBlockToggle() {
  const toggle = document.getElementById('popup-block-toggle')
  if (!toggle) return

  chrome.storage.sync.get({ popup_blocking_enabled: true }, (items) => {
    toggle.checked = items.popup_blocking_enabled
  })

  toggle.addEventListener('change', () => {
    chrome.storage.sync.set({ popup_blocking_enabled: toggle.checked })
  })
}

// ── Status polling ────────────────────────────────────────────────────────────

/**
 * Requests current download status from the background service worker and
 * renders the result. Switches to the install prompt if the app is not found.
 */
async function refreshStatus() {
  let response
  try {
    response = await chrome.runtime.sendMessage({ type: 'get_status' })
  } catch {
    showInstall()
    return
  }

  if (!response || response.error === 'not_installed') {
    showInstall()
    return
  }

  if (response.downloads) {
    renderDownloads(response.downloads)
  }
}

// ── Rendering ─────────────────────────────────────────────────────────────────

/**
 * Shows the install prompt and hides the downloads view.
 */
function showInstall() {
  stopPolling()
  document.getElementById('install-prompt').classList.remove('hidden')
  document.getElementById('downloads-view').classList.add('hidden')
  // Update the status dot if the downloads view was previously shown.
  const dot = document.getElementById('status-dot')
  if (dot) {
    dot.className = 'dot dot-disconnected'
    dot.title = 'ReLay not connected'
  }
}

/**
 * Renders the list of active downloads. Shows the empty state when there
 * are no active downloads. Caps the list at 5 rows.
 *
 * Args:
 *   downloads: Array of download status objects from the native host.
 */
function renderDownloads(downloads) {
  const list  = document.getElementById('downloads-list')
  const empty = document.getElementById('empty-state')

  const active = downloads.filter(
    (d) => d.status === 'downloading' || d.status === 'queued' || d.status === 'paused'
  )

  if (active.length === 0) {
    empty.classList.remove('hidden')
    // Remove any previously rendered cards (keep the empty-state node).
    list.querySelectorAll('.dl-row').forEach((el) => el.remove())
    return
  }

  empty.classList.add('hidden')
  list.querySelectorAll('.dl-row').forEach((el) => el.remove())

  for (const dl of active.slice(0, 5)) {
    const pct         = dl.total > 0 ? Math.round((dl.downloaded / dl.total) * 100) : 0
    const sizeLabel   = dl.total > 0
      ? `${formatBytes(dl.downloaded)} / ${formatBytes(dl.total)}`
      : formatBytes(dl.downloaded)
    const indeterminate = dl.total === 0 || dl.status === 'queued'

    const row = document.createElement('div')
    row.className = 'dl-row'
    row.innerHTML = `
      <div class="dl-name" title="${escHtml(dl.filename)}">${escHtml(dl.filename)}</div>
      <div class="progress-bar">
        <div class="progress-fill${indeterminate ? ' indeterminate' : ''}"
             style="width: ${indeterminate ? '25' : pct}%"></div>
      </div>
      <div class="dl-meta">
        <span class="dl-size">${escHtml(sizeLabel)}</span>
        <span class="dl-pct">${indeterminate ? dl.status : pct + '%'}</span>
      </div>
    `
    list.appendChild(row)
  }
}

// ── Utilities ─────────────────────────────────────────────────────────────────

/**
 * Stops the status-poll timer.
 */
function stopPolling() {
  if (pollTimer !== null) {
    clearInterval(pollTimer)
    pollTimer = null
  }
}

/**
 * Escapes HTML special characters to prevent XSS when setting innerHTML.
 *
 * Args:
 *   s: Input string.
 *
 * Returns:
 *   HTML-escaped string.
 */
function escHtml(s) {
  return String(s)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
}

/**
 * Formats a byte count into a human-readable string (B, KB, MB, GB).
 *
 * Args:
 *   n: Number of bytes.
 *
 * Returns:
 *   Formatted string, e.g. "4.2 MB".
 */
function formatBytes(n) {
  if (n < 1024)        return `${n} B`
  if (n < 1_048_576)   return `${(n / 1024).toFixed(1)} KB`
  if (n < 1_073_741_824) return `${(n / 1_048_576).toFixed(1)} MB`
  return `${(n / 1_073_741_824).toFixed(2)} GB`
}

// ── Lifecycle ─────────────────────────────────────────────────────────────────

document.addEventListener('DOMContentLoaded', init)
window.addEventListener('unload', stopPolling)

/**
 * ReLay — MV3 service worker.
 *
 * Responsibilities:
 *   1. Intercept file downloads before Chrome saves them; forward URL to the
 *      ReLay desktop app via Chrome Native Messaging.
 *   2. If the native host is not installed, flag the install prompt and open
 *      the popup so the user sees the download link.
 *   3. Bridge popup status-poll requests to the native host and return results.
 *   4. Switch the toolbar icon between default (grey) and busy (teal) based on
 *      whether any downloads are currently active.
 */

const HOST_NAME = 'com.relay.app'

/**
 * File extensions that ReLay should intercept.
 * The set covers executables, installers, archives, and Linux package formats.
 */
const INTERCEPTED_EXTS = new Set([
  'exe', 'msi', 'dmg', 'pkg', 'deb', 'rpm',
  'zip', 'tar', 'gz', 'bz2', 'xz', '7z', 'rar',
  'appimage', 'flatpak', 'snap',
])

/** Toolbar icon path sets — grey default and teal busy. */
const ICONS = {
  default: { 16: 'icons/icon16.png', 48: 'icons/icon48.png', 128: 'icons/icon128.png' },
  busy:    { 16: 'icons/busy16.png',  48: 'icons/busy48.png',  128: 'icons/busy128.png'  },
}

/** Alarm name used for periodic status polling while downloads are active. */
const POLL_ALARM = 'relay_status_poll'

/** Active native messaging port. Null when disconnected or not yet opened. */
let port = null

/**
 * Whether the ReLay desktop app was last seen as installed.
 * Flipped to false on connect failure; reset to true on successful connect.
 */
let appInstalled = true

/**
 * Map of pending status-poll callbacks keyed by request id (timestamp).
 * Each entry is { resolve, reject } waiting on a native 'status' response.
 */
const pendingCallbacks = new Map()

// ── Icon helpers ──────────────────────────────────────────────────────────────

/**
 * Sets the toolbar icon and persists the state so it can be restored after
 * the service worker is restarted.
 *
 * Args:
 *   busy: true to show the teal downloading icon, false for the default grey.
 */
function setIcon(busy) {
  chrome.action.setIcon({ path: busy ? ICONS.busy : ICONS.default })
  chrome.storage.local.set({ iconBusy: busy })
}

/**
 * Starts a recurring alarm that polls the native host for active downloads.
 * No-ops if the alarm already exists.
 */
function startPollAlarm() {
  chrome.alarms.get(POLL_ALARM, (alarm) => {
    if (!alarm) {
      chrome.alarms.create(POLL_ALARM, { periodInMinutes: 1 })
    }
  })
}

/**
 * Cancels the status-poll alarm.
 */
function stopPollAlarm() {
  chrome.alarms.clear(POLL_ALARM)
}

// ── Native messaging ──────────────────────────────────────────────────────────

/**
 * Returns the active native messaging port, opening a new one if needed.
 * Sets appInstalled = false and returns null if the host binary is not found.
 *
 * Returns:
 *   The chrome.runtime.Port, or null if the host is not available.
 */
function getPort() {
  if (port) return port

  try {
    port = chrome.runtime.connectNative(HOST_NAME)

    port.onMessage.addListener(onNativeMessage)

    port.onDisconnect.addListener(() => {
      port = null
      const err = chrome.runtime.lastError?.message ?? 'disconnected'
      if (err.includes('not found') || err.includes('not installed') || err.includes('cannot find')) {
        appInstalled = false
      }
      for (const [, { reject }] of pendingCallbacks) {
        reject(new Error(err))
      }
      pendingCallbacks.clear()
    })

    appInstalled = true
  } catch {
    port = null
    appInstalled = false
  }

  return port
}

/**
 * Sends a message to the native host. Returns false if the port could not be
 * opened (app not installed).
 *
 * Args:
 *   msg: Plain object to serialise and send.
 *
 * Returns:
 *   true if the message was sent, false otherwise.
 */
function sendToNative(msg) {
  const p = getPort()
  if (!p) return false
  p.postMessage(msg)
  return true
}

/**
 * Handles messages arriving from the native host.
 * Updates the toolbar icon based on whether any downloads are active.
 * Dispatches 'status' payloads to any waiting popup poll callbacks.
 *
 * Args:
 *   msg: Deserialised JSON message from the native host.
 */
function onNativeMessage(msg) {
  if (msg.type === 'status') {
    const hasActive = Array.isArray(msg.downloads) && msg.downloads.length > 0
    setIcon(hasActive)
    if (!hasActive) stopPollAlarm()

    for (const [id, { resolve }] of pendingCallbacks) {
      resolve(msg.downloads)
      pendingCallbacks.delete(id)
    }
  }
}

// ── Download interception ─────────────────────────────────────────────────────

/**
 * Returns true if the given URL or filename should be intercepted by ReLay.
 * Checks the filename extension first (most reliable), then falls back to
 * the URL path extension.
 *
 * Args:
 *   url:      The download URL string.
 *   filename: The filename Chrome resolved for the download (may be empty).
 *
 * Returns:
 *   true if ReLay should handle this download.
 */
function shouldIntercept(url, filename) {
  const fromFilename = filename?.split('.').pop()?.toLowerCase()
  if (fromFilename && INTERCEPTED_EXTS.has(fromFilename)) return true

  try {
    const urlExt = new URL(url).pathname.split('.').pop()?.toLowerCase()
    if (urlExt && INTERCEPTED_EXTS.has(urlExt)) return true
  } catch { /* invalid URL — ignore */ }

  return false
}

/**
 * Fires on every new Chrome download. Intercepts matching file types,
 * cancels Chrome's built-in download, and forwards the URL to ReLay.
 * Switches the toolbar icon to teal and starts polling while the download
 * is active. If the native host is unavailable, sets the install prompt flag
 * and opens the popup so the user sees the download prompt.
 */
chrome.downloads.onCreated.addListener((item) => {
  if (!shouldIntercept(item.url, item.filename)) return

  // Cancel Chrome's download immediately.
  chrome.downloads.cancel(item.id, () => {
    chrome.downloads.erase({ id: item.id })
  })

  const sent = sendToNative({
    type: 'start_download',
    url: item.url,
    filename: item.filename ?? '',
  })

  if (sent) {
    setIcon(true)
    startPollAlarm()
  } else {
    // Desktop app not installed — show the install prompt in the popup.
    chrome.storage.local.set({ showInstallPrompt: true })
    chrome.action.openPopup().catch(() => {
      // openPopup() requires user gesture in some Chrome versions; silently fail.
    })
  }
})

// ── Status-poll alarm ─────────────────────────────────────────────────────────

/**
 * Fires on each alarm tick. Polls the native host for active downloads and
 * updates the icon. If the native host is unreachable, stops polling and
 * resets the icon to default.
 */
chrome.alarms.onAlarm.addListener((alarm) => {
  if (alarm.name !== POLL_ALARM) return

  const sent = sendToNative({ type: 'get_status' })
  if (!sent) {
    setIcon(false)
    stopPollAlarm()
  }
})

// ── Startup icon restore ──────────────────────────────────────────────────────

/**
 * Restores the icon state and poll alarm after the service worker restarts.
 * This ensures the icon stays consistent across browser sessions.
 */
chrome.runtime.onStartup.addListener(() => {
  chrome.storage.local.get('iconBusy', ({ iconBusy }) => {
    if (iconBusy) {
      setIcon(true)
      startPollAlarm()
    }
  })
})

// ── Popup message bridge ──────────────────────────────────────────────────────

/**
 * Handles messages from popup.js.
 *
 * get_status: Asks the native host for active download status. Responds
 *   asynchronously (returns true to keep the channel open). Times out after
 *   3 seconds if the native host does not respond.
 */
chrome.runtime.onMessage.addListener((msg, _sender, sendResponse) => {
  if (msg.type !== 'get_status') return false

  if (!appInstalled) {
    sendResponse({ error: 'not_installed' })
    return false
  }

  const reqId = Date.now()

  const timer = setTimeout(() => {
    if (pendingCallbacks.has(reqId)) {
      pendingCallbacks.delete(reqId)
      sendResponse({ error: 'timeout' })
    }
  }, 3000)

  pendingCallbacks.set(reqId, {
    resolve: (downloads) => {
      clearTimeout(timer)
      sendResponse({ downloads })
    },
    reject: (err) => {
      clearTimeout(timer)
      sendResponse({ error: err.message })
    },
  })

  const sent = sendToNative({ type: 'get_status' })
  if (!sent) {
    clearTimeout(timer)
    pendingCallbacks.delete(reqId)
    sendResponse({ error: 'not_installed' })
    return false
  }

  return true // keep channel open for async sendResponse
})

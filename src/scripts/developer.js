/**
 * Developer tab — manages API key and snapshot token purchase, polling, and
 * credential display for the Threat Intelligence API.
 */

import { invoke } from '@tauri-apps/api/tauri'
import { showErrorToast, showInfoToast } from './toast.js'

// ── Module state ──────────────────────────────────────────────────────────────
// Keys and tokens are kept in JS closure only — never written to the DOM.

let pollingTimer    = null
let pollingAttempts = 0
const POLL_MAX      = 100   // 100 × 3 s = 5 minutes
const POLL_INTERVAL = 3000

let countdownTimer = null

let pendingEmail = ''
let pendingPlan  = ''
let storedKey    = ''  // full API key or snapshot token, never placed in the DOM

// ── Initialisation ────────────────────────────────────────────────────────────

/**
 * Initialises the Developer tab.
 * Loads any previously stored credentials from the backend and renders the
 * appropriate UI state. Wires purchase buttons when no credentials are present.
 */
export async function initDeveloper() {
  let creds
  try {
    creds = await invoke('get_stored_api_credentials')
  } catch {
    renderNoKey()
    wireNoKeyButtons()
    return
  }

  if (creds.plan !== 'none' && creds.key !== '') {
    storedKey = creds.key
    if (creds.plan === 'snapshot') {
      renderSnapshotActive(creds)
    } else {
      renderKeyActive(creds)
    }
  } else {
    renderNoKey()
    wireNoKeyButtons()
  }
}

// ── State rendering ───────────────────────────────────────────────────────────

/**
 * Switches the visible Developer tab panel.
 *
 * Args:
 *   state: One of "no-key" | "polling" | "key-active" | "snapshot-active".
 */
function setState(state) {
  document.getElementById('dev-no-key')         ?.classList.toggle('hidden', state !== 'no-key')
  document.getElementById('dev-polling')        ?.classList.toggle('hidden', state !== 'polling')
  document.getElementById('dev-key-active')     ?.classList.toggle('hidden', state !== 'key-active')
  document.getElementById('dev-snapshot-active')?.classList.toggle('hidden', state !== 'snapshot-active')

  if (state !== 'snapshot-active') {
    clearInterval(countdownTimer)
    countdownTimer = null
  }
}

/**
 * Renders the no-credentials state (purchase options).
 */
function renderNoKey() {
  setState('no-key')
}

/**
 * Renders the active API subscription state with masked key and endpoint URL.
 *
 * Args:
 *   creds: Object with plan, expiry, and endpoint_url fields.
 */
function renderKeyActive(creds) {
  setState('key-active')

  const planBadge  = document.getElementById('dev-plan-badge')
  const expiryEl   = document.getElementById('dev-expiry-value')
  const endpointEl = document.getElementById('dev-endpoint-url')

  if (planBadge)  planBadge.textContent  = planLabel(creds.plan)
  if (expiryEl)   expiryEl.textContent   = creds.expiry > 0
    ? new Date(Number(creds.expiry) * 1000).toLocaleDateString()
    : '\u2014'
  if (endpointEl) endpointEl.value = creds.endpoint_url ?? ''

  const copyBtn   = document.getElementById('dev-key-copy-btn')
  const cancelBtn = document.getElementById('dev-cancel-sub-btn')

  const keyForClosure = storedKey  // capture in closure

  copyBtn?.addEventListener('click', () => {
    navigator.clipboard.writeText(keyForClosure).catch(() => {})
    copyBtn.textContent = 'Copied!'
    setTimeout(() => { copyBtn.textContent = 'Copy' }, 1500)
  })

  cancelBtn?.addEventListener('click', async () => {
    if (!confirm('Cancel your API subscription? Access will end immediately.')) return
    if (cancelBtn) cancelBtn.disabled = true
    try {
      await invoke('cancel_api_subscription', { email: creds.email ?? '' })
      storedKey = ''
      renderNoKey()
      wireNoKeyButtons()
    } catch (err) {
      showErrorToast(`Could not cancel subscription: ${err}`)
    } finally {
      if (cancelBtn) cancelBtn.disabled = false
    }
  })
}

/**
 * Renders the active snapshot token state with live countdown timer.
 *
 * Args:
 *   creds: Object with expiry field (Unix seconds).
 */
function renderSnapshotActive(creds) {
  setState('snapshot-active')
  startCountdown(creds.expiry)

  const downloadBtn = document.getElementById('dev-download-btn')
  downloadBtn?.addEventListener('click', async () => {
    const format = document.querySelector('[name="dev-format"]:checked')?.value ?? 'json'
    let destination
    try {
      destination = await invoke('pick_folder')
    } catch {
      return
    }
    if (!destination) return

    if (downloadBtn) {
      downloadBtn.disabled    = true
      downloadBtn.textContent = 'Downloading\u2026'
    }
    try {
      await invoke('download_threat_export', { format, destination })
      showInfoToast('Export saved to disk')
      storedKey = ''
      renderNoKey()
      wireNoKeyButtons()
    } catch (err) {
      if (String(err).includes('already used')) {
        showErrorToast('Token was already used \u2014 credentials cleared')
        storedKey = ''
        renderNoKey()
        wireNoKeyButtons()
      } else {
        showErrorToast(`Download failed: ${err}`)
      }
    } finally {
      if (downloadBtn) {
        downloadBtn.disabled    = false
        downloadBtn.textContent = 'Download to disk'
      }
    }
  })
}

// ── Polling ───────────────────────────────────────────────────────────────────

/**
 * Wires the purchase buttons in the no-key state.
 * Validates email, initiates checkout, then starts the payment polling loop.
 */
function wireNoKeyButtons() {
  const emailEl     = document.getElementById('dev-email-input')
  const snapshotBtn = document.getElementById('dev-snapshot-btn')

  snapshotBtn?.addEventListener('click', async () => {
    const email = emailEl?.value.trim() ?? ''
    if (!isValidEmail(email)) { emailEl?.focus(); return }
    try {
      await invoke('api_snapshot_checkout', { email })
    } catch (err) {
      showErrorToast(`Could not start checkout: ${err}`)
      return
    }
    pendingEmail = email
    pendingPlan  = 'snapshot'
    startPolling('Snapshot ($10 once-off)')
  })

  document.querySelectorAll('.dev-plan-btn').forEach(btn => {
    btn.addEventListener('click', async () => {
      const email = emailEl?.value.trim() ?? ''
      if (!isValidEmail(email)) { emailEl?.focus(); return }
      const plan = btn.dataset.plan
      try {
        await invoke('api_key_checkout', { plan, email })
      } catch (err) {
        showErrorToast(`Could not start checkout: ${err}`)
        return
      }
      pendingEmail = email
      pendingPlan  = plan
      startPolling(planLabel(plan))
    })
  })
}

/**
 * Starts polling the Worker for payment confirmation.
 *
 * Args:
 *   label: Human-readable plan name shown in the polling state UI.
 */
function startPolling(label) {
  pollingAttempts = 0
  const planEl = document.getElementById('dev-polling-plan')
  if (planEl) planEl.textContent = label
  setState('polling')

  const cancelBtn = document.getElementById('dev-polling-cancel')
  cancelBtn?.addEventListener('click', () => {
    stopPolling()
    renderNoKey()
    wireNoKeyButtons()
  }, { once: true })

  pollingTimer = setInterval(doPoll, POLL_INTERVAL)
}

/**
 * Single poll iteration. Checks the Worker for a confirmed payment and updates
 * UI state on success or timeout.
 */
async function doPoll() {
  pollingAttempts++
  if (pollingAttempts > POLL_MAX) {
    stopPolling()
    showErrorToast('Payment not confirmed after 5 minutes \u2014 try again.')
    renderNoKey()
    wireNoKeyButtons()
    return
  }

  let status
  try {
    status = await invoke('poll_developer_status', { email: pendingEmail })
  } catch {
    return  // transient error, retry next tick
  }

  if (!status.found) return  // still waiting

  stopPolling()
  storedKey = status.key

  if (status.plan === 'snapshot') {
    renderSnapshotActive({ expiry: status.expiry })
  } else {
    renderKeyActive({
      email:        pendingEmail,
      plan:         status.plan,
      expiry:       status.expiry,
      endpoint_url: status.endpoint_url,
    })
  }
}

/**
 * Stops the active polling interval.
 */
function stopPolling() {
  clearInterval(pollingTimer)
  pollingTimer    = null
  pollingAttempts = 0
}

// ── Countdown timer ───────────────────────────────────────────────────────────

/**
 * Starts a live countdown on #dev-snap-countdown showing time remaining until
 * the snapshot token expires. Reverts to no-key state on expiry.
 *
 * Args:
 *   expiryUnix: Unix seconds when the token expires.
 */
function startCountdown(expiryUnix) {
  const el = document.getElementById('dev-snap-countdown')

  function tick() {
    const secsLeft = expiryUnix - Math.floor(Date.now() / 1000)
    if (secsLeft <= 0) {
      clearInterval(countdownTimer)
      countdownTimer = null
      if (el) el.textContent = 'Expired'
      storedKey = ''
      renderNoKey()
      wireNoKeyButtons()
      return
    }
    const h = Math.floor(secsLeft / 3600)
    const m = Math.floor((secsLeft % 3600) / 60)
    const s = secsLeft % 60
    if (el) el.textContent = `${h}h ${String(m).padStart(2, '0')}m ${String(s).padStart(2, '0')}s`
  }

  tick()
  countdownTimer = setInterval(tick, 1000)
}

// ── Utilities ─────────────────────────────────────────────────────────────────

/**
 * Returns a human-readable label for a plan identifier.
 *
 * Args:
 *   plan: "snapshot" | "monthly"
 *
 * Returns:
 *   Display string.
 */
function planLabel(plan) {
  return { snapshot: 'Snapshot', monthly: 'Monthly' }[plan] ?? plan
}

/**
 * Validates a basic email address format.
 *
 * Args:
 *   email: String to validate.
 *
 * Returns:
 *   True if the string looks like a valid email address.
 */
function isValidEmail(email) {
  return /^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(email)
}


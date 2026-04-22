/**
 * Settings tab — loads saved settings from the backend and wires up live save on change.
 */

import { invoke } from '@tauri-apps/api/tauri'
import { showErrorToast } from './toast.js'

/**
 * Initialises the Settings tab.
 * Loads current values from the backend and binds change handlers that
 * persist updates immediately via set_setting. Also loads and renders the Pro panel.
 */
export async function initSettings() {
  // ── Appearance — theme ────────────────────────────────────────────────────
  const themeEl = document.getElementById('setting-theme')
  if (themeEl) {
    const savedTheme = await invoke('get_setting', { key: 'theme' }).catch(() => null)
    themeEl.value = savedTheme ?? 'auto'
    themeEl.addEventListener('change', async () => {
      const theme = themeEl.value
      await invoke('set_setting', { key: 'theme', value: theme }).catch(() => {})
      if (theme === 'auto') {
        document.documentElement.removeAttribute('data-theme')
      } else {
        document.documentElement.setAttribute('data-theme', theme)
      }
    })
  }

  // ── Shield settings ───────────────────────────────────────────────────────
  const [sandboxEnabled, vtKey] = await Promise.all([
    invoke('get_setting', { key: 'sandbox_enabled' }).catch(() => null),
    invoke('get_setting', { key: 'virustotal_api_key' }).catch(() => null),
  ])

  const sandboxEl = document.getElementById('setting-sandbox')
  const vtKeyEl   = document.getElementById('setting-vt-key')

  if (sandboxEl) {
    sandboxEl.checked = sandboxEnabled === 'true'
    sandboxEl.addEventListener('change', async () => {
      await invoke('set_setting', {
        key: 'sandbox_enabled',
        value: sandboxEl.checked ? 'true' : 'false',
      })
    })
  }

  if (vtKeyEl) {
    vtKeyEl.value = vtKey ?? ''
    vtKeyEl.addEventListener('change', async () => {
      await invoke('set_setting', {
        key: 'virustotal_api_key',
        value: vtKeyEl.value.trim(),
      })
    })
  }

  // ── Pro panel ─────────────────────────────────────────────────────────────
  await loadProStatus()

  document.getElementById('pro-upgrade-btn')?.addEventListener('click', async () => {
    const emailEl = document.getElementById('pro-email-input')
    const email = emailEl?.value.trim() ?? ''
    if (!email) {
      emailEl?.focus()
      return
    }
    await invoke('open_upgrade_page', { email }).catch(err => {
      console.error('open_upgrade_page:', err)
      showErrorToast(`Could not open upgrade page: ${err}`)
    })
  })

  document.getElementById('pro-cancel-btn')?.addEventListener('click', async () => {
    const btn = document.getElementById('pro-cancel-btn')
    if (!confirm('Cancel your Pro subscription? You will lose access at the end of the current billing period.')) return
    if (btn) btn.disabled = true
    try {
      await invoke('cancel_subscription')
      await loadProStatus()
    } catch (err) {
      console.error('cancel_subscription:', err)
      showErrorToast(`Could not cancel subscription: ${err}`)
    } finally {
      if (btn) btn.disabled = false
    }
  })

  document.getElementById('pro-recheck-btn')?.addEventListener('click', async () => {
    const btn = document.getElementById('pro-recheck-btn')
    if (btn) btn.disabled = true
    try {
      await invoke('recheck_licence')
      await loadProStatus()
    } catch (err) {
      console.error('recheck_licence:', err)
    } finally {
      if (btn) btn.disabled = false
    }
  })
}

/**
 * Fetches the current Pro status from the backend and updates the Pro panel DOM.
 * Safe to call multiple times — overwrites previous values each time.
 */
async function loadProStatus() {
  let info
  try {
    info = await invoke('get_pro_status')
  } catch {
    return
  }

  const isPro     = info.status === 'pro'
  const statusEl  = document.getElementById('pro-status-value')
  const expiryEl  = document.getElementById('pro-expiry-value')
  const deviceEl  = document.getElementById('pro-device-id')
  const upgradeRow    = document.getElementById('pro-upgrade-row')
  const upgradeBtnRow = document.getElementById('pro-upgrade-btn-row')
  const cancelRow     = document.getElementById('pro-cancel-row')

  if (statusEl) {
    statusEl.textContent = isPro ? 'Pro' : 'Free'
    statusEl.style.color = isPro
      ? 'var(--accent-green)'
      : 'var(--text-muted)'
  }

  if (expiryEl) {
    expiryEl.textContent = info.expiry_timestamp
      ? new Date(Number(info.expiry_timestamp) * 1000).toLocaleDateString()
      : '\u2014'
  }

  if (deviceEl) {
    deviceEl.textContent = info.device_id || '\u2014'
    deviceEl.title = info.device_id || ''
  }

  upgradeRow?.classList.toggle('hidden', isPro)
  upgradeBtnRow?.classList.toggle('hidden', isPro)
  cancelRow?.classList.toggle('hidden', !isPro)
}

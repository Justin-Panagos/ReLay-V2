/**
 * Settings tab — loads saved settings from the backend and wires up live save on change.
 */

import { invoke } from '@tauri-apps/api/tauri'

/**
 * Initialises the Settings tab.
 * Loads current values from the backend and binds change handlers that
 * persist updates immediately via set_setting.
 */
export async function initSettings() {
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
}

/**
 * ReLay — content script.
 *
 * Injected at document_start (before any page script runs) on all URLs.
 * Overrides window.open to suppress popup/redirect chains that download sites
 * commonly trigger when a user clicks a download link.
 *
 * Also installs two smart popup interceptors:
 *  A. Timed overlay detector — hides large fixed overlays added after page load.
 *  B. Redirecting close button trap — detects close buttons that navigate away.
 *
 * All interceptors are gated by the `popup_blocking_enabled` setting (default true).
 */

;(function () {
  'use strict'

  // ── Settings gate ────────────────────────────────────────────────────────────

  let popupBlockingEnabled = true
  chrome.storage.sync.get({ popup_blocking_enabled: true }, (items) => {
    popupBlockingEnabled = items.popup_blocking_enabled
  })

  // ── window.open override ─────────────────────────────────────────────────────

  /**
   * URL patterns that indicate a redirect or tracking chain rather than a
   * genuine new window the user requested. Conservative set — only match
   * patterns that are reliably associated with ad redirects on download sites.
   */
  const REDIRECT_PATTERNS = [
    /[?&]download=/i,
    /download\.php\?/i,
    /redirect\.php\?/i,
    /go\.php\?/i,
    /click\.php\?/i,
    /out\.php\?/i,
    /\/track[?/]/i,
    /\/go\//i,
    /\/redirect\//i,
    /\/click\//i,
    /[?&]ref=/i,
    /[?&]url=/i,
  ]

  const _originalOpen = window.open.bind(window)

  /**
   * Replacement for window.open that suppresses calls matching known redirect
   * patterns. All other calls are forwarded to the original window.open.
   *
   * Args:
   *   url:      The URL to open (string or URL object).
   *   target:   Window target (_blank, _self, etc.).
   *   features: Window features string.
   *
   * Returns:
   *   null if the call was suppressed, otherwise the opened Window object.
   */
  window.open = function (url, target, features) {
    if (!popupBlockingEnabled) return _originalOpen(url, target, features)

    const urlStr = String(url ?? '')

    if (urlStr && REDIRECT_PATTERNS.some((p) => p.test(urlStr))) {
      console.debug('[ReLay] Blocked redirect popup:', urlStr)
      return null
    }

    return _originalOpen(url, target, features)
  }

  // ── Interceptor A: Timed overlay detector ────────────────────────────────────

  const loadedAt = Date.now()
  let lastUserGesture = 0

  document.addEventListener('click', () => { lastUserGesture = Date.now() }, true)
  document.addEventListener('keydown', () => { lastUserGesture = Date.now() }, true)

  /**
   * Renders a brief toast notification in the bottom-right corner of the page.
   *
   * Args:
   *   msg: The message string to display.
   */
  function showBlockedToast(msg) {
    const t = document.createElement('div')
    t.textContent = msg
    t.style.cssText = 'position:fixed;bottom:16px;right:16px;background:#1a1a2e;color:#fff;'
      + 'padding:10px 16px;border-radius:6px;font-size:13px;z-index:2147483647;pointer-events:none'
    document.body.appendChild(t)
    setTimeout(() => t.remove(), 3000)
  }

  /**
   * Wires click-capture listeners on close-pattern children of a hidden overlay.
   * If clicking the close element causes a cross-origin navigation, navigates back
   * and shows a toast.
   *
   * Args:
   *   node: The overlay element that was blocked.
   */
  function wireCloseButtons(node) {
    const CLOSE_PATTERN = /close|dismiss|no\s*thanks|skip|[×✕]/i
    const candidates = node.querySelectorAll('a, button, [role="button"], [onclick]')
    const originBefore = window.location.origin

    candidates.forEach((el) => {
      const label = (el.textContent || el.getAttribute('aria-label') || '').trim()
      if (!CLOSE_PATTERN.test(label)) return

      el.addEventListener('click', () => {
        setTimeout(() => {
          if (window.location.origin !== originBefore) {
            history.back()
            showBlockedToast('ReLay: Blocked redirect from close button')
          }
        }, 50)
      }, { capture: true })
    })
  }

  /**
   * Tests whether a DOM node is a timed overlay that should be blocked:
   * large, fixed/absolute, high z-index, added more than 2 s after load,
   * and not a user-triggered dialog.
   *
   * Args:
   *   node: The element to test.
   *
   * Returns:
   *   true if the node should be hidden.
   */
  function isTimedOverlay(node) {
    if (!(node instanceof HTMLElement)) return false
    if (!node.isConnected) return false
    if (Date.now() - loadedAt < 2000) return false

    const style = window.getComputedStyle(node)
    if (style.position !== 'fixed' && style.position !== 'absolute') return false

    const zIndex = parseInt(style.zIndex, 10)
    if (isNaN(zIndex) || zIndex < 1000) return false

    const vw = window.innerWidth
    const vh = window.innerHeight
    const rect = node.getBoundingClientRect()
    if (rect.width < vw * 0.3 || rect.height < vh * 0.3) return false

    // Allow legitimate modals opened by a user gesture within the last 2 s.
    const hasDialogRole = node.getAttribute('role') === 'dialog'
      || !!node.closest('[role="dialog"]')
    if (hasDialogRole && Date.now() - lastUserGesture < 2000) return false

    return true
  }

  const observer = new MutationObserver((mutations) => {
    if (!popupBlockingEnabled) return
    for (const mutation of mutations) {
      for (const node of mutation.addedNodes) {
        if (isTimedOverlay(node)) {
          node.style.display = 'none'
          console.debug('[ReLay] Blocked timed overlay:', node)
          wireCloseButtons(node)
        }
      }
    }
  })

  // Start observing once the body is available.
  if (document.body) {
    observer.observe(document.body, { childList: true, subtree: true })
  } else {
    document.addEventListener('DOMContentLoaded', () => {
      observer.observe(document.body, { childList: true, subtree: true })
    })
  }
})()

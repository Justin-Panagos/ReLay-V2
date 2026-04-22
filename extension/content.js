/**
 * ReLay — content script.
 *
 * Injected at document_start (before any page script runs) on all URLs.
 * Overrides window.open to suppress popup/redirect chains that download sites
 * commonly trigger when a user clicks a download link.
 *
 * The override is in place before the page's own scripts execute, so it
 * intercepts calls that page scripts make immediately on load.
 */

;(function () {
  'use strict'

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
    const urlStr = String(url ?? '')

    if (urlStr && REDIRECT_PATTERNS.some((p) => p.test(urlStr))) {
      console.debug('[ReLay] Blocked redirect popup:', urlStr)
      return null
    }

    return _originalOpen(url, target, features)
  }
})()

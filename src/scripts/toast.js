/**
 * Lightweight toast notification utility.
 * Provides showErrorToast() for surfacing action failures and showInfoToast()
 * for success/informational feedback. Toasts stack vertically, auto-dismiss
 * after 5 s, and can be clicked to dismiss early. At most 3 toasts are visible
 * at once — additional calls are dropped silently.
 */

let visible = 0
const MAX = 3

/**
 * Shows a brief error toast in the bottom-right corner of the app window.
 * Does nothing if 3 toasts are already visible.
 *
 * Args:
 *   message: Human-readable error description to display.
 */
export function showErrorToast(message) {
  showToast(message, 'toast-error')
}

/**
 * Shows a brief informational toast (e.g. for success feedback).
 * Does nothing if 3 toasts are already visible.
 *
 * Args:
 *   message: Human-readable message to display.
 */
export function showInfoToast(message) {
  showToast(message, 'toast-info')
}

/**
 * Internal helper — creates a toast element, appends it, and schedules auto-dismiss.
 * Stores the timer ID on the element so clicking to dismiss early can cancel it,
 * preventing the timer from firing after the element has already been removed.
 *
 * Args:
 *   message:   Text to display.
 *   className: CSS class controlling toast colour ('toast-error' or 'toast-info').
 */
function showToast(message, className) {
  if (visible >= MAX) return
  visible++

  const el = document.createElement('div')
  el.className = `toast ${className}`
  el.textContent = message

  const timerId = setTimeout(() => dismiss(el), 5000)
  el._timerId = timerId

  el.addEventListener('click', () => {
    clearTimeout(el._timerId)
    dismiss(el)
  })
  document.body.appendChild(el)
}

/**
 * Removes a toast element from the DOM and decrements the visible counter.
 *
 * Args:
 *   el: The toast HTMLElement to remove.
 */
function dismiss(el) {
  if (!el.parentNode) return
  el.remove()
  visible--
}

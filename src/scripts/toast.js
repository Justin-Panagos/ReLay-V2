/**
 * Lightweight toast notification utility.
 * Provides showErrorToast() for surfacing action failures to the user.
 * Toasts stack vertically, auto-dismiss after 5 s, and can be clicked to dismiss early.
 * At most 3 toasts are visible at once — additional calls are dropped silently.
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
  if (visible >= MAX) return
  visible++

  const el = document.createElement('div')
  el.className = 'toast toast-error'
  el.textContent = message
  el.addEventListener('click', () => dismiss(el))
  document.body.appendChild(el)

  setTimeout(() => dismiss(el), 5000)
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

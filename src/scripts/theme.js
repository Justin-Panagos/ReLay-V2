/**
 * Theme helpers — apply a theme to the document and keep the app icon in sync.
 * Extracted into its own module to avoid circular imports between main.js and settings.js.
 */

/**
 * Swaps #app-icon src between dark and light variants based on the active effective theme.
 * Reads data-src-dark / data-src-light attributes from the element.
 */
export function updateAppIcon() {
  const icon = document.getElementById('app-icon')
  if (!icon) return
  const dt = document.documentElement.getAttribute('data-theme')
  const isDark = dt === 'dark' || (dt == null && window.matchMedia('(prefers-color-scheme: dark)').matches)
  const src = isDark ? icon.dataset.srcDark : icon.dataset.srcLight
  if (src && icon.getAttribute('src') !== src) icon.src = src
}

/**
 * Applies the given theme to the document root and updates the app icon.
 * "auto" removes data-theme so CSS @media prefers-color-scheme takes over.
 *
 * Args:
 *   theme: "auto" | "light" | "dark"
 */

export function applyTheme(theme) {
  if (theme === 'auto') {
    document.documentElement.removeAttribute('data-theme')
  } else {
    document.documentElement.setAttribute('data-theme', theme)
  }
  updateAppIcon()
}

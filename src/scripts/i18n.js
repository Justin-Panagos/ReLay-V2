/**
 * Locale loader and translation function for ReLay.
 * Fetches JSON bundles from src/locales/, applies them to the DOM, and
 * exposes t() for dynamic string lookups throughout the app.
 */

let strings = {}
let currentLang = 'en'

/**
 * Returns the language tag that is currently loaded.
 *
 * Returns:
 *   BCP 47 language tag string (e.g. 'en', 'es').
 */
export function getLang() {
  return currentLang
}

/**
 * Fetches the locale JSON for `lang`, falls back to 'en' on any failure.
 * Populates the module-level strings map and immediately re-applies all
 * data-i18n, data-i18n-placeholder, data-i18n-title, and data-i18n-key
 * attributes in the DOM.
 *
 * Args:
 *   lang: BCP 47 language tag (e.g. 'en', 'es', 'fr', 'pt').
 */
export async function loadLocale(lang = 'en') {
  currentLang = lang
  try {
    const res = await fetch(`./locales/${lang}.json`)
    if (!res.ok) throw new Error(`HTTP ${res.status}`)
    strings = await res.json()
  } catch {
    if (lang !== 'en') {
      try {
        const res = await fetch('./locales/en.json')
        if (!res.ok) throw new Error()
        strings = await res.json()
      } catch {
        strings = {}
      }
    } else {
      strings = {}
    }
  }
  applyStaticTranslations()
}

/**
 * Returns the translated string for `key`. Interpolates `vars` into
 * {{name}} placeholders. Falls back to the key itself if the string is missing.
 *
 * Args:
 *   key:  Dot-separated locale key (e.g. 'downloads.status.complete').
 *   vars: Object of placeholder values (e.g. { attempt: 2 }).
 *
 * Returns:
 *   Translated string with placeholders replaced, or the key if not found.
 */
export function t(key, vars = {}) {
  const val = key.split('.').reduce((obj, k) => obj?.[k], strings)
  if (typeof val !== 'string') return key
  return val.replace(/\{\{(\w+)\}\}/g, (_, name) => (name in vars ? vars[name] : `{{${name}}}`))
}

/**
 * Walks the DOM and applies translations to all annotated elements.
 * Also re-translates dynamic card status elements that carry data-i18n-key.
 */
function applyStaticTranslations() {
  document.querySelectorAll('[data-i18n]').forEach(el => {
    const val = t(el.dataset.i18n)
    if (val !== el.dataset.i18n) el.textContent = val
  })
  document.querySelectorAll('[data-i18n-placeholder]').forEach(el => {
    const val = t(el.dataset.i18nPlaceholder)
    if (val !== el.dataset.i18nPlaceholder) el.placeholder = val
  })
  document.querySelectorAll('[data-i18n-title]').forEach(el => {
    const val = t(el.dataset.i18nTitle)
    if (val !== el.dataset.i18nTitle) el.title = val
  })
  document.querySelectorAll('[data-i18n-key]').forEach(el => {
    const key  = el.dataset.i18nKey
    const vars = el.dataset.i18nVars ? JSON.parse(el.dataset.i18nVars) : {}
    el.textContent = t(key, vars)
  })
}

import i18n from 'i18next'
import { initReactI18next } from 'react-i18next'
import en from './locales/en.json'

/** Every user-visible string goes through `t()` and lives in `locales/`.
 *  English is the only locale today; adding another is a JSON file plus one
 *  line in `resources` — no component changes. */
export const resources = { en: { translation: en } } as const

export const defaultLanguage = 'en'

void i18n.use(initReactI18next).init({
  resources,
  lng: defaultLanguage,
  fallbackLng: defaultLanguage,
  interpolation: { escapeValue: false },
})

export default i18n

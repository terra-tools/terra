import { useTranslation } from 'react-i18next'
import { repoUrl } from '../data/releases'

const LINKS = [
  { key: 'agents', href: `${repoUrl}/blob/main/docs/AGENTS.md` },
  { key: 'architecture', href: `${repoUrl}/blob/main/docs/ARCHITECTURE.md` },
  { key: 'development', href: `${repoUrl}/blob/main/docs/DEVELOPMENT.md` },
  { key: 'learn', href: `${repoUrl}#quick-start` },
] as const

export function Docs() {
  const { t } = useTranslation()

  return (
    <section id="docs" className="border-t border-white/5 px-6 py-24">
      <div className="mx-auto max-w-5xl">
        <h2 className="text-2xl font-semibold tracking-tight text-white sm:text-4xl">
          {t('docs.title')}
        </h2>

        <div className="mt-10 grid gap-4 sm:grid-cols-2">
          {LINKS.map(({ key, href }) => (
            <a
              key={key}
              href={href}
              className="group rounded-xl bg-tile-200/60 p-6 ring-1 ring-white/8 transition-colors hover:bg-tile-200 hover:ring-white/15"
            >
              <h3 className="flex items-center gap-2 text-sm font-semibold text-white">
                {t(`docs.${key}.title`)}
                <span className="text-ink-500 transition-transform group-hover:translate-x-0.5">
                  →
                </span>
              </h3>
              <p className="mt-2 text-sm leading-relaxed text-ink-300">
                {t(`docs.${key}.body`)}
              </p>
            </a>
          ))}
        </div>
      </div>
    </section>
  )
}

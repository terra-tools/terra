import { useTranslation } from 'react-i18next'

const CARDS = [
  { key: 'open', code: 'id=$(terra new --title "tests" -- bash)' },
  { key: 'drive', code: 'terra send "$id" "cargo test{Enter}"' },
  { key: 'read', code: 'terra capture "$id" --scrollback 200' },
  { key: 'watch', code: 'terra ls' },
] as const

export function Story() {
  const { t } = useTranslation()

  return (
    <section id="story" className="border-t border-white/5 px-6 py-24">
      <div className="mx-auto max-w-5xl">
        <h2 className="text-balance text-2xl font-semibold tracking-tight text-white sm:text-4xl">
          {t('story.title')}
        </h2>
        <p className="mt-4 max-w-2xl text-pretty leading-relaxed text-ink-300">
          {t('story.subtitle')}
        </p>

        <div className="mt-12 grid gap-4 sm:grid-cols-2">
          {CARDS.map(({ key, code }) => (
            <div
              key={key}
              className="rounded-xl bg-tile-200/60 p-6 ring-1 ring-white/8 transition-colors hover:ring-white/15"
            >
              <h3 className="text-sm font-semibold text-white">
                {t(`story.${key}.title`)}
              </h3>
              <p className="mt-2 text-sm leading-relaxed text-ink-300">
                {t(`story.${key}.body`)}
              </p>
              <code className="mt-5 block overflow-x-auto rounded-lg bg-tile-300/80 px-3 py-2.5 font-mono text-[12px] whitespace-pre text-steel-100 ring-1 ring-white/8">
                {code}
              </code>
            </div>
          ))}
        </div>
      </div>
    </section>
  )
}

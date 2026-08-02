import { Trans, useTranslation } from 'react-i18next'
import { TerraWindow } from './TerraWindow'
import { PlatformIcon } from './icons'
import { useDownloadAction } from './download-action'
import {
  ALL_PLATFORMS,
  useRelease,
  useTarget,
  type PlatformId,
} from '../data/releases'

export function Hero() {
  const { t } = useTranslation()
  const state = useRelease()
  const { platform } = useTarget()
  const name = t(`install.platform.${platform}`)
  const download = useDownloadAction(platform)

  return (
    <section className="halo relative isolate overflow-hidden px-6 pt-16 pb-24 sm:pt-24">
      <div className="mx-auto max-w-5xl text-center">
        {state.isMock && (
          <span className="mb-6 inline-flex items-center gap-2 rounded-full bg-white/5 px-3 py-1 text-xs font-medium text-ink-300 ring-1 ring-white/10">
            <span className="size-1.5 rounded-full bg-steel-500" />
            {state.status === 'loading'
              ? t('hero.badge.loading')
              : t(`hero.badge.${state.reason}`)}
          </span>
        )}

        <h1 className="text-balance text-4xl font-semibold tracking-tight text-white sm:text-6xl">
          {t('hero.title')}
        </h1>

        <p className="mx-auto mt-6 max-w-2xl text-pretty text-base leading-relaxed text-ink-300 sm:text-lg">
          <Trans
            i18nKey="hero.subtitle"
            components={[
              <span key="0" />,
              <code
                key="1"
                className="rounded bg-white/8 px-1.5 py-0.5 font-mono text-[0.9em] text-ink-100"
              />,
            ]}
          />
        </p>

        <div className="mt-9 flex flex-wrap items-center justify-center gap-3">
          <a
            href={download.href}
            onClick={download.onClick}
            className="inline-flex items-center gap-2 rounded-lg bg-steel-700 px-5 py-2.5 text-sm font-medium text-white shadow-lg shadow-steel-700/25 transition-colors hover:bg-steel-500"
          >
            <PlatformIcon platform={platform} className="size-4" />
            {t('hero.primaryCta', { platform: name })}
          </a>
          <a
            href="#story"
            className="rounded-lg px-5 py-2.5 text-sm font-medium text-ink-100 ring-1 ring-white/15 transition-colors hover:bg-white/5"
          >
            {t('hero.secondaryCta')}
          </a>
        </div>

        <p className="mt-4 text-xs text-ink-500">{t('hero.note')}</p>

        {state.status === 'ready' && (
          <p className="mt-3 font-mono text-xs text-ink-500">
            {t('hero.version', { version: state.release.version })}
          </p>
        )}

        {/* all three platforms as a quiet row of marks; each downloads
            directly (Linux opens the install modal) */}
        <div className="mt-3 flex items-center justify-center gap-4">
          {ALL_PLATFORMS.map((id) => (
            <PlatformMark key={id} id={id} />
          ))}
        </div>
      </div>

      <div className="mx-auto mt-16 max-w-4xl">
        <TerraWindow />
      </div>
    </section>
  )
}

function PlatformMark({ id }: { id: PlatformId }) {
  const { t } = useTranslation()
  const download = useDownloadAction(id)

  return (
    <a
      href={download.href}
      onClick={download.onClick}
      aria-label={t('hero.primaryCta', {
        platform: t(`install.platform.${id}`),
      })}
      className="text-ink-500 transition-colors hover:text-ink-300"
    >
      <PlatformIcon platform={id} className="size-5" />
    </a>
  )
}

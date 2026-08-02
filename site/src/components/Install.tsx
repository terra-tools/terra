import { useEffect, useState } from 'react'
import { Trans, useTranslation } from 'react-i18next'
import { CopyLine } from './CopyLine'
import {
  ALL_PLATFORMS,
  featuredDownload,
  releasesUrl,
  useRelease,
  useTarget,
  type Download,
  type PlatformId,
} from '../data/releases'

export function Install() {
  const { t } = useTranslation()
  const state = useRelease()
  const target = useTarget()
  const [selected, setSelected] = useState<PlatformId>(target.platform)

  // The synchronous guess picks the tab on first paint; the async hints only
  // ever change `arch`, so a visitor who has clicked keeps their tab.
  useEffect(() => setSelected(target.platform), [target.platform])

  const release = state.status === 'ready' ? state.release : null
  const platform = release?.platforms.find((p) => p.id === selected)
  const featured = featuredDownload(platform, target.arch)
  const rest = platform?.downloads.filter((d) => d !== featured) ?? []

  return (
    <section id="install" className="border-t border-white/5 px-6 py-24">
      <div className="mx-auto max-w-3xl">
        <h2 className="text-2xl font-semibold tracking-tight text-white sm:text-4xl">
          {t('install.title')}
        </h2>
        <p className="mt-4 text-ink-300">{t('install.subtitle')}</p>

        {state.isMock && (
          <p className="mt-6 rounded-lg bg-amber-400/8 px-4 py-3 text-sm text-amber-200/90 ring-1 ring-amber-400/20">
            {state.status === 'loading'
              ? t('install.notice.loading')
              : t(`install.notice.${state.reason}`)}
          </p>
        )}

        {/* platform tabs */}
        <div className="mt-10 flex gap-1 rounded-lg bg-tile-200/70 p-1 ring-1 ring-white/8">
          {ALL_PLATFORMS.map((id) => (
            <button
              key={id}
              type="button"
              onClick={() => setSelected(id)}
              className={[
                'flex-1 rounded-md px-4 py-2 text-sm font-medium transition-colors',
                id === selected
                  ? 'bg-chrome-active text-white'
                  : 'text-ink-300 hover:bg-white/5',
              ].join(' ')}
            >
              {t(`install.platform.${id}`)}
            </button>
          ))}
        </div>

        {/* downloads for the selected platform, or an honest placeholder */}
        <div className="mt-4 space-y-2">
          {featured && <DownloadRow download={featured} featured />}
          {rest.map((d) => (
            <DownloadRow key={d.file} download={d} />
          ))}

          {!featured && (
            <a
              href={releasesUrl}
              className="flex items-center justify-between gap-4 rounded-lg bg-tile-200/40 px-5 py-4 ring-1 ring-white/8 transition-colors hover:bg-tile-200/70 hover:ring-white/15"
            >
              <span className="min-w-0">
                <span className="block text-sm font-medium text-white">
                  {t('install.comingSoon')}
                </span>
                <span className="block text-xs text-ink-500">
                  {state.status === 'ready'
                    ? t('install.noAssetForPlatform', {
                        platform: t(`install.platform.${selected}`),
                      })
                    : t('install.checkReleases')}
                </span>
              </span>
              <span className="shrink-0 rounded-md bg-white/8 px-3 py-1.5 text-xs font-medium text-ink-100">
                {t('install.viewReleases')}
              </span>
            </a>
          )}
        </div>

        {/* one-line installers */}
        <div className="mt-12 space-y-4">
          <h3 className="text-sm font-semibold text-white">
            {t('install.cliTitle')}
          </h3>
          <CopyLine command="curl -fsSL https://terra-tools.github.io/terra/install.sh | sh" />
          <p className="text-xs text-ink-500">
            <Trans
              i18nKey="install.cliNote"
              components={[
                <span key="0" />,
                <code key="1" className="font-mono text-ink-300" />,
              ]}
            />
          </p>

          <h3 className="pt-4 text-sm font-semibold text-white">
            {t('install.brewTitle')}
          </h3>
          <CopyLine command="brew install --cask terra-tools/terra/terra" />
        </div>

        <p className="mt-10 text-center font-mono text-xs text-ink-500">
          {release
            ? t('install.version', { version: release.version })
            : t('install.versionUnknown')}
        </p>
      </div>
    </section>
  )
}

const ARCH_LABEL: Record<string, string> = {
  arm64: 'Apple silicon / arm64',
  x64: 'Intel / x86_64',
  universal: 'Universal',
}

function DownloadRow({
  download,
  featured = false,
}: {
  download: Download
  featured?: boolean
}) {
  const { t } = useTranslation()
  const arch = ARCH_LABEL[download.arch]
  const meta = [download.file, download.size].filter(Boolean).join(' · ')

  return (
    <a
      href={download.url}
      className={[
        'flex items-center justify-between gap-4 rounded-lg px-5 py-4 ring-1 transition-colors',
        featured
          ? 'bg-tile-200 ring-white/15 hover:ring-white/25'
          : 'bg-tile-200/60 ring-white/8 hover:bg-tile-200 hover:ring-white/15',
      ].join(' ')}
    >
      <span className="min-w-0">
        <span className="block text-sm font-medium text-white">
          {t(download.labelKey)}
          {arch && <span className="text-ink-300"> · {arch}</span>}
        </span>
        <span className="block truncate font-mono text-xs text-ink-500">
          {meta}
        </span>
      </span>
      <span
        className={[
          'shrink-0 rounded-md px-3 py-1.5 text-xs font-medium',
          featured ? 'bg-steel-700 text-white' : 'bg-white/8 text-ink-100',
        ].join(' ')}
      >
        {t('install.download')}
      </span>
    </a>
  )
}

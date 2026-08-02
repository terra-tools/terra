import { useTranslation } from 'react-i18next'
import { Logo } from './Logo'
import { repoUrl } from '../data/releases'

export function Nav() {
  const { t } = useTranslation()

  return (
    <header className="sticky top-0 z-50 border-b border-white/5 bg-tile-300/80 backdrop-blur-md">
      <div className="mx-auto flex max-w-5xl items-center gap-4 px-6 py-3">
        <a href="#" className="flex items-center gap-2.5">
          <Logo className="size-7 rounded-md" />
          <span className="font-mono text-sm font-medium text-white">terra</span>
        </a>

        <nav className="ml-auto flex items-center gap-5 text-sm text-ink-300">
          <a href="#docs" className="hidden transition-colors hover:text-white sm:block">
            {t('nav.docs')}
          </a>
          <a href={repoUrl} className="hidden transition-colors hover:text-white sm:block">
            {t('nav.github')}
          </a>
          <a
            href="#install"
            className="rounded-md bg-white/8 px-3 py-1.5 text-xs font-medium text-white transition-colors hover:bg-white/15"
          >
            {t('nav.download')}
          </a>
        </nav>
      </div>
    </header>
  )
}

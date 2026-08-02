import { useTranslation } from 'react-i18next'
import { Logo } from './Logo'
import { repoUrl } from '../data/releases'

export function Footer() {
  const { t } = useTranslation()

  return (
    <footer className="border-t border-white/5 px-6 py-12">
      <div className="mx-auto flex max-w-5xl flex-wrap items-center justify-between gap-6">
        <div className="flex items-center gap-3">
          <Logo className="size-8 rounded-lg" />
          <div>
            <p className="text-sm font-medium text-white">Terra</p>
            <p className="text-xs text-ink-500">{t('footer.tagline')}</p>
          </div>
        </div>

        <nav className="flex items-center gap-6 text-xs text-ink-300">
          <a href={repoUrl} className="transition-colors hover:text-white">
            {t('footer.source')}
          </a>
          <a href={`${repoUrl}/issues`} className="transition-colors hover:text-white">
            {t('footer.issues')}
          </a>
          <span className="text-ink-500">{t('footer.license')}</span>
        </nav>
      </div>
    </footer>
  )
}

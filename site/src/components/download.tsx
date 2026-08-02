import { useEffect, useState, type ReactNode } from 'react'
import { useTranslation } from 'react-i18next'
import { CopyLine } from './CopyLine'
import { LinuxModalContext } from './download-action'

/** There is no install section on the page: the hero and nav buttons download
 *  directly via useDownloadAction (download-action.ts). This file owns the one
 *  piece of UI that flow needs — the Linux install modal, which is just the
 *  quick-install command (install.sh grabs the right artifacts itself). */

export function DownloadProvider({ children }: { children: ReactNode }) {
  const [linuxOpen, setLinuxOpen] = useState(false)

  return (
    <LinuxModalContext.Provider value={() => setLinuxOpen(true)}>
      {children}
      {linuxOpen && <LinuxInstallModal onClose={() => setLinuxOpen(false)} />}
    </LinuxModalContext.Provider>
  )
}

function LinuxInstallModal({ onClose }: { onClose: () => void }) {
  const { t } = useTranslation()

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose()
    }
    window.addEventListener('keydown', onKey)
    document.body.style.overflow = 'hidden'
    return () => {
      window.removeEventListener('keydown', onKey)
      document.body.style.overflow = ''
    }
  }, [onClose])

  return (
    <div
      className="fixed inset-0 z-100 flex items-center justify-center bg-black/60 p-4 backdrop-blur-sm"
      onClick={onClose}
    >
      <div
        role="dialog"
        aria-modal="true"
        aria-label={t('linuxModal.title')}
        className="w-full max-w-2xl rounded-2xl bg-tile-300 p-6 shadow-2xl ring-1 ring-white/10 sm:p-8"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex items-start justify-between gap-4">
          <h2 className="text-xl font-semibold tracking-tight text-white sm:text-2xl">
            {t('linuxModal.title')}
          </h2>
          <button
            type="button"
            onClick={onClose}
            aria-label={t('linuxModal.close')}
            className="rounded-md px-2 py-1 text-ink-500 transition-colors hover:bg-white/5 hover:text-white"
          >
            ✕
          </button>
        </div>

        <h3 className="mt-6 text-sm font-semibold text-white">
          {t('linuxModal.quickTitle')}
        </h3>
        <div className="mt-3 mb-2">
          <CopyLine command="curl -fsSL https://terra-tools.github.io/terra/install.sh | sh" />
        </div>
      </div>
    </div>
  )
}

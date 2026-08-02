import { useCallback, useEffect, useState } from 'react'
import { useTranslation } from 'react-i18next'

/** A one-line shell command with a copy button. */
export function CopyLine({ command }: { command: string }) {
  const { t } = useTranslation()
  const [copied, setCopied] = useState(false)

  useEffect(() => {
    if (!copied) return
    const timer = setTimeout(() => setCopied(false), 1800)
    return () => clearTimeout(timer)
  }, [copied])

  const copy = useCallback(() => {
    void navigator.clipboard.writeText(command).then(() => setCopied(true))
  }, [command])

  return (
    <div className="flex items-center gap-3 rounded-lg bg-tile-300/80 px-4 py-3 ring-1 ring-white/10">
      <code className="min-w-0 flex-1 overflow-x-auto font-mono text-[13px] whitespace-pre text-ink-100 [scrollbar-width:none] [&::-webkit-scrollbar]:hidden">
        <span className="text-ink-500 select-none">$ </span>
        {command}
      </code>
      <button
        type="button"
        onClick={copy}
        className="shrink-0 rounded-md px-2.5 py-1 text-xs font-medium text-ink-300 ring-1 ring-white/15 transition-colors hover:bg-white/5 hover:text-ink-100"
      >
        {copied ? t('install.copied') : t('install.copy')}
      </button>
    </div>
  )
}

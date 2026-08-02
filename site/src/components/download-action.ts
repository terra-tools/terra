import { createContext, useContext, type MouseEvent } from 'react'
import {
  featuredDownload,
  releasesUrl,
  useRelease,
  useTarget,
  type PlatformId,
} from '../data/releases'

/** Set by DownloadProvider (download.tsx); opens the Linux install modal. */
export const LinuxModalContext = createContext<() => void>(() => {})

export interface DownloadAction {
  href: string
  onClick?: (e: MouseEvent<HTMLAnchorElement>) => void
}

/** What clicking "Download for <platform>" should do right now.
 *
 *  macOS and Windows link straight to the featured asset (or the releases
 *  page while none is published); Linux opens a small modal instead — vibe
 *  style — because "install" there is a command, not a double-click. */
export function useDownloadAction(platform: PlatformId): DownloadAction {
  const state = useRelease()
  const { arch } = useTarget()
  const openLinuxModal = useContext(LinuxModalContext)

  if (platform === 'linux') {
    return {
      href: '#',
      onClick: (e) => {
        e.preventDefault()
        openLinuxModal()
      },
    }
  }

  const release = state.status === 'ready' ? state.release : null
  const entry = release?.platforms.find((p) => p.id === platform)
  // The bare-CLI tarball never headlines: install.sh covers the CLI.
  const installers = entry && {
    ...entry,
    downloads: entry.downloads.filter((d) => d.kind !== 'cli'),
  }
  const featured = featuredDownload(installers, arch)
  return { href: featured?.url ?? releasesUrl }
}

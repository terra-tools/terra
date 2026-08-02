/** The only module that knows about releases.
 *
 *  Data comes from the GitHub Releases API at runtime — there is no build-time
 *  step and no hardcoded version. Everything downstream reads this module.
 *
 *  Three things make that safe on a static page:
 *    * the repo may have **no release at all** (that is the state today), so
 *      `unavailable` is a first-class result, not an error path;
 *    * the unauthenticated API allows 60 requests/hour/IP, so answers —
 *      including negative ones — are cached in `sessionStorage`;
 *    * a rate-limited or offline visitor still gets working links, pointed at
 *      the repo's releases page instead of at a specific asset.
 */

import { useEffect, useState } from 'react'

const REPO = 'https://github.com/terra-tools/terra'
const API = 'https://api.github.com/repos/terra-tools/terra/releases/latest'

export const repoUrl = REPO
/** Always safe to link to, release or not. */
export const releasesUrl = `${REPO}/releases`

// ---------------------------------------------------------------------------
// types
// ---------------------------------------------------------------------------

export type PlatformId = 'macos' | 'windows' | 'linux'

export type Arch = 'arm64' | 'x64' | 'universal' | 'unknown'

/** Coarse asset flavour, derived from the file name. Drives the button label. */
export type AssetKind =
  | 'dmg'
  | 'app'
  | 'cli'
  | 'exe'
  | 'msi'
  | 'deb'
  | 'rpm'
  | 'appimage'
  | 'archive'

export interface Download {
  kind: AssetKind
  /** i18n key for the button label, e.g. `install.asset.dmg`. */
  labelKey: string
  /** File name as attached to the release. */
  file: string
  arch: Arch
  /** Pre-formatted size, e.g. "18.2 MB". */
  size: string
  sizeBytes: number
  url: string
}

export interface Platform {
  id: PlatformId
  downloads: Download[]
}

export interface Release {
  version: string
  publishedAt: string
  /** The release's own page on GitHub. */
  htmlUrl: string
  platforms: Platform[]
}

export type UnavailableReason =
  /** The API answered, and the repo has published nothing yet. */
  | 'no-release'
  /** 403/429 from the unauthenticated API. */
  | 'rate-limited'
  /** Offline, DNS failure, CORS, blocked request. */
  | 'network'

/** `isMock` is carried on every variant so the banner never re-derives it:
 *  true means "the page cannot offer a real published artifact right now". */
export type ReleaseState =
  | { status: 'loading'; isMock: true }
  | { status: 'ready'; isMock: false; release: Release }
  | { status: 'unavailable'; isMock: true; reason: UnavailableReason }

const LOADING: ReleaseState = { status: 'loading', isMock: true }

// ---------------------------------------------------------------------------
// asset -> platform mapping
// ---------------------------------------------------------------------------

interface ApiAsset {
  name: string
  size: number
  browser_download_url: string
}

interface ApiRelease {
  tag_name: string
  name: string | null
  published_at: string
  html_url: string
  draft: boolean
  prerelease: boolean
  assets: ApiAsset[]
}

function archOf(name: string): Arch {
  const n = name.toLowerCase()
  if (/universal/.test(n)) return 'universal'
  if (/(aarch64|arm64|apple[-_]silicon)/.test(n)) return 'arm64'
  if (/(x86[-_]?64|x64|amd64|intel)/.test(n)) return 'x64'
  return 'unknown'
}

/** Classify one asset by file name. Returns null for things the site should
 *  not offer as a download (checksums, signatures, update manifests). */
function classify(
  asset: ApiAsset,
): { platform: PlatformId; kind: AssetKind } | null {
  const n = asset.name.toLowerCase()

  if (/\.(sig|asc|sha256|sha512|pem|json|txt)$/.test(n)) return null

  const isCli = /(^|[-_.])cli([-_.]|$)/.test(n)

  if (/\.dmg$/.test(n)) return { platform: 'macos', kind: 'dmg' }
  if (/\.app\.tar\.(gz|xz|bz2)$/.test(n)) return { platform: 'macos', kind: 'app' }
  if (/\.msi$/.test(n)) return { platform: 'windows', kind: 'msi' }
  if (/\.exe$/.test(n)) return { platform: 'windows', kind: 'exe' }
  if (/\.deb$/.test(n)) return { platform: 'linux', kind: 'deb' }
  if (/\.rpm$/.test(n)) return { platform: 'linux', kind: 'rpm' }
  if (/\.appimage$/.test(n)) return { platform: 'linux', kind: 'appimage' }

  // Plain archives only tell us the platform through their name.
  if (/\.(tar\.(gz|xz|bz2)|tgz|zip)$/.test(n)) {
    const platform: PlatformId | null = /(macos|darwin|apple|osx)/.test(n)
      ? 'macos'
      : /(windows|win32|win64|[-_]win[-_.])/.test(n)
        ? 'windows'
        : /(linux|musl|gnu)/.test(n)
          ? 'linux'
          : null
    if (!platform) return null
    return { platform, kind: isCli ? 'cli' : 'archive' }
  }

  return null
}

function formatSize(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return ''
  const mb = bytes / 1_000_000
  if (mb >= 1) return `${mb.toFixed(1)} MB`
  return `${Math.max(1, Math.round(bytes / 1000))} KB`
}

/** Installers before archives before bare CLIs — the first entry of a platform
 *  is what the page features. */
const KIND_RANK: Record<AssetKind, number> = {
  dmg: 0,
  msi: 1,
  exe: 2,
  deb: 3,
  appimage: 4,
  rpm: 5,
  archive: 6,
  app: 7,
  cli: 8,
}

/** Tab order in the UI. Every platform gets a tab whether or not the current
 *  release has an artifact for it, so nobody's OS silently disappears. */
export const ALL_PLATFORMS: readonly PlatformId[] = ['macos', 'windows', 'linux']

/** Exported for the sake of being testable against a captured API payload. */
export function mapRelease(api: ApiRelease): Release | null {
  const buckets = new Map<PlatformId, Download[]>()

  for (const asset of api.assets ?? []) {
    const hit = classify(asset)
    if (!hit) continue
    const list = buckets.get(hit.platform) ?? []
    list.push({
      kind: hit.kind,
      labelKey: `install.asset.${hit.kind}`,
      file: asset.name,
      arch: archOf(asset.name),
      size: formatSize(asset.size),
      sizeBytes: asset.size,
      url: asset.browser_download_url,
    })
    buckets.set(hit.platform, list)
  }

  const platforms: Platform[] = ALL_PLATFORMS.filter((id) =>
    buckets.has(id),
  ).map((id) => ({
    id,
    downloads: (buckets.get(id) ?? []).sort(
      (a, b) =>
        KIND_RANK[a.kind] - KIND_RANK[b.kind] || a.file.localeCompare(b.file),
    ),
  }))

  // A release with no recognisable artifact is no better than no release.
  if (platforms.length === 0) return null

  return {
    version: (api.tag_name || api.name || '').replace(/^v/, ''),
    publishedAt: api.published_at,
    htmlUrl: api.html_url || releasesUrl,
    platforms,
  }
}

// ---------------------------------------------------------------------------
// fetching + caching
// ---------------------------------------------------------------------------

const CACHE_KEY = 'terra:release:v1'
/** Real releases change rarely; a "nothing yet" answer should be re-checked
 *  sooner, but never often enough to burn the 60/hr budget. */
const TTL_READY = 30 * 60_000
const TTL_UNAVAILABLE = 5 * 60_000

type SettledState = Exclude<ReleaseState, { status: 'loading' }>

interface CacheEntry {
  at: number
  state: SettledState
}

function readCache(): SettledState | null {
  try {
    const raw = globalThis.sessionStorage?.getItem(CACHE_KEY)
    if (!raw) return null
    const entry = JSON.parse(raw) as CacheEntry
    if (!entry?.at || !entry.state?.status) return null
    const ttl = entry.state.status === 'ready' ? TTL_READY : TTL_UNAVAILABLE
    if (Date.now() - entry.at > ttl) return null
    return entry.state
  } catch {
    return null
  }
}

function writeCache(state: SettledState): void {
  try {
    globalThis.sessionStorage?.setItem(
      CACHE_KEY,
      JSON.stringify({ at: Date.now(), state } satisfies CacheEntry),
    )
  } catch {
    // Private mode, storage disabled, quota — caching is only an optimisation.
  }
}

/** Never throws. Every failure becomes an `unavailable` state with a reason. */
export async function fetchRelease(signal?: AbortSignal): Promise<SettledState> {
  let res: Response
  try {
    res = await fetch(API, {
      signal,
      headers: { Accept: 'application/vnd.github+json' },
    })
  } catch {
    return { status: 'unavailable', isMock: true, reason: 'network' }
  }

  // 404 is the honest answer for "this repo has never published a release".
  if (res.status === 404) {
    return { status: 'unavailable', isMock: true, reason: 'no-release' }
  }

  if (res.status === 403 || res.status === 429) {
    const remaining = res.headers.get('x-ratelimit-remaining')
    const limited = res.status === 429 || remaining === '0'
    return {
      status: 'unavailable',
      isMock: true,
      reason: limited ? 'rate-limited' : 'network',
    }
  }

  if (!res.ok) return { status: 'unavailable', isMock: true, reason: 'network' }

  let api: ApiRelease
  try {
    api = (await res.json()) as ApiRelease
  } catch {
    return { status: 'unavailable', isMock: true, reason: 'network' }
  }

  const release = api && !api.draft ? mapRelease(api) : null
  if (!release) return { status: 'unavailable', isMock: true, reason: 'no-release' }
  return { status: 'ready', isMock: false, release }
}

/** The single subscription point for release data.
 *
 *  Starts in `loading` (banner up, honest "checking…" copy) and resolves to
 *  `ready` or `unavailable`. The result is shared through `sessionStorage`, so
 *  a second component — or a reload in the same tab — costs no request. */
export function useRelease(): ReleaseState {
  const [state, setState] = useState<ReleaseState>(() => readCache() ?? LOADING)

  useEffect(() => {
    if (state.status !== 'loading') return
    const ctrl = new AbortController()
    let live = true
    void fetchRelease(ctrl.signal).then((next) => {
      if (!live) return
      writeCache(next)
      setState(next)
    })
    return () => {
      live = false
      ctrl.abort()
    }
    // The only transition out of `loading` is this effect, so it runs once.

  }, [state.status])

  return state
}

// ---------------------------------------------------------------------------
// visitor platform detection
// ---------------------------------------------------------------------------

export interface Target {
  platform: PlatformId
  arch: Arch
}

interface UaData {
  platform?: string
  getHighEntropyValues?: (hints: string[]) => Promise<{ architecture?: string }>
}

function uaData(): UaData | undefined {
  if (typeof navigator === 'undefined') return undefined
  return (navigator as Navigator & { userAgentData?: UaData }).userAgentData
}

/** Synchronous best guess — good enough to pick the tab on first paint.
 *  Wrong guesses cost nothing: every platform stays one click away. */
export function detectPlatform(): PlatformId {
  if (typeof navigator === 'undefined') return 'macos'

  const hinted = uaData()?.platform?.toLowerCase()
  if (hinted) {
    if (hinted.includes('win')) return 'windows'
    if (hinted.includes('mac')) return 'macos'
    if (hinted.includes('linux') || hinted.includes('android')) return 'linux'
    if (hinted.includes('chrome os')) return 'linux'
  }

  const ua = `${navigator.userAgent} ${navigator.platform ?? ''}`
  if (/Win/i.test(ua)) return 'windows'
  if (/Mac|iPhone|iPad|iPod/i.test(ua)) return 'macos'
  if (/Linux|X11|Android|CrOS/i.test(ua)) return 'linux'
  return 'macos'
}

/** Synchronous arch guess. macOS is the hard case: every browser still reports
 *  "Intel Mac OS X" in its UA string, so Apple silicon is assumed there unless
 *  the high-entropy hints (Chromium only) say otherwise — see `useTarget`. */
export function detectArch(platform: PlatformId): Arch {
  if (typeof navigator === 'undefined') return 'unknown'
  const ua = navigator.userAgent
  if (/aarch64|arm64|armv8/i.test(ua)) return 'arm64'
  if (/Win64|x86_64|x64;|WOW64|amd64/i.test(ua)) return 'x64'
  if (platform === 'macos') return 'arm64'
  return 'unknown'
}

/** Detected target, refined asynchronously where the browser allows it. */
export function useTarget(): Target {
  const [target, setTarget] = useState<Target>(() => {
    const platform = detectPlatform()
    return { platform, arch: detectArch(platform) }
  })

  useEffect(() => {
    const data = uaData()
    if (!data?.getHighEntropyValues) return
    let live = true
    void data
      .getHighEntropyValues(['architecture'])
      .then((v) => {
        if (!live || !v.architecture) return
        const arch: Arch =
          v.architecture === 'arm'
            ? 'arm64'
            : v.architecture === 'x86'
              ? 'x64'
              : 'unknown'
        if (arch !== 'unknown') setTarget((prev) => ({ ...prev, arch }))
      })
      .catch(() => {
        // Hints are a nicety; the synchronous guess already stands.
      })
    return () => {
      live = false
    }
  }, [])

  return target
}

/** The download to feature for `arch` — an exact match if the release has one,
 *  else a universal build, else the platform's first (highest-ranked) asset. */
export function featuredDownload(
  platform: Platform | undefined,
  arch: Arch,
): Download | undefined {
  if (!platform) return undefined
  return (
    platform.downloads.find((d) => d.arch === arch) ??
    platform.downloads.find((d) => d.arch === 'universal') ??
    platform.downloads[0]
  )
}

import { useTranslation } from 'react-i18next'

/** A faithful mock of the terra window, built from the app's own constants
 *  (crates/terra-app/src/ui.rs). It doubles as the preview of the updater
 *  affordance: the "↑ Update" pill sits at the trailing edge of the tab bar,
 *  left of the + button, which is where issue #1 puts it. */

interface TabProps {
  label: string
  shortcut: string
  active?: boolean
  bell?: boolean
}

function Tab({ label, shortcut, active, bell }: TabProps) {
  return (
    <div
      className={[
        'flex h-6 min-w-0 flex-1 items-center justify-center gap-2 rounded-md px-3',
        'text-[11px] tracking-tight select-none',
        active
          ? 'bg-chrome-active text-ink-100 ring-1 ring-chrome-edge/70'
          : 'bg-chrome-tab text-ink-300',
      ].join(' ')}
    >
      <span className="truncate">
        {bell ? '🔔 ' : ''}
        {label}
      </span>
      <span className="shrink-0 text-ink-500">{shortcut}</span>
    </div>
  )
}

function TrafficLights() {
  return (
    <div className="flex items-center gap-2">
      <span className="size-3 rounded-full bg-[#ff5f57]" />
      <span className="size-3 rounded-full bg-[#febc2e]" />
      <span className="size-3 rounded-full bg-[#28c840]" />
    </div>
  )
}

/** One line of the fake session. `tone` maps to the terminal palette. */
type Line = { text: string; tone?: 'prompt' | 'dim' | 'accent' | 'ok' }

const SESSION: Line[] = [
  { text: '➜  ~/terra  id=$(terra new --title "tests" -- cargo watch)', tone: 'prompt' },
  { text: '3' },
  { text: '➜  ~/terra  terra send 3 "cargo test{Enter}"', tone: 'prompt' },
  { text: '➜  ~/terra  terra capture 3', tone: 'prompt' },
  { text: '   Compiling terra-app v0.1.0', tone: 'dim' },
  { text: '    Finished test profile in 4.21s', tone: 'dim' },
  { text: 'test tabs::a_new_tab_gets_the_next_id ... ok', tone: 'ok' },
  { text: 'test ipc::capture_trims_the_blank_tail ... ok', tone: 'ok' },
  { text: 'test result: ok. 34 passed; 0 failed', tone: 'ok' },
  { text: '➜  ~/terra  ', tone: 'prompt' },
]

const TONE: Record<NonNullable<Line['tone']>, string> = {
  prompt: 'text-[#7fb069]',
  dim: 'text-ink-500',
  accent: 'text-steel-100',
  ok: 'text-[#8ec07c]',
}

export function TerraWindow() {
  const { t } = useTranslation()

  return (
    <div className="overflow-hidden rounded-xl bg-tile-200 shadow-[0_40px_80px_-20px_rgba(0,0,0,0.8)] ring-1 ring-white/10">
      {/* titlebar. The update pill lives here, at the trailing edge — a native
          titlebar accessory on macOS, the way Ghostty does it. */}
      <div className="flex items-center gap-3 bg-chrome-bar px-4 py-2">
        <TrafficLights />
        <span className="flex-1 text-center text-[11px] font-medium text-ink-300">
          Terra
        </span>
        <button
          type="button"
          title={t('window.updateTitle')}
          className="flex h-[22px] shrink-0 items-center gap-1.5 rounded-full bg-steel-700 pr-3 pl-2.5 text-[11px] font-medium text-white transition-colors hover:bg-steel-500"
        >
          <svg
            viewBox="0 0 12 12"
            className="size-3 fill-none stroke-current"
            strokeWidth="1.6"
          >
            <path
              d="M6 9.5V2.5M6 2.5 3 5.5M6 2.5 9 5.5"
              strokeLinecap="round"
              strokeLinejoin="round"
            />
          </svg>
          {t('window.update')}
        </button>
      </div>

      {/* tab bar — 32px tall, 4px vertical padding, no gap between pills */}
      <div className="flex items-center gap-1.5 border-b border-chrome-sep bg-chrome-bar px-1.5 pb-1">
        <Tab label={t('window.tabOne')} shortcut="⌘1" active />
        <Tab label={t('window.tabTwo')} shortcut="⌘2" />
        <Tab label={t('window.tabThree')} shortcut="⌘3" bell />

        <button
          type="button"
          className="flex size-6 shrink-0 items-center justify-center rounded-full text-ink-300 ring-1 ring-[#45454a] transition-colors hover:bg-[#2e2e33]"
          aria-hidden
        >
          <span className="-mt-px text-sm leading-none">+</span>
        </button>
      </div>

      {/* terminal */}
      <div className="bg-term-bg px-4 py-3 font-mono text-[12px] leading-[1.65] text-term-fg sm:text-[13px]">
        {SESSION.map((line, i) => (
          <div key={i} className={`whitespace-pre ${line.tone ? TONE[line.tone] : ''}`}>
            {line.text}
            {i === SESSION.length - 1 && (
              <span className="ml-px inline-block h-[1.1em] w-[0.6em] translate-y-[0.2em] bg-term-fg/80" />
            )}
          </div>
        ))}
      </div>
    </div>
  )
}

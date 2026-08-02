/** The app icon, inlined from crates/terra-app/assets/icon/terra.svg so the
 *  site never drifts from the shipped icon. Simplified: the two heavy blur
 *  filters are dropped at small sizes where they only cost bytes. */
export function Logo({ className = 'size-10' }: { className?: string }) {
  return (
    <svg viewBox="0 0 1024 1024" className={className} aria-hidden>
      <defs>
        <linearGradient id="terra-tile" x1="0" y1="0" x2="0" y2="1">
          <stop offset="0" stopColor="#232329" />
          <stop offset="0.5" stopColor="#17171c" />
          <stop offset="1" stopColor="#0e0e12" />
        </linearGradient>
        <linearGradient id="terra-steel" x1="0" y1="0" x2="0" y2="1">
          <stop offset="0" stopColor="#bfdbfe" />
          <stop offset="1" stopColor="#1d4ed8" />
        </linearGradient>
      </defs>
      <rect x="100" y="100" width="824" height="824" rx="183" fill="url(#terra-tile)" />
      <rect
        x="239.5"
        y="248.7"
        width="545"
        height="545"
        rx="152.6"
        fill="url(#terra-steel)"
      />
      <g transform="translate(455.7 509.6) scale(0.828)">
        <path
          d="M-68 -96 L68 0 L-68 96"
          stroke="#ffffff"
          strokeWidth="40"
          fill="none"
          strokeLinecap="round"
          strokeLinejoin="round"
        />
        <path
          d="M116 124 L204 124"
          stroke="#ffffff"
          strokeWidth="40"
          fill="none"
          strokeLinecap="round"
        />
      </g>
    </svg>
  )
}

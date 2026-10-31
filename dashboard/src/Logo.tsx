/** The mark follows hypout's icon: a framed dark tile, strokes in the foreground colour, and one amber accent. It uses the theme tokens, so it works in both themes. */
export function Logo({ size = 28 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 32 32" fill="none" aria-hidden="true">
      <rect x="1.5" y="1.5" width="29" height="29" rx="8" fill="var(--hy-bg)" stroke="var(--hy-ink)" strokeOpacity="0.35" strokeWidth="1.5" />
      <path d="M18.5 6.5 10 17.5h5.6L13.8 25.5 22 14.2h-5.4z" stroke="var(--hy-ink)" strokeWidth="1.8" strokeLinejoin="round" strokeLinecap="round" />
      <circle cx="23.5" cy="8.5" r="2.2" fill="var(--hy-live)" />
    </svg>
  );
}

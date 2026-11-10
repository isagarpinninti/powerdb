/** The PowerDB mark: a P made of a stem and a closed bowl, with an amber bar beneath it (the file that belongs to the record), on a
 *  dark tile. The colours are fixed, not theme tokens: a logo does not change with the theme. See docs/brand. */
export function Logo({ size = 28 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 32 32" role="img" aria-label="PowerDB">
      <rect width="32" height="32" rx="7.5" fill="#0a0a0b" />
      <rect x="0.5" y="0.5" width="31" height="31" rx="7" fill="none" stroke="#fff" strokeOpacity="0.14" />
      <rect x="8" y="6" width="4.5" height="20" rx="2.25" fill="#fafafa" />
      <path fill="#fafafa" fillRule="evenodd" d="M10.25 6H17.5a7 7 0 0 1 0 14H10.25zM13.5 10.5H17.5a2.5 2.5 0 0 1 0 5H13.5a1 1 0 0 1-1-1v-3a1 1 0 0 1 1-1z" />
      <rect x="14.5" y="22.5" width="10" height="3.5" rx="1.75" fill="#f59e0b" />
    </svg>
  );
}

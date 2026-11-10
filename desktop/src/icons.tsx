import type { SVGProps } from 'react';

/** One small, consistent icon set (24×24 grid, 1.75 stroke), drawn for this app so nothing depends on an icon font. */
const paths = {
  overview: 'M4 4h6.5v6.5H4zM13.5 4H20v4h-6.5zM13.5 11.5H20V20h-6.5zM4 13.5h6.5V20H4z',
  database: 'M4 6c0-1.66 3.58-3 8-3s8 1.34 8 3-3.58 3-8 3-8-1.34-8-3zM4 6v6c0 1.66 3.58 3 8 3s8-1.34 8-3V6M4 12v6c0 1.66 3.58 3 8 3s8-1.34 8-3v-6',
  backups: 'M3.5 5h17v4h-17zM5 9v10h14V9M10 13h4',
  logs: 'M4 5h16v14H4zM7.5 10l3 2-3 2M12.5 15h4',
  settings: 'M4 7h9M17 7h3M4 17h3M11 17h9M15 4.5v5M9 14.5v5',
  info: 'M12 21a9 9 0 100-18 9 9 0 000 18zM12 11v5.5M12 7.5v.01',
  play: 'M8 5v14l11-7z',
  stop: 'M7 7h10v10H7z',
  restart: 'M19.5 12a7.5 7.5 0 11-2.2-5.3M19.5 4.5v4.5H15',
  copy: 'M9 9h11v11H9zM5.5 15V4.5H15',
  folder: 'M3 6.5h6l2 2.5h10V19H3z',
  sun: 'M12 16a4 4 0 100-8 4 4 0 000 8zM12 2.5v2M12 19.5v2M2.5 12h2M19.5 12h2M5.3 5.3l1.4 1.4M17.3 17.3l1.4 1.4M5.3 18.7l1.4-1.4M17.3 6.7l1.4-1.4',
  moon: 'M20 14.5A8 8 0 019.5 4a8 8 0 1010.5 10.5z',
  search: 'M11 18.5a7.5 7.5 0 100-15 7.5 7.5 0 000 15zM20.5 20.5l-4.2-4.2',
  check: 'M5 12.5l4.5 4.5L19 7.5',
  alert: 'M12 3.5l9.5 16.5h-19zM12 10v4.5M12 17.5v.01',
  external: 'M14 4.5h5.5V10M19.5 4.5L11 13M17.5 14v5.5h-13v-13H10',
  chevron: 'M9 6l6 6-6 6',
  close: 'M6 6l12 12M18 6L6 18',
  plus: 'M12 5v14M5 12h14',
  key: 'M14.5 9.5a4 4 0 11-1.2 2.8L4 21.5v-3l2-1v-2l2-1 5.3-5.3',
  shield: 'M12 3l8 3v6c0 4.5-3.2 8-8 9-4.8-1-8-4.5-8-9V6z',
  clock: 'M12 21a9 9 0 100-18 9 9 0 000 18zM12 7v5l3 2',
  disk: 'M4 15h16v5H4zM6 15l2-9h8l2 9M16.5 17.5v.01',
  link: 'M10 14a4 4 0 005.7 0l3-3a4 4 0 00-5.7-5.7l-1 1M14 10a4 4 0 00-5.7 0l-3 3A4 4 0 0011 18.7l1-1',
} as const;

export type IconName = keyof typeof paths;

export function Icon({ name, size = 18, className, ...rest }: { name: IconName; size?: number } & Omit<SVGProps<SVGSVGElement>, 'name'>) {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.75" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true" className={className} {...rest}>
      <path d={paths[name]} />
    </svg>
  );
}

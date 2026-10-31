export type Theme = 'dark' | 'light';
const KEY = 'powerdb-theme';

/** Same rule as hypout: a saved choice wins, then the OS preference, otherwise dark. Applied before first paint so nothing flashes. */
export function initTheme(): void {
  try {
    let t = localStorage.getItem(KEY);
    if (t !== 'light' && t !== 'dark') t = matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark';
    document.documentElement.dataset.theme = t;
  } catch {
    document.documentElement.dataset.theme = 'dark';
  }
}

export const readTheme = (): Theme => (document.documentElement.dataset.theme === 'light' ? 'light' : 'dark');

export function applyTheme(t: Theme): void {
  document.documentElement.dataset.theme = t;
  try { localStorage.setItem(KEY, t); } catch { /* storage can be blocked; the theme still applies for this visit */ }
}

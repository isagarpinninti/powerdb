import { useCallback, useEffect, useMemo, useState } from 'react';
import { revealItemInDir } from '@tauri-apps/plugin-opener';
import { Logo } from '@dash/Logo';
import { applyTheme, readTheme, type Theme } from '@dash/theme';
import { cn, useToast } from '@dash/ui';
import { api, errorText, onGoto } from './api';
import { useApp } from './app';
import { Dot, phaseLabel } from './common';
import { CommandPalette, type Command } from './components/CommandPalette';
import { StatusBar } from './components/StatusBar';
import { Icon, type IconName } from './icons';
import { NavCtx, type View } from './nav';
import { About } from './views/About';
import { Backups } from './views/Backups';
import { Data } from './views/Data';
import { Logs } from './views/Logs';
import { Overview } from './views/Overview';
import { Settings } from './views/Settings';
import { Setup } from './views/Setup';

interface Section { id: View; label: string; icon: IconName; Page: () => React.JSX.Element }
const sections: Section[] = [
  { id: 'server', label: 'Overview', icon: 'overview', Page: Overview },
  { id: 'data', label: 'Data', icon: 'database', Page: Data },
  { id: 'backups', label: 'Backups', icon: 'backups', Page: Backups },
  { id: 'logs', label: 'Logs', icon: 'logs', Page: Logs },
  { id: 'settings', label: 'Settings', icon: 'settings', Page: Settings },
  { id: 'about', label: 'About', icon: 'info', Page: About },
];
const groups: { title?: string; ids: View[] }[] = [
  { ids: ['server', 'data'] },
  { title: 'Operations', ids: ['backups', 'logs'] },
  { title: 'System', ids: ['settings', 'about'] },
];
const mod = typeof navigator !== 'undefined' && /Mac|iPhone|iPad/.test(navigator.platform) ? '⌘' : 'Ctrl+';

export function Shell() {
  const { snap, status } = useApp();
  const toast = useToast();
  const [view, setView] = useState<View>(() => {
    // `?view=logs` opens a section directly (used for design previews); the real app takes `--view=logs` at launch
    const v = new URLSearchParams(window.location.search).get('view');
    return sections.some((s) => s.id === v) ? (v as View) : 'server';
  });
  const [palette, setPalette] = useState(false);
  const [theme, setTheme] = useState<Theme>(readTheme());
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    const un = onGoto((v) => setView(v as View));
    return () => { void Promise.resolve(un).then((f) => typeof f === 'function' && f()); };
  }, []);

  const running = status.phase === 'running';
  const transitional = status.phase === 'starting' || status.phase === 'stopping';
  const server = useCallback(async (what: 'start' | 'stop' | 'restart') => {
    setBusy(true);
    try { await api[what](); } catch (e) { toast(errorText(e), 'err'); } finally { setBusy(false); }
  }, [toast]);
  const flip = useCallback(() => { const t: Theme = readTheme() === 'dark' ? 'light' : 'dark'; applyTheme(t); setTheme(t); }, []);

  const commands = useMemo<Command[]>(() => [
    ...sections.map((s, i) => ({ id: `go-${s.id}`, label: s.label, icon: s.icon, group: 'Go to' as const, shortcut: `${mod}${i + 1}`, run: () => setView(s.id) })),
    ...(running || status.phase === 'starting'
      ? [
          { id: 'restart', label: 'Restart server', icon: 'restart' as const, group: 'Server' as const, run: () => void server('restart') },
          { id: 'stop', label: 'Stop server', icon: 'stop' as const, group: 'Server' as const, run: () => void server('stop') },
        ]
      : [{ id: 'start', label: 'Start server', icon: 'play' as const, group: 'Server' as const, run: () => void server('start') }]),
    { id: 'backup', label: 'Back up now', hint: running ? undefined : 'server must be running', icon: 'backups', group: 'Actions', keywords: 'snapshot save copy',
      run: () => { api.backup(false).then((b) => toast(`Backup ${b.name} saved`), (e) => toast(errorText(e), 'err')); } },
    { id: 'reveal', label: 'Show data folder', icon: 'folder', group: 'Actions', keywords: 'finder explorer open', run: () => { revealItemInDir(snap.config.data_dir).catch(() => {}); } },
    { id: 'theme', label: theme === 'dark' ? 'Switch to light theme' : 'Switch to dark theme', icon: theme === 'dark' ? 'sun' : 'moon', group: 'Actions', keywords: 'appearance dark light', run: flip },
  ], [running, status.phase, server, toast, snap.config.data_dir, theme, flip]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!(e.metaKey || e.ctrlKey) || e.altKey) return;
      if (e.key.toLowerCase() === 'k') { e.preventDefault(); setPalette((p) => !p); return; }
      const n = Number(e.key);
      if (Number.isInteger(n) && n >= 1 && n <= sections.length) { e.preventDefault(); setView(sections[n - 1].id); }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, []);

  if (!snap.config.configured) return <Setup />;
  const current = sections.find((s) => s.id === view)!;
  const Current = current.Page;

  return (
    <NavCtx.Provider value={setView}>
      <div className="flex h-full flex-col no-select">
        <div className="flex min-h-0 flex-1">
          <aside className="flex w-[236px] shrink-0 flex-col border-r border-line bg-surface" aria-label="Primary">
            <div className="flex items-center gap-2.5 px-5 pb-5 pt-5">
              <Logo size={30} />
              <div className="leading-tight"><div className="text-[15px] font-bold tracking-tight">PowerDB</div><div className="text-[11px] text-faint">Local server</div></div>
            </div>
            <nav className="flex-1 overflow-auto px-3" aria-label="Sections">
              {groups.map((g, gi) => (
                <div key={gi} className={gi ? 'mt-5' : ''}>
                  {g.title && <div className="px-3 pb-1.5 text-[11px] font-semibold uppercase tracking-wider text-faint">{g.title}</div>}
                  <ul className="space-y-0.5">
                    {g.ids.map((id) => {
                      const s = sections.find((x) => x.id === id)!;
                      const on = view === id;
                      return (
                        <li key={id}>
                          <button onClick={() => setView(id)} aria-current={on ? 'page' : undefined}
                            className={cn('group flex w-full items-center gap-3 rounded-md px-3 py-2 text-left text-[13px] transition-colors', on ? 'bg-raised font-medium text-ink' : 'text-muted hover:bg-raised/60 hover:text-ink')}>
                            <Icon name={s.icon} size={17} className={on ? 'text-ink' : 'text-faint group-hover:text-ink'} />
                            <span className="flex-1">{s.label}</span>
                            <kbd className="hidden text-[10px] text-faint group-hover:inline">{mod}{sections.indexOf(s) + 1}</kbd>
                          </button>
                        </li>
                      );
                    })}
                  </ul>
                </div>
              ))}
            </nav>
            <div className="m-3 rounded-lg border border-line bg-bg p-3" role="group" aria-label="Server">
              <div className="flex items-center gap-2 text-[13px] font-medium"><Dot phase={status.phase} />{phaseLabel[status.phase]}</div>
              <div className="mt-0.5 font-mono text-[11px] text-faint">127.0.0.1:{snap.config.port}</div>
              <div className="mt-2.5 flex gap-1.5">
                {running || status.phase === 'stopping' ? (
                  <>
                    <button disabled={busy || transitional} onClick={() => void server('restart')} className="flex flex-1 items-center justify-center gap-1.5 rounded-md border border-line-strong bg-raised py-1.5 text-xs font-medium hover:bg-line disabled:opacity-50"><Icon name="restart" size={13} />Restart</button>
                    <button disabled={busy || transitional} onClick={() => void server('stop')} aria-label="Stop server" className="flex items-center justify-center rounded-md border border-danger/40 px-2.5 text-danger hover:bg-danger/10 disabled:opacity-50"><Icon name="stop" size={13} /></button>
                  </>
                ) : (
                  <button disabled={busy || transitional} onClick={() => void server('start')} className="flex flex-1 items-center justify-center gap-1.5 rounded-md bg-ink py-1.5 text-xs font-medium text-bg hover:opacity-90 disabled:opacity-50"><Icon name="play" size={13} />Start server</button>
                )}
              </div>
            </div>
          </aside>

          <div className="flex min-w-0 flex-1 flex-col">
            <header className="flex h-12 shrink-0 items-center justify-between border-b border-line px-5">
              <nav aria-label="Breadcrumb" className="flex items-center gap-1.5 text-[13px] text-muted">
                <span>Local server</span><Icon name="chevron" size={13} className="text-faint" /><span className="font-medium text-ink" aria-current="page">{current.label}</span>
              </nav>
              <div className="flex items-center gap-2">
                <button onClick={() => setPalette(true)} className="flex h-8 items-center gap-2 rounded-md border border-line-strong bg-bg px-2.5 text-xs text-muted hover:text-ink" aria-label="Open command palette" aria-keyshortcuts={`${mod}K`}>
                  <Icon name="search" size={14} /><span>Search or run a command</span><kbd className="rounded border border-line-strong px-1 text-[10px]">{mod}K</kbd>
                </button>
                <button onClick={flip} aria-label={theme === 'dark' ? 'Switch to light theme' : 'Switch to dark theme'} className="flex h-8 w-8 items-center justify-center rounded-md border border-line-strong bg-bg text-muted hover:text-ink"><Icon name={theme === 'dark' ? 'sun' : 'moon'} size={15} /></button>
              </div>
            </header>
            <main className={view === 'data' ? 'min-h-0 flex-1 overflow-hidden' : 'min-h-0 flex-1 overflow-auto select-text'} id="main"><Current /></main>
          </div>
        </div>
        <StatusBar />
        {palette && <CommandPalette commands={commands} onClose={() => setPalette(false)} />}
      </div>
    </NavCtx.Provider>
  );
}

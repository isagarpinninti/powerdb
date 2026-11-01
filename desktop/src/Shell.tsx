import { useState } from 'react';
import { Logo } from '@dash/Logo';
import { cn } from '@dash/ui';
import { useApp } from './app';
import { Dot, phaseLabel } from './common';
import { About } from './views/About';
import { Backups } from './views/Backups';
import { Logs } from './views/Logs';
import { Overview } from './views/Overview';
import { Settings } from './views/Settings';
import { Setup } from './views/Setup';

const nav = [
  ['server', 'Server', Overview], ['backups', 'Backups', Backups], ['logs', 'Logs', Logs], ['settings', 'Settings', Settings], ['about', 'About', About],
] as const;

export function Shell() {
  const { snap, status } = useApp();
  const [view, setView] = useState<(typeof nav)[number][0]>('server');
  if (!snap.config.configured) return <Setup />;
  const Current = nav.find((n) => n[0] === view)![2];
  return (
    <div className="grid h-full grid-cols-[220px_1fr] no-select">
      <aside className="flex flex-col border-r border-line bg-surface">
        <div className="flex items-center gap-2.5 px-5 pb-4 pt-6"><Logo size={28} /><span className="text-base font-bold tracking-tight">PowerDB</span></div>
        <nav className="flex-1 space-y-0.5 px-3" aria-label="Sections">
          {nav.map(([id, label]) => (
            <button key={id} onClick={() => setView(id)} aria-current={view === id ? 'page' : undefined}
              className={cn('flex w-full items-center rounded-md px-3 py-2 text-left text-sm transition-colors', view === id ? 'bg-raised font-medium text-ink' : 'text-muted hover:bg-raised hover:text-ink')}>{label}</button>))}
        </nav>
        <div className="m-3 flex items-center gap-2.5 rounded-md border border-line bg-bg px-3 py-2.5 text-sm" role="status">
          <Dot phase={status.phase} /><div className="leading-tight"><div className="font-medium">{phaseLabel[status.phase]}</div><div className="text-xs text-muted">port {snap.config.port}</div></div>
        </div>
      </aside>
      <main className="overflow-auto select-text"><Current /></main>
    </div>
  );
}

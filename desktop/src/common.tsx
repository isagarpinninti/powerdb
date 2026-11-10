import type { ReactNode } from 'react';
import { cn } from '@dash/ui';
import type { Phase } from './api';

export const phaseLabel: Record<Phase, string> = { stopped: 'Stopped', starting: 'Starting…', running: 'Running', stopping: 'Stopping…', failed: 'Failed' };

export function Dot({ phase, size = 10 }: { phase: Phase; size?: number }) {
  const color = { running: 'bg-ok', starting: 'bg-live pulse', stopping: 'bg-live pulse', stopped: 'bg-faint', failed: 'bg-danger' }[phase];
  return <span className={cn('inline-block shrink-0 rounded-full', color)} style={{ width: size, height: size }} aria-hidden="true" />;
}

export function Page({ title, subtitle, actions, children }: { title: string; subtitle?: ReactNode; actions?: ReactNode; children: ReactNode }) {
  return (
    <div className="w-full space-y-6 px-8 py-7">
      <div className="flex items-start justify-between gap-4">
        <div><h1 className="text-[22px] font-bold tracking-tight">{title}</h1>{subtitle && <p className="mt-0.5 text-sm text-muted">{subtitle}</p>}</div>
        {actions && <div className="flex shrink-0 items-center gap-2">{actions}</div>}
      </div>
      {children}
    </div>
  );
}

export function Section({ title, description, children }: { title: string; description?: ReactNode; children: ReactNode }) {
  return (
    <section className="space-y-3">
      <div><h2 className="text-sm font-semibold uppercase tracking-wide text-muted">{title}</h2>{description && <p className="mt-0.5 text-sm text-muted">{description}</p>}</div>
      {children}
    </section>
  );
}

export function Row({ label, hint, children }: { label: string; hint?: ReactNode; children: ReactNode }) {
  return (
    <div className="flex items-center justify-between gap-6 px-5 py-3.5">
      <div className="min-w-0"><div className="text-sm font-medium">{label}</div>{hint && <div className="mt-0.5 text-xs text-muted">{hint}</div>}</div>
      <div className="shrink-0">{children}</div>
    </div>
  );
}

export function Toggle({ checked, onChange, label }: { checked: boolean; onChange: (v: boolean) => void; label: string }) {
  return (
    <button type="button" role="switch" aria-checked={checked} aria-label={label} onClick={() => onChange(!checked)}
      className={cn('relative h-6 w-11 rounded-full border border-line-strong transition-colors', checked ? 'bg-ink' : 'bg-raised')}>
      <span className={cn('absolute top-0.5 h-5 w-5 rounded-full transition-all', checked ? 'left-[22px] bg-bg' : 'left-0.5 bg-muted')} />
    </button>
  );
}

export function ErrorLine({ error }: { error: string | null }) {
  if (!error) return null;
  return <div role="alert" className="rounded-md border border-danger/40 bg-danger/10 px-3 py-2 text-sm text-danger">{error}</div>;
}

export const fileManager = (platform: string) => (platform === 'macos' ? 'Finder' : platform === 'windows' ? 'File Explorer' : 'file manager');

export function uptime(since: number | null): string {
  if (!since) return '';
  const s = Math.max(0, Math.floor((Date.now() - since) / 1000));
  const d = Math.floor(s / 86400), h = Math.floor((s % 86400) / 3600), m = Math.floor((s % 3600) / 60);
  return d ? `${d}d ${h}h` : h ? `${h}h ${m}m` : m ? `${m}m` : `${s}s`;
}

/** "4 minutes ago", "3 days ago": how long since a moment, in words. */
export function timeAgo(ms: number, now = Date.now()): string {
  const s = Math.max(0, Math.floor((now - ms) / 1000));
  if (s < 45) return 'just now';
  const m = Math.floor(s / 60);
  if (m < 60) return `${m} minute${m === 1 ? '' : 's'} ago`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h} hour${h === 1 ? '' : 's'} ago`;
  const d = Math.floor(h / 24);
  return `${d} day${d === 1 ? '' : 's'} ago`;
}

/** The level word in a log line the server wrote ("INFO", "WARN", "ERROR"), for colouring and filtering. */
export function logLevel(line: string): 'error' | 'warn' | 'info' | 'debug' {
  if (/\bERROR\b/.test(line)) return 'error';
  if (/\bWARN\b/.test(line)) return 'warn';
  if (/\b(DEBUG|TRACE)\b/.test(line)) return 'debug';
  return 'info';
}

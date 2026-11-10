import { useCallback, useEffect, useState } from 'react';
import { revealItemInDir } from '@tauri-apps/plugin-opener';
import { Button, Card, CopyButton, cn, fmtBytes, useToast } from '@dash/ui';
import { api, errorText, type BackupInfo, type Stats } from '../api';
import { useApp } from '../app';
import { Dot, ErrorLine, Page, Section, fileManager, logLevel, phaseLabel, timeAgo, uptime } from '../common';
import { Icon, type IconName } from '../icons';
import { useNav } from '../nav';

type Tone = 'ok' | 'warn' | 'bad' | 'idle';
const tone: Record<Tone, string> = { ok: 'text-ok', warn: 'text-warn', bad: 'text-danger', idle: 'text-faint' };

function Kpi({ icon, label, value, note, t = 'idle' }: { icon: IconName; label: string; value: string; note?: string; t?: Tone }) {
  return (
    <Card className="p-4">
      <div className="flex items-center justify-between text-muted"><span className="text-xs font-semibold uppercase tracking-wide">{label}</span><Icon name={icon} size={16} className={tone[t]} /></div>
      <div className="mt-2 text-2xl font-bold tracking-tight">{value}</div>
      {note && <div className="mt-0.5 truncate text-xs text-muted">{note}</div>}
    </Card>
  );
}

function Check({ t, title, detail, action }: { t: Tone; title: string; detail: string; action?: { label: string; run: () => void } }) {
  return (
    <li className="flex items-center gap-3 px-5 py-3">
      <span className={cn('flex h-6 w-6 shrink-0 items-center justify-center rounded-full border', t === 'ok' ? 'border-ok/40 bg-ok/10' : t === 'warn' ? 'border-warn/40 bg-warn/10' : t === 'bad' ? 'border-danger/40 bg-danger/10' : 'border-line-strong bg-raised', tone[t])}>
        <Icon name={t === 'ok' ? 'check' : t === 'idle' ? 'clock' : 'alert'} size={13} />
      </span>
      <div className="min-w-0 flex-1"><div className="text-sm font-medium">{title}</div><div className="truncate text-xs text-muted">{detail}</div></div>
      {action && <Button small onClick={action.run}>{action.label}</Button>}
    </li>
  );
}

export function Overview() {
  const { snap, status, logs } = useApp();
  const toast = useToast();
  const go = useNav();
  const [busy, setBusy] = useState(false);
  const [stats, setStats] = useState<Stats | null>(null);
  const [backups, setBackups] = useState<BackupInfo[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [, tick] = useState(0);
  const running = status.phase === 'running';
  const url = `http://127.0.0.1:${snap.config.port}`;

  const load = useCallback(() => {
    void api.stats().then(setStats, () => {});
    void api.backups().then(setBackups, () => {});
  }, []);
  useEffect(() => {
    if (!running) { setStats(null); setBackups(null); return; }
    load();
    const t = setInterval(() => { load(); tick((n) => n + 1); }, 10_000);
    return () => clearInterval(t);
  }, [running, load]);

  const act = async (f: () => Promise<void>) => { setBusy(true); setError(null); try { await f(); } catch (e) { setError(errorText(e)); } finally { setBusy(false); } };
  const transitional = status.phase === 'starting' || status.phase === 'stopping';
  const newest = backups && backups.length ? Math.max(...backups.map((b) => b.created_at)) : null;
  const reclaim = stats && stats.log_bytes ? stats.dead_bytes / stats.log_bytes : 0;
  const recent = logs.slice(-6).reverse();

  return (
    <Page title="Overview" subtitle="The PowerDB server running on this computer."
      actions={<Button variant="primary" disabled={!running} onClick={() => go('data')}><Icon name="database" size={15} />Browse data</Button>}>
      <Card className="flex flex-wrap items-center justify-between gap-4 p-5">
        <div className="flex items-center gap-4">
          <span className={cn('flex h-11 w-11 items-center justify-center rounded-xl border', running ? 'border-ok/40 bg-ok/10 text-ok' : status.phase === 'failed' ? 'border-danger/40 bg-danger/10 text-danger' : 'border-line-strong bg-raised text-muted')}><Icon name="database" size={22} /></span>
          <div>
            <div className="flex items-center gap-2 text-lg font-semibold"><Dot phase={status.phase} />{phaseLabel[status.phase]}</div>
            <div className="text-sm text-muted">{running ? `Up ${uptime(status.since)} · process ${status.pid}` : status.phase === 'stopped' ? 'Not running. Your data is safe on disk.' : status.phase === 'failed' ? 'Stopped unexpectedly.' : 'Please wait…'}</div>
          </div>
        </div>
        <div className="flex items-center gap-2">
          {running || status.phase === 'stopping' ? (
            <><Button disabled={busy || transitional} onClick={() => act(api.restart)}><Icon name="restart" size={14} />Restart</Button><Button variant="danger" disabled={busy || transitional} onClick={() => act(api.stop)}><Icon name="stop" size={14} />Stop</Button></>
          ) : (
            <Button variant="primary" disabled={busy || transitional} onClick={() => act(api.start)}><Icon name="play" size={14} />Start server</Button>
          )}
        </div>
        {(status.message || error) && <div className="w-full space-y-2"><ErrorLine error={status.message} /><ErrorLine error={error} /></div>}
      </Card>

      <div className="grid grid-cols-2 gap-3 lg:grid-cols-4">
        <Kpi icon="disk" label="Stored" value={stats ? fmtBytes(stats.log_bytes) : '—'} note={stats ? `${stats.segments} segment${stats.segments === 1 ? '' : 's'}` : 'server not running'} t={stats ? 'ok' : 'idle'} />
        <Kpi icon="backups" label="Last backup" value={newest ? timeAgo(newest) : running ? 'Never' : '—'} note={backups ? `${backups.length} backup${backups.length === 1 ? '' : 's'} kept` : undefined} t={!running ? 'idle' : newest && Date.now() - newest < 7 * 864e5 ? 'ok' : 'warn'} />
        <Kpi icon="clock" label="Reclaimable" value={stats ? fmtBytes(stats.dead_bytes) : '—'} note={stats ? `${stats.gc_pending} file${stats.gc_pending === 1 ? '' : 's'} waiting` : undefined} t={reclaim > 0.3 ? 'warn' : stats ? 'ok' : 'idle'} />
        <Kpi icon="shield" label="Version" value={snap.server_version.replace('powerdb ', '')} note="server" t="idle" />
      </div>

      <Section title="Health">
        <Card><ul className="divide-y divide-line">
          <Check t={running ? 'ok' : status.phase === 'failed' ? 'bad' : 'idle'} title="Server is reachable" detail={running ? `Answering on ${url}` : 'Start the server to use your data.'} action={!running && !transitional ? { label: 'Start', run: () => void act(api.start) } : undefined} />
          <Check t={!running ? 'idle' : newest && Date.now() - newest < 7 * 864e5 ? 'ok' : 'warn'} title="Recent backup" detail={!running ? 'Checked while the server is running.' : newest ? `Last one ${timeAgo(newest)}.` : 'No backup yet. A copy on this same disk will not survive losing the disk, but it protects against mistakes.'} action={running && !(newest && Date.now() - newest < 7 * 864e5) ? { label: 'Back up now', run: () => go('backups') } : undefined} />
          <Check t={!stats ? 'idle' : reclaim > 0.3 ? 'warn' : 'ok'} title="Space held by deleted files" detail={!stats ? 'Checked while the server is running.' : reclaim > 0.3 ? `${fmtBytes(stats.dead_bytes)} of deleted files can be reclaimed.` : 'Nothing significant to reclaim.'} />
        </ul></Card>
      </Section>

      <div className="grid gap-5 lg:grid-cols-2">
        <Section title="Recent activity">
          <Card className="overflow-hidden">
            {recent.length === 0 ? <div className="p-6 text-center text-sm text-muted">Nothing logged yet.</div> : (
              <ul className="divide-y divide-line font-mono text-[11px] leading-5">
                {recent.map((l, i) => { const lv = logLevel(l); return (
                  <li key={i} className="flex gap-2 px-4 py-1.5"><span className={cn('w-10 shrink-0 font-semibold uppercase', lv === 'error' ? 'text-danger' : lv === 'warn' ? 'text-warn' : 'text-faint')}>{lv}</span><span className="truncate text-muted" title={l}>{l.replace(/^\S+\s+(INFO|WARN|ERROR|DEBUG)\s+/, '')}</span></li>); })}
              </ul>)}
            <button onClick={() => go('logs')} className="flex w-full items-center justify-center gap-1 border-t border-line py-2 text-xs text-muted hover:text-ink">View all logs<Icon name="chevron" size={12} /></button>
          </Card>
        </Section>
        <Section title="Connect">
          <Card className="divide-y divide-line">
            <div className="space-y-1.5 px-4 py-3"><div className="flex items-center justify-between text-xs"><span className="font-semibold uppercase tracking-wide text-muted">TypeScript</span><CopyButton text={`import { PowerDB } from 'powerdb-sdk';\nconst db = new PowerDB('${url}', 'pdb_live_…');`} /></div>
              <pre className="overflow-x-auto rounded-md bg-raised p-2.5 text-[11px]">{`import { PowerDB } from 'powerdb-sdk';\nconst db = new PowerDB('${url}', 'pdb_live_…');`}</pre></div>
            <div className="space-y-1.5 px-4 py-3"><div className="flex items-center justify-between text-xs"><span className="font-semibold uppercase tracking-wide text-muted">curl</span><CopyButton text={`curl -H "authorization: Bearer $POWERDB_KEY" ${url}/api/v1/collections`} /></div>
              <pre className="overflow-x-auto rounded-md bg-raised p-2.5 text-[11px]">{`curl -H "authorization: Bearer $POWERDB_KEY" \\\n  ${url}/api/v1/collections`}</pre></div>
            <div className="flex items-center justify-between gap-3 px-4 py-3"><div className="min-w-0"><div className="text-xs font-semibold uppercase tracking-wide text-muted">Data folder</div><div className="truncate font-mono text-[11px] text-muted" title={snap.config.data_dir}>{snap.config.data_dir}</div></div>
              <Button small onClick={() => revealItemInDir(snap.config.data_dir).catch((e) => toast(errorText(e), 'err'))}>Show in {fileManager(snap.platform)}</Button></div>
          </Card>
        </Section>
      </div>
    </Page>
  );
}

import { useEffect, useState } from 'react';
import { revealItemInDir } from '@tauri-apps/plugin-opener';
import { Badge, Button, Card, CopyButton, fmtBytes, useToast } from '@dash/ui';
import { api, errorText, type Stats } from '../api';
import { useApp } from '../app';
import { Dot, ErrorLine, Page, Section, fileManager, phaseLabel, uptime } from '../common';

export function Overview() {
  const { snap, status } = useApp();
  const toast = useToast();
  const [busy, setBusy] = useState(false);
  const [stats, setStats] = useState<Stats | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [, tick] = useState(0);
  const url = `http://127.0.0.1:${snap.config.port}`;
  const running = status.phase === 'running';

  useEffect(() => {
    if (!running) { setStats(null); return; }
    let live = true;
    const load = () => api.stats().then((s) => live && setStats(s), () => {});
    void load();
    const t = setInterval(() => { void load(); tick((n) => n + 1); }, 5000);
    return () => { live = false; clearInterval(t); };
  }, [running]);

  const act = async (f: () => Promise<void>) => {
    setBusy(true); setError(null);
    try { await f(); } catch (e) { setError(errorText(e)); } finally { setBusy(false); }
  };
  const transitional = status.phase === 'starting' || status.phase === 'stopping';

  const reveal = async () => { try { await revealItemInDir(snap.config.data_dir); } catch (e) { toast(errorText(e), 'err'); } };

  return (
    <Page title="Server" subtitle="The PowerDB server running on this computer."
      actions={<Button variant="primary" disabled={!running} onClick={() => act(api.openDashboard)}>Open Dashboard</Button>}>
      <Card className="p-6">
        <div className="flex items-start justify-between gap-6">
          <div className="flex items-center gap-4">
            <Dot phase={status.phase} size={14} />
            <div>
              <div className="text-xl font-semibold">{phaseLabel[status.phase]}</div>
              <div className="text-sm text-muted">
                {running ? `Up for ${uptime(status.since)} · process ${status.pid}` : status.phase === 'stopped' ? 'The server is not running. Your data is safe on disk.' : status.phase === 'failed' ? 'The server stopped unexpectedly.' : ' '}
              </div>
            </div>
          </div>
          <div className="flex gap-2">
            {running || status.phase === 'stopping' ? (
              <>
                <Button disabled={busy || transitional} onClick={() => act(api.restart)}>Restart</Button>
                <Button variant="danger" disabled={busy || transitional} onClick={() => act(api.stop)}>Stop</Button>
              </>
            ) : (
              <Button variant="primary" disabled={busy || transitional} onClick={() => act(api.start)}>Start Server</Button>
            )}
          </div>
        </div>
        {status.message && <div className="mt-4"><ErrorLine error={status.message} /></div>}
        <div className="mt-4"><ErrorLine error={error} /></div>
        <div className="mt-5 flex flex-wrap items-center gap-x-6 gap-y-2 border-t border-line pt-4 text-sm">
          <span className="text-muted">Address</span><code className="rounded bg-raised px-2 py-0.5">{url}</code><CopyButton text={url} />
        </div>
      </Card>

      {running && stats && (
        <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
          {[['Stored', fmtBytes(stats.log_bytes), `${stats.segments} segment${stats.segments === 1 ? '' : 's'}`],
            ['Reclaimable', fmtBytes(stats.dead_bytes), 'from deleted files'],
            ['Waiting', String(stats.gc_pending), 'files to release'],
            ['Version', snap.server_version.replace('powerdb ', ''), 'server']].map(([k, v, n]) => (
            <Card key={k} className="p-4"><div className="text-xs font-semibold uppercase tracking-wide text-muted">{k}</div><div className="mt-1 text-2xl font-bold">{v}</div><div className="mt-0.5 text-xs text-muted">{n}</div></Card>
          ))}
        </div>
      )}

      <Section title="Connect">
        <Card className="divide-y divide-line">
          <div className="space-y-2 px-5 py-4">
            <div className="flex items-center justify-between"><span className="text-sm font-medium">TypeScript</span><CopyButton text={`import { PowerDB } from 'powerdb-sdk';\nconst db = new PowerDB('${url}');\ndb.setToken('pdb_live_…');`} /></div>
            <pre className="overflow-x-auto rounded-md bg-raised p-3 text-xs">{`import { PowerDB } from 'powerdb-sdk';\nconst db = new PowerDB('${url}');\ndb.setToken('pdb_live_…');   // an API key from the dashboard`}</pre>
          </div>
          <div className="space-y-2 px-5 py-4">
            <div className="flex items-center justify-between"><span className="text-sm font-medium">curl</span><CopyButton text={`curl -H "authorization: Bearer $POWERDB_KEY" ${url}/api/v1/collections`} /></div>
            <pre className="overflow-x-auto rounded-md bg-raised p-3 text-xs">{`curl -H "authorization: Bearer $POWERDB_KEY" ${url}/api/v1/collections`}</pre>
          </div>
        </Card>
      </Section>

      <Section title="Data">
        <Card className="flex items-center justify-between gap-4 px-5 py-4">
          <div className="min-w-0"><div className="text-sm font-medium">Data folder</div><div className="truncate font-mono text-xs text-muted" title={snap.config.data_dir}>{snap.config.data_dir}</div></div>
          <div className="flex shrink-0 items-center gap-2"><Badge tone="slate">format 1</Badge><Button small onClick={reveal}>Show in {fileManager(snap.platform)}</Button></div>
        </Card>
      </Section>
    </Page>
  );
}

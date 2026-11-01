import { useCallback, useEffect, useState } from 'react';
import { open } from '@tauri-apps/plugin-dialog';
import { Badge, Button, Card, Modal, fmtBytes, fmtDate, useToast } from '@dash/ui';
import { api, errorText, type BackupInfo } from '../api';
import { useApp } from '../app';
import { ErrorLine, Page, Section } from '../common';

export function Backups() {
  const { snap, status, refresh } = useApp();
  const toast = useToast();
  const running = status.phase === 'running';
  const [list, setList] = useState<BackupInfo[] | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [deep, setDeep] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [restoring, setRestoring] = useState<BackupInfo | null>(null);
  const [checkOut, setCheckOut] = useState<string | null>(null);

  const load = useCallback(() => (running ? api.backups().then(setList, (e) => setError(errorText(e))) : Promise.resolve(setList([]))), [running]);
  useEffect(() => { void load(); }, [load]);

  const run = async (what: string, f: () => Promise<void>) => {
    setBusy(what); setError(null);
    try { await f(); } catch (e) { setError(errorText(e)); } finally { setBusy(null); }
  };

  return (
    <Page title="Backups" subtitle="Consistent copies of your database, taken while it is running."
      actions={<Button variant="primary" disabled={!running || !!busy} onClick={() => run('backup', async () => { const b = await api.backup(deep); toast(`Backup ${b.name} saved (${fmtBytes(b.bytes)})`); await load(); })}>{busy === 'backup' ? 'Backing up…' : 'Back up now'}</Button>}>
      {!running && <ErrorLine error="Start the server to take or list backups." />}
      <ErrorLine error={error} />
      <label className="flex items-center gap-2 text-sm text-muted"><input type="checkbox" checked={deep} onChange={(e) => setDeep(e.target.checked)} /> Verify every stored chunk too (slower, more thorough)</label>

      <Card className="overflow-hidden">
        <table className="w-full text-sm">
          <thead className="bg-raised text-left text-xs uppercase tracking-wide text-muted"><tr><th className="px-4 py-2">Name</th><th>Created</th><th>Size</th><th>Files</th><th className="px-4" /></tr></thead>
          <tbody>
            {list === null ? <tr><td colSpan={5} className="p-6 text-center text-muted">Loading…</td></tr>
              : list.length === 0 ? <tr><td colSpan={5} className="p-8 text-center text-muted">No backups yet. They are stored in <code>backups</code> inside the data folder.</td></tr>
              : list.map((b) => (
                <tr key={b.name} className="border-t border-line">
                  <td className="px-4 py-2.5 font-mono text-xs">{b.name}</td>
                  <td>{fmtDate(b.created_at)}</td>
                  <td>{fmtBytes(b.bytes)}</td>
                  <td>{b.files} {b.includes_secret && <Badge tone="gold">secret</Badge>}</td>
                  <td className="px-4 text-right"><Button small onClick={() => setRestoring(b)}>Restore…</Button></td>
                </tr>))}
          </tbody>
        </table>
      </Card>

      <Section title="Integrity check" description="Re-reads the stored data and recomputes every count. The server is stopped for the duration and started again.">
        <Card className="space-y-3 p-5">
          <div className="flex gap-2">
            <Button disabled={!!busy} onClick={() => run('quick', async () => setCheckOut(await api.check(true)))}>{busy === 'quick' ? 'Checking…' : 'Quick check'}</Button>
            <Button disabled={!!busy} onClick={() => run('full', async () => setCheckOut(await api.check(false)))}>{busy === 'full' ? 'Checking…' : 'Full check'}</Button>
          </div>
          {checkOut && <pre className="overflow-auto rounded-md bg-raised p-3 text-xs">{checkOut}</pre>}
        </Card>
      </Section>

      {restoring && <RestoreModal backup={restoring} dataDir={snap.config.data_dir} onClose={() => setRestoring(null)} onSwitched={async () => { setRestoring(null); await refresh(); toast('Now using the restored database'); }} />}
    </Page>
  );
}

function RestoreModal({ backup, dataDir, onClose, onSwitched }: { backup: BackupInfo; dataDir: string; onClose: () => void; onSwitched: () => void }) {
  const [parent, setParent] = useState<string | null>(null);
  const [target, setTarget] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [out, setOut] = useState<string | null>(null);
  const sep = dataDir.includes('\\') ? '\\' : '/';

  const choose = async () => {
    const p = await open({ directory: true, title: 'Choose where the restored database goes' });
    if (typeof p === 'string') { setParent(p); setTarget(`${p}${sep}restored-${backup.name}`); }
  };
  const go = async () => {
    if (!target) return;
    setBusy(true); setError(null);
    try { setOut(await api.restore(backup.name, target)); } catch (e) { setError(errorText(e)); } finally { setBusy(false); }
  };

  return (
    <Modal title={`Restore ${backup.name}`} onClose={onClose}>
      <div className="space-y-4 text-sm">
        {!out ? (
          <>
            <p className="text-muted">The backup is verified against its checksums, then restored into a <b>new folder</b>. Your current database is not changed.</p>
            <div className="flex items-center gap-2"><Button onClick={choose}>Choose a folder…</Button><span className="truncate font-mono text-xs text-muted">{target ?? 'No folder chosen'}</span></div>
            <ErrorLine error={error} />
            <div className="flex justify-end gap-2"><Button variant="ghost" onClick={onClose}>Cancel</Button><Button variant="primary" disabled={!parent || busy} onClick={go}>{busy ? 'Restoring…' : 'Restore'}</Button></div>
          </>
        ) : (
          <>
            <p className="rounded-md bg-ok/10 px-3 py-2 text-ok">{out.trim().split('\n')[0]}</p>
            <p className="text-muted">The restored database is in <code>{target}</code>. Switch to it now, or keep using the current one and open it later.</p>
            <ErrorLine error={error} />
            <div className="flex justify-end gap-2">
              <Button variant="ghost" onClick={onClose}>Keep current</Button>
              <Button variant="primary" disabled={busy} onClick={async () => { setBusy(true); try { await api.useDatabase(target!); onSwitched(); } catch (e) { setError(errorText(e)); } finally { setBusy(false); } }}>Use the restored database</Button>
            </div>
          </>
        )}
      </div>
    </Modal>
  );
}

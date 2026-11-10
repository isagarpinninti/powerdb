import { useCallback, useEffect, useState } from 'react';
import { db } from '../session';
import { Button, Card, ErrorBox, Spinner, fmtBytes, useToast } from '../ui';

type Stats = Awaited<ReturnType<typeof db.admin.stats>>;

export function System() {
  const [s, setS] = useState<Stats | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const toast = useToast();
  const load = useCallback(() => db.admin.stats().then(setS, setError), []);
  useEffect(() => { void load(); }, [load]);

  const gc = async () => {
    setBusy(true);
    try {
      const r = await db.admin.gc();
      toast(`Released ${r.files_released} file${r.files_released === 1 ? '' : 's'}, reclaimed ${fmtBytes(r.bytes_reclaimed)}`);
      void load();
    } catch (e) { toast(e instanceof Error ? e.message : 'Failed', 'err'); } finally { setBusy(false); }
  };

  const [backups, setBackups] = useState<Awaited<ReturnType<typeof db.admin.backups>>['backups']>([]);
  const loadBackups = useCallback(() => db.admin.backups().then((r) => setBackups(r.backups), setError), []);
  useEffect(() => { void loadBackups(); }, [loadBackups]);
  const backup = async () => {
    setBusy(true);
    try {
      const r = await db.admin.backup();
      toast(`Backup ${r.name}: ${r.files} files, ${fmtBytes(r.bytes)}`);
      void loadBackups();
    } catch (e) { toast(e instanceof Error ? e.message : 'Failed', 'err'); } finally { setBusy(false); }
  };

  const tile = (label: string, value: string, note?: string) => (
    <Card className="p-4"><div className="text-xs font-semibold uppercase tracking-wide text-muted">{label}</div><div className="mt-1 text-2xl font-bold">{value}</div>{note && <div className="mt-0.5 text-xs text-muted">{note}</div>}</Card>
  );

  return (
    <div className="space-y-5 p-8">
      <div><h1 className="text-2xl font-bold">System</h1><p className="text-sm text-muted">The file store behind your collections.</p></div>
      <ErrorBox error={error} />
      {!s ? <Spinner /> : (
        <>
          <div className="grid gap-3 sm:grid-cols-4">
            {tile('Log size', fmtBytes(s.log_bytes), `${s.segments} segment${s.segments === 1 ? '' : 's'}`)}
            {tile('Reclaimable', fmtBytes(s.dead_bytes), 'bytes of deleted files')}
            {tile('Waiting', String(s.gc_pending), 'deleted files to release')}
            {tile('Server', window.location.host)}
          </div>
          <Card className="p-5">
            <h2 className="font-semibold">Reclaim space</h2>
            <p className="mt-1 text-sm text-muted">Deleting a record hides it at once; the disk space comes back in the background every few seconds. Run it now to release deleted files and compact segments that are mostly dead.</p>
            <Button className="mt-3" variant="primary" onClick={gc} disabled={busy}>{busy ? 'Working…' : 'Reclaim now'}</Button>
          </Card>
          <Card className="p-5">
            <h2 className="font-semibold">Backups</h2>
            <p className="mt-1 text-sm text-muted">A consistent copy of the live database, taken without pausing reads or writes. It is written on the server (in its backup directory); the secret key is left out. Restore one with <code>powerdb restore</code>.</p>
            <Button className="mt-3" variant="primary" onClick={backup} disabled={busy}>{busy ? 'Working…' : 'Back up now'}</Button>
            {backups.length > 0 && (
              <ul className="mt-4 divide-y divide-line text-sm">
                {backups.map((b) => (
                  <li key={b.name} className="flex justify-between py-2"><span className="font-mono">{b.name}</span><span className="text-muted">{new Date(b.created_at).toLocaleString()} · {fmtBytes(b.bytes)}</span></li>
                ))}
              </ul>
            )}
          </Card>
        </>
      )}
    </div>
  );
}

import { useCallback, useEffect, useState } from 'react';
import type { ApiKeyMeta, CollectionDef } from '@sdk';
import { db } from '../session';
import { Badge, Button, Card, CopyButton, ErrorBox, Field, Input, Modal, Select, Spinner, fmtDate, useToast } from '../ui';

export function Keys({ collections }: { collections: CollectionDef[] }) {
  const [keys, setKeys] = useState<ApiKeyMeta[] | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [adding, setAdding] = useState(false);
  const [secret, setSecret] = useState<string | null>(null);
  const toast = useToast();
  const load = useCallback(() => db.admin.keys.list().then(setKeys, setError), []);
  useEffect(() => { void load(); }, [load]);

  const revoke = async (k: ApiKeyMeta) => {
    if (!confirm(`Revoke "${k.name}"? Anything using it stops working at once.`)) return;
    try { await db.admin.keys.revoke(k.id); toast('Key revoked'); void load(); } catch (e) { toast(e instanceof Error ? e.message : 'Failed', 'err'); }
  };

  return (
    <div className="space-y-4 p-8">
      <div className="flex items-center justify-between">
        <div><h1 className="text-2xl font-bold">API keys</h1><p className="text-sm text-muted">For servers and scripts. A key can be limited to reading or writing and to chosen collections. Only a hash is stored.</p></div>
        <Button variant="primary" onClick={() => setAdding(true)}>Create key</Button>
      </div>
      <ErrorBox error={error} />
      {keys === null ? <Spinner /> : (
        <Card className="overflow-hidden">
          <table className="w-full text-sm">
            <thead className="bg-raised text-left text-xs uppercase tracking-wide text-muted"><tr><th className="px-4 py-2">Name</th><th>Access</th><th>Collections</th><th>Created</th><th className="px-4" /></tr></thead>
            <tbody>
              {keys.map((k) => (
                <tr key={k.id} className="border-t border-line">
                  <td className="px-4 py-2.5"><div>{k.name} {k.admin && <Badge tone="gold">admin</Badge>}</div><div className="font-mono text-xs text-muted">{k.id}</div></td>
                  <td><Badge>{k.access.replace('_', ' / ')}</Badge></td>
                  <td className="text-muted">{k.collections.length ? k.collections.join(', ') : 'all'}</td>
                  <td className="text-muted">{fmtDate(k.created_at)}</td>
                  <td className="px-4 text-right"><Button small variant="danger" onClick={() => revoke(k)}>Revoke</Button></td>
                </tr>
              ))}
            </tbody>
          </table>
        </Card>
      )}
      {adding && <AddKey collections={collections} onClose={() => setAdding(false)} onCreated={(s) => { setAdding(false); setSecret(s); void load(); }} />}
      {secret && (
        <Modal title="Your new API key" onClose={() => setSecret(null)}>
          <p className="mb-3 text-sm text-muted">Copy it now. It is shown only once; if you lose it, create another.</p>
          <code className="block break-all rounded-md border border-line-strong bg-raised p-3 text-sm text-ink">{secret}</code>
          <div className="mt-4 flex justify-end gap-2"><CopyButton text={secret} /><Button variant="primary" onClick={() => setSecret(null)}>Done</Button></div>
        </Modal>
      )}
    </div>
  );
}

function AddKey({ collections, onClose, onCreated }: { collections: CollectionDef[]; onClose: () => void; onCreated: (secret: string) => void }) {
  const [name, setName] = useState('');
  const [access, setAccess] = useState<'read' | 'write' | 'read_write'>('read');
  const [admin, setAdmin] = useState(false);
  const [chosen, setChosen] = useState<string[]>([]);
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setBusy(true);
    try { onCreated((await db.admin.keys.create({ name, access: admin ? 'read_write' : access, collections: admin ? [] : chosen, admin })).key); } catch (err) { setError(err); setBusy(false); }
  };
  return (
    <Modal title="Create API key" onClose={onClose}>
      <form onSubmit={submit} className="space-y-4">
        <Field label="Name" hint="So you can tell keys apart later."><Input value={name} onChange={(e) => setName(e.target.value)} required autoFocus placeholder="reporting service" /></Field>
        <label className="flex items-start gap-2 rounded-md border border-line p-3 text-sm"><input type="checkbox" className="mt-0.5 h-4 w-4 accent-ink" checked={admin} onChange={(e) => setAdmin(e.target.checked)} /><span><b>Administrator key</b><span className="block text-muted">Full access, including this dashboard, and it bypasses access rules.</span></span></label>
        {!admin && (
          <>
            <Field label="Access"><Select value={access} onChange={(e) => setAccess(e.target.value as typeof access)}><option value="read">Read only (list and view)</option><option value="write">Write only (create, update, delete)</option><option value="read_write">Read and write</option></Select></Field>
            <Field label="Collections" hint="None selected means all of them.">
              <div className="flex flex-wrap gap-2">
                {collections.map((c) => (
                  <label key={c.name} className="flex items-center gap-1.5 rounded-md border border-line px-2.5 py-1 text-sm"><input type="checkbox" className="accent-ink" checked={chosen.includes(c.name)} onChange={(e) => setChosen((s) => (e.target.checked ? [...s, c.name] : s.filter((x) => x !== c.name)))} />{c.name}</label>
                ))}
              </div>
            </Field>
          </>
        )}
        <ErrorBox error={error} />
        <div className="flex justify-end gap-2"><Button type="button" onClick={onClose}>Cancel</Button><Button type="submit" variant="primary" disabled={busy}>Create key</Button></div>
      </form>
    </Modal>
  );
}

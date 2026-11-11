import { useCallback, useEffect, useRef, useState } from 'react';
import type { BucketInfo, ObjectInfo } from '@sdk';
import { db } from '../session';
import { Badge, Button, Card, ErrorBox, Field, Input, Modal, Spinner, fmtBytes, fmtDate, useToast } from '../ui';

const message = (e: unknown) => (e instanceof Error ? e.message : 'Failed');

/** Saves bytes the browser already holds as a file. The request carries the administrator's credentials, so a plain link would not work. */
async function saveAs(bucket: string, key: string, version?: string) {
  const res = await db.bucket(bucket).response(key, { version });
  const url = URL.createObjectURL(await res.blob());
  const a = document.createElement('a');
  a.href = url;
  a.download = key.split('/').pop() || 'download';
  document.body.appendChild(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 10_000);
}

export function NewBucket({ onClose, onCreated }: { onClose: () => void; onCreated: (name: string) => void }) {
  const [name, setName] = useState('');
  const [versioning, setVersioning] = useState(false);
  const [encrypt, setEncrypt] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setBusy(true);
    try { await db.buckets.create(name, { versioning, encrypt }); onCreated(name); } catch (err) { setError(err); setBusy(false); }
  };
  return (
    <Modal title="New bucket" onClose={onClose}>
      <form onSubmit={submit} className="space-y-4">
        <ErrorBox error={error} />
        <Field label="Name" hint="Lowercase letters, digits, - and _. It cannot be renamed.">
          <Input value={name} onChange={(e) => setName(e.target.value)} placeholder="documents" className="font-mono" autoFocus spellCheck={false} required />
        </Field>
        <label className="flex items-start gap-2 text-sm">
          <input type="checkbox" checked={versioning} onChange={(e) => setVersioning(e.target.checked)} className="mt-1" />
          <span><b>Keep every version.</b> A new upload adds a version instead of replacing, and a delete only hides the file. It can be switched on later but not off.</span>
        </label>
        <label className="flex items-start gap-2 text-sm">
          <input type="checkbox" checked={encrypt} onChange={(e) => setEncrypt(e.target.checked)} className="mt-1" />
          <span><b>Encrypt at rest.</b> The file data is encrypted on disk. Set when the bucket is created.</span>
        </label>
        <div className="flex justify-end gap-2">
          <Button type="button" onClick={onClose}>Cancel</Button>
          <Button variant="primary" type="submit" disabled={busy || !name}>Create bucket</Button>
        </div>
      </form>
    </Modal>
  );
}

export function BucketView({ name, onChanged, onDropped }: { name: string; onChanged: () => void; onDropped: () => void }) {
  const [bucket, setBucket] = useState<BucketInfo | null>(null);
  const [items, setItems] = useState<ObjectInfo[] | null>(null);
  const [next, setNext] = useState<string | undefined>();
  const [prefix, setPrefix] = useState('');
  const [applied, setApplied] = useState('');
  const [error, setError] = useState<unknown>(null);
  const [versionsOf, setVersionsOf] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const fileInput = useRef<HTMLInputElement>(null);
  const toast = useToast();

  const load = useCallback(async (p: string) => {
    try {
      const [b, page] = await Promise.all([db.buckets.get(name), db.bucket(name).list({ prefix: p, limit: 100 })]);
      setBucket(b);
      setItems(page.items);
      setNext(page.next);
      setError(null);
    } catch (e) {
      setError(e);
    }
  }, [name]);
  useEffect(() => { void load(applied); }, [load, applied]);

  const more = async () => {
    if (!next) return;
    try {
      const page = await db.bucket(name).list({ prefix: applied, after: next, limit: 100 });
      setItems((cur) => [...(cur ?? []), ...page.items]);
      setNext(page.next);
    } catch (e) { toast(message(e), 'err'); }
  };

  const upload = async (files: FileList | null) => {
    if (!files || files.length === 0) return;
    setBusy(true);
    // files go under the folder typed in the filter, when that is one (ends with a slash)
    const folder = applied.endsWith('/') ? applied : '';
    let done = 0;
    for (const f of Array.from(files)) {
      try {
        await db.bucket(name).put(folder + f.name, f, { type: f.type || 'application/octet-stream', filename: f.name });
        done++;
      } catch (e) {
        toast(`${f.name}: ${message(e)}`, 'err');
      }
    }
    setBusy(false);
    if (fileInput.current) fileInput.current.value = '';
    if (done) toast(done === 1 ? 'Uploaded' : `Uploaded ${done} files`);
    void load(applied);
  };

  const remove = async (o: ObjectInfo) => {
    const note = bucket?.versioning ? 'It will be hidden; every version is kept and can be restored.' : 'This cannot be undone.';
    if (!confirm(`Delete ${o.key}? ${note}`)) return;
    try { await db.bucket(name).delete(o.key); toast('Deleted'); void load(applied); } catch (e) { toast(message(e), 'err'); }
  };

  const dropBucket = async () => {
    if (!confirm(`Delete the bucket ${name}? It must be empty.`)) return;
    try { await db.buckets.delete(name); toast('Bucket deleted'); onDropped(); } catch (e) { toast(message(e), 'err'); }
  };

  const download = async (o: ObjectInfo) => {
    try { await saveAs(name, o.key); } catch (e) { toast(message(e), 'err'); }
  };

  return (
    <div className="space-y-4 p-8">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <h1 className="flex items-center gap-2 text-2xl font-bold">{name}
            {bucket?.versioning && <Badge tone="gold">versioned</Badge>}
            {bucket?.encrypt && <Badge>encrypted</Badge>}
          </h1>
          <p className="text-sm text-muted">Files stored by name, with no record needed. Folders are only a habit in the names.</p>
        </div>
        <div className="flex gap-2">
          <input ref={fileInput} type="file" multiple className="sr-only" aria-label="Choose files to upload" onChange={(e) => void upload(e.target.files)} />
          <Button variant="primary" disabled={busy} onClick={() => fileInput.current?.click()}>{busy ? 'Uploading…' : 'Upload files'}</Button>
          {bucket && !bucket.versioning && <Button onClick={async () => { try { await db.buckets.enableVersioning(name); toast('Versioning is on'); void load(applied); onChanged(); } catch (e) { toast(message(e), 'err'); } }}>Keep versions</Button>}
          <Button variant="danger" onClick={dropBucket}>Delete bucket</Button>
        </div>
      </div>
      <ErrorBox error={error} />
      <form className="flex gap-2" onSubmit={(e) => { e.preventDefault(); setApplied(prefix); }}>
        <Input value={prefix} onChange={(e) => setPrefix(e.target.value)} placeholder="Start of the name, for example contracts/2026/" aria-label="Name starts with" className="w-full max-w-md font-mono" spellCheck={false} />
        <Button type="submit">Filter</Button>
        {applied && <Button type="button" variant="ghost" onClick={() => { setPrefix(''); setApplied(''); }}>Clear</Button>}
      </form>
      {items === null ? <Spinner /> : (
        <Card className="overflow-hidden">
          <table className="w-full text-sm">
            <thead className="bg-raised text-left text-xs uppercase tracking-wide text-muted">
              <tr><th className="px-4 py-2">Name</th><th>Size</th><th>Type</th><th>Modified</th><th className="px-4" /></tr>
            </thead>
            <tbody>
              {items.map((o) => (
                <tr key={o.key} className="border-t border-line">
                  <td className="px-4 py-2.5"><span className="break-all font-mono text-[13px]">{o.key}</span>
                    {Object.keys(o.metadata).length > 0 && <div className="text-xs text-muted">{Object.entries(o.metadata).map(([k, v]) => `${k}: ${v}`).join(' · ')}</div>}
                  </td>
                  <td className="whitespace-nowrap text-muted">{fmtBytes(o.size)}</td>
                  <td className="text-muted">{o.mime}</td>
                  <td className="whitespace-nowrap text-muted">{fmtDate(o.created_at)}</td>
                  <td className="whitespace-nowrap px-4 text-right">
                    <Button small variant="ghost" onClick={() => download(o)} aria-label={`Download ${o.key}`}>Download</Button>
                    {bucket?.versioning && <Button small variant="ghost" onClick={() => setVersionsOf(o.key)} aria-label={`Versions of ${o.key}`}>Versions</Button>}
                    <Button small variant="danger" onClick={() => remove(o)} aria-label={`Delete ${o.key}`}>Delete</Button>
                  </td>
                </tr>
              ))}
              {items.length === 0 && <tr><td colSpan={5} className="px-4 py-10 text-center text-muted">{applied ? 'Nothing starts with that.' : 'Nothing here yet. Upload a file to start.'}</td></tr>}
            </tbody>
          </table>
        </Card>
      )}
      {next && <div><Button onClick={more}>Load more</Button></div>}
      {versionsOf && <Versions bucket={name} objectKey={versionsOf} onClose={() => setVersionsOf(null)} onChanged={() => void load(applied)} />}
    </div>
  );
}

function Versions({ bucket, objectKey, onClose, onChanged }: { bucket: string; objectKey: string; onClose: () => void; onChanged: () => void }) {
  const [versions, setVersions] = useState<ObjectInfo[] | null>(null);
  const [error, setError] = useState<unknown>(null);
  const toast = useToast();
  const load = useCallback(() => db.bucket(bucket).versions(objectKey).then(setVersions, setError), [bucket, objectKey]);
  useEffect(() => { void load(); }, [load]);

  const act = async (fn: () => Promise<unknown>, done: string) => {
    try { await fn(); toast(done); await load(); onChanged(); } catch (e) { toast(message(e), 'err'); }
  };

  return (
    <Modal title={`Versions of ${objectKey}`} onClose={onClose} wide>
      <ErrorBox error={error} />
      {versions === null ? <Spinner /> : (
        <table className="w-full text-sm">
          <thead className="text-left text-xs uppercase tracking-wide text-muted"><tr><th className="py-2">Written</th><th>Size</th><th /></tr></thead>
          <tbody>
            {versions.map((v, i) => (
              <tr key={v.version_id} className="border-t border-line">
                <td className="py-2">{fmtDate(v.created_at)} {i === 0 && <Badge tone="green">newest</Badge>}</td>
                <td className="text-muted">{v.delete_marker ? 'deleted' : fmtBytes(v.size)}</td>
                <td className="whitespace-nowrap text-right">
                  {!v.delete_marker && <Button small variant="ghost" onClick={() => saveAs(bucket, objectKey, v.version_id).catch((e) => toast(message(e), 'err'))} aria-label={`Download the version from ${fmtDate(v.created_at)}`}>Download</Button>}
                  {!v.delete_marker && i !== 0 && <Button small variant="ghost" onClick={() => act(() => db.bucket(bucket).restore(objectKey, v.version_id), 'Restored as the newest version')}>Restore</Button>}
                  <Button small variant="danger" onClick={() => { if (confirm('Remove this version for good?')) void act(() => db.bucket(bucket).delete(objectKey, { version: v.version_id }), 'Version removed'); }}>Remove</Button>
                </td>
              </tr>
            ))}
            {versions.length === 0 && <tr><td colSpan={3} className="py-6 text-center text-muted">No versions left.</td></tr>}
          </tbody>
        </table>
      )}
    </Modal>
  );
}

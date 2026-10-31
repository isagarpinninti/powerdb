import { useState } from 'react';
import type { CollectionDef, FieldDef, FileInfo, PowerRecord } from '@sdk';
import { db } from '../session';
import { Badge, Button, ErrorBox, Field, Input, Textarea, fmtBytes, fmtDate, useToast } from '../ui';

type Values = Record<string, string | boolean>;

function initial(def: CollectionDef, rec: PowerRecord | null): Values {
  const v: Values = {};
  for (const f of def.fields) {
    if (f.kind === 'file') continue;
    const x = rec?.[f.name];
    v[f.name] = f.kind === 'bool' ? x === true : f.kind === 'json' ? (x === undefined ? '' : JSON.stringify(x, null, 2)) : x === undefined || x === null ? '' : String(x);
  }
  return v;
}

export function RecordDrawer({ def, rec, onClose, onSaved, onDeleted }: { def: CollectionDef; rec: PowerRecord | null; onClose: () => void; onSaved: (r: PowerRecord) => void; onDeleted: (id: string) => void }) {
  const [values, setValues] = useState<Values>(() => initial(def, rec));
  const [files, setFiles] = useState<Record<string, File | undefined>>({});
  const [remove, setRemove] = useState<Record<string, boolean>>({});
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const toast = useToast();
  const col = db.collection(def.name);

  const payload = (): Record<string, unknown> => {
    const out: Record<string, unknown> = {};
    for (const f of def.fields) {
      if (f.kind === 'file') {
        if (files[f.name]) out[f.name] = files[f.name];
        continue;
      }
      const raw = values[f.name];
      if (f.kind === 'bool') out[f.name] = raw === true;
      else if (raw === '' || raw === undefined) { if (rec) out[f.name] = null; }
      else if (f.kind === 'number') {
        const n = Number(raw);
        if (!Number.isFinite(n)) throw new Error(`"${f.name}" must be a number`);
        out[f.name] = n;
      } else if (f.kind === 'json') {
        try { out[f.name] = JSON.parse(String(raw)); } catch { throw new Error(`"${f.name}" is not valid JSON`); }
      } else out[f.name] = raw;
    }
    return out;
  };

  const save = async (e: React.FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      const data = payload();
      const saved = rec
        ? await col.update(rec.id, data, { removeFiles: Object.keys(remove).filter((k) => remove[k] && !files[k]) })
        : await col.create(data);
      toast(rec ? 'Saved' : 'Created');
      onSaved(saved);
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  };

  const del = async () => {
    if (!rec || !confirm('Delete this record and its files? This cannot be undone.')) return;
    setBusy(true);
    try {
      await col.delete(rec.id);
      toast('Deleted');
      onDeleted(rec.id);
    } catch (err) {
      setError(err);
      setBusy(false);
    }
  };

  const input = (f: FieldDef) => {
    const v = values[f.name];
    const set = (x: string | boolean) => setValues((s) => ({ ...s, [f.name]: x }));
    switch (f.kind) {
      case 'bool':
        return <label className="flex items-center gap-2 text-sm"><input type="checkbox" checked={v === true} onChange={(e) => set(e.target.checked)} className="h-4 w-4 accent-ink" /> {v === true ? 'true' : 'false'}</label>;
      case 'json':
        return <Textarea rows={4} value={String(v)} onChange={(e) => set(e.target.value)} spellCheck={false} />;
      case 'number':
        return <Input type="number" step="any" value={String(v)} onChange={(e) => set(e.target.value)} required={f.required} />;
      case 'file': {
        const cur = rec?.[f.name] as FileInfo | undefined;
        return (
          <div className="space-y-2">
            {cur && !remove[f.name] && !files[f.name] && (
              <div className="flex items-center justify-between rounded-md border border-line px-3 py-2 text-sm">
                <span className="truncate">{cur.name ?? f.name} <span className="text-muted">· {fmtBytes(cur.size)} · {cur.mime}</span></span>
                <Button small variant="danger" type="button" onClick={() => setRemove((r) => ({ ...r, [f.name]: true }))}>Remove</Button>
              </div>
            )}
            {remove[f.name] && !files[f.name] && <div className="text-sm text-danger">Will be removed on save. <button type="button" className="underline" onClick={() => setRemove((r) => ({ ...r, [f.name]: false }))}>Undo</button></div>}
            <input type="file" accept={f.mime?.join(',')} onChange={(e) => setFiles((s) => ({ ...s, [f.name]: e.target.files?.[0] }))} className="block w-full text-sm file:mr-3 file:rounded-md file:border-0 file:bg-ink file:px-3 file:py-1.5 file:font-medium file:text-bg" required={f.required && !cur} />
          </div>
        );
      }
      default:
        return <Input value={String(v)} onChange={(e) => set(e.target.value)} required={f.required} maxLength={f.max} />;
    }
  };

  return (
    <div className="fixed inset-0 z-40 flex justify-end bg-black/40" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <form onSubmit={save} className="flex h-full w-full max-w-lg flex-col bg-surface shadow-2xl" aria-label={rec ? 'Edit record' : 'New record'}>
        <div className="flex items-start justify-between border-b border-line px-6 py-4">
          <div>
            <h2 className="text-lg font-semibold">{rec ? 'Edit record' : `New ${def.name} record`}</h2>
            {rec && <p className="mt-0.5 font-mono text-xs text-muted">{rec.id}</p>}
          </div>
          <Button variant="ghost" type="button" onClick={onClose} aria-label="Close">✕</Button>
        </div>
        <div className="flex-1 space-y-4 overflow-y-auto px-6 py-5">
          {def.fields.length === 0 && <p className="text-sm text-muted">This collection has no fields yet. Add some on the Schema tab.</p>}
          {def.fields.map((f) => (
            <Field key={f.name} label={`${f.name}${f.required ? ' *' : ''}`} hint={<><Badge>{f.kind}</Badge>{f.max ? ` max ${f.kind === 'file' ? fmtBytes(f.max) : `${f.max} bytes`}` : ''}{f.mime?.length ? ` · ${f.mime.join(', ')}` : ''}</>}>
              {input(f)}
            </Field>
          ))}
          {rec && <p className="text-xs text-muted">Created {fmtDate(rec.created_at)} · updated {fmtDate(rec.updated_at)}</p>}
          <ErrorBox error={error} />
        </div>
        <div className="flex items-center justify-between border-t border-line px-6 py-4">
          {rec ? <Button variant="danger" type="button" onClick={del} disabled={busy}>Delete</Button> : <span />}
          <div className="flex gap-2">
            <Button type="button" onClick={onClose}>Cancel</Button>
            <Button variant="primary" type="submit" disabled={busy}>{busy ? 'Saving…' : 'Save'}</Button>
          </div>
        </div>
      </form>
    </div>
  );
}

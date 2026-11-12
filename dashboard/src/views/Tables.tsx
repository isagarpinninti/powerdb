import { useCallback, useEffect, useState } from 'react';
import type { ColumnSpec, TableFile, TableInfo } from '@sdk';
import { db } from '../session';
import { Badge, Button, Card, ErrorBox, Field, Input, Modal, Select, Spinner, Textarea, fmtBytes, useToast } from '../ui';

const message = (e: unknown) => (e instanceof Error ? e.message : 'Failed');

const TYPES = ['text', 'int32', 'int64', 'float64', 'bool', 'decimal', 'uuid', 'date', 'time', 'timestamp', 'timestamptz', 'json', 'binary', 'file', 'array<text>', 'array<file>'];

type DraftColumn = { name: string; type: string; nullable: boolean; pk: boolean; generate: string };
const blank = (): DraftColumn => ({ name: '', type: 'text', nullable: true, pk: false, generate: '' });

export function NewTable({ onClose, onCreated }: { onClose: () => void; onCreated: (name: string) => void }) {
  const [name, setName] = useState('');
  const [cols, setCols] = useState<DraftColumn[]>([{ name: 'id', type: 'uuid', nullable: false, pk: true, generate: 'uuid' }, blank()]);
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const set = (i: number, patch: Partial<DraftColumn>) => setCols((c) => c.map((x, j) => (j === i ? { ...x, ...patch } : x)));
  const used = cols.filter((c) => c.name.trim());

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setBusy(true);
    try {
      const columns: ColumnSpec[] = used.map((c) => ({
        name: c.name.trim(),
        type: c.type,
        nullable: c.pk ? false : c.nullable,
        ...(c.generate ? { generate: c.generate as 'uuid' | 'now' | 'sequence' } : {}),
      }));
      await db.tables.create({ name, columns, primary_key: used.filter((c) => c.pk).map((c) => c.name.trim()) });
      onCreated(name);
    } catch (err) {
      setError(err);
      setBusy(false);
    }
  };

  return (
    <Modal title="New table" onClose={onClose} wide>
      <form onSubmit={submit} className="space-y-4">
        <ErrorBox error={error} />
        <Field label="Name" hint="Lowercase letters, digits, - and _.">
          <Input value={name} onChange={(e) => setName(e.target.value)} placeholder="people" className="font-mono" autoFocus spellCheck={false} required />
        </Field>
        <div className="space-y-2">
          <div className="grid grid-cols-[1fr_9rem_6rem_5rem_6rem] gap-2 text-xs uppercase tracking-wide text-muted"><span>Column</span><span>Type</span><span>Fill in</span><span>Required</span><span>Key</span></div>
          {cols.map((c, i) => (
            <div key={i} className="grid grid-cols-[1fr_9rem_6rem_5rem_6rem] items-center gap-2">
              <Input value={c.name} onChange={(e) => set(i, { name: e.target.value })} aria-label={`Column ${i + 1} name`} className="font-mono" spellCheck={false} />
              <Select value={c.type} onChange={(e) => set(i, { type: e.target.value })} aria-label={`Column ${i + 1} type`}>{TYPES.map((t) => <option key={t}>{t}</option>)}</Select>
              <Select value={c.generate} onChange={(e) => set(i, { generate: e.target.value })} aria-label={`Column ${i + 1} generator`}>
                <option value="">—</option><option value="uuid">uuid</option><option value="now">now</option><option value="sequence">sequence</option>
              </Select>
              <input type="checkbox" checked={c.pk || !c.nullable} disabled={c.pk} onChange={(e) => set(i, { nullable: !e.target.checked })} aria-label={`Column ${i + 1} required`} />
              <label className="flex items-center gap-1 text-sm"><input type="checkbox" checked={c.pk} onChange={(e) => set(i, { pk: e.target.checked })} /> primary</label>
            </div>
          ))}
          <Button type="button" small variant="ghost" onClick={() => setCols((c) => [...c, blank()])}>Add a column</Button>
        </div>
        <p className="text-xs text-muted">The primary key identifies a row and cannot be changed afterwards. Constraints and indexes can be added from the command line or the API.</p>
        <div className="flex justify-end gap-2">
          <Button type="button" onClick={onClose}>Cancel</Button>
          <Button variant="primary" type="submit" disabled={busy || !name || !used.some((c) => c.pk)}>Create table</Button>
        </div>
      </form>
    </Modal>
  );
}

type Row = Record<string, unknown>;

function cell(v: unknown): string {
  if (v === null || v === undefined) return '';
  if (typeof v === 'object' && v !== null && 'url' in v && 'size' in v) return `${(v as TableFile).name ?? 'file'} (${fmtBytes((v as TableFile).size)})`;
  return typeof v === 'object' ? JSON.stringify(v) : String(v);
}

export function TableView({ name, onDropped }: { name: string; onDropped: () => void }) {
  const [info, setInfo] = useState<{ table: TableInfo; rows: number } | null>(null);
  const [rows, setRows] = useState<Row[] | null>(null);
  const [next, setNext] = useState<string | undefined>();
  const [error, setError] = useState<unknown>(null);
  const [adding, setAdding] = useState(false);
  const [addingColumn, setAddingColumn] = useState(false);
  const toast = useToast();

  const load = useCallback(async () => {
    try {
      const [i, page] = await Promise.all([db.tables.get(name), db.table(name).list({ limit: 100 })]);
      setInfo(i);
      setRows(page.items);
      setNext(page.next);
      setError(null);
    } catch (e) { setError(e); }
  }, [name]);
  useEffect(() => { void load(); }, [load]);

  const keyOf = (r: Row) => (info!.table.primary_key.length === 1 ? String(r[info!.table.primary_key[0]]) : info!.table.primary_key.map((k) => r[k]));
  const more = async () => {
    if (!next) return;
    try { const p = await db.table(name).list({ limit: 100, after: next }); setRows((c) => [...(c ?? []), ...p.items]); setNext(p.next); } catch (e) { toast(message(e), 'err'); }
  };
  const remove = async (r: Row) => {
    if (!confirm('Delete this row? This cannot be undone.')) return;
    try { await db.table(name).delete(keyOf(r)); toast('Deleted'); void load(); } catch (e) { toast(message(e), 'err'); }
  };
  const drop = async () => {
    if (!confirm(`Delete the table ${name} and all ${info?.rows ?? ''} of its rows? This cannot be undone.`)) return;
    try { await db.tables.drop(name); toast('Table deleted'); onDropped(); } catch (e) { toast(message(e), 'err'); }
  };
  const download = async (f: TableFile) => {
    try {
      const url = URL.createObjectURL(new Blob([new Uint8Array(await db.table(name).download(f))], { type: f.mime }));
      const a = document.createElement('a');
      a.href = url; a.download = f.name ?? 'download';
      document.body.appendChild(a); a.click(); a.remove();
      setTimeout(() => URL.revokeObjectURL(url), 10_000);
    } catch (e) { toast(message(e), 'err'); }
  };

  const t = info?.table;
  return (
    <div className="space-y-4 p-8">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <h1 className="flex items-center gap-2 text-2xl font-bold">{name}{t?.encrypt && <Badge>encrypted</Badge>}</h1>
          <p className="text-sm text-muted">{info ? `${info.rows} row${info.rows === 1 ? '' : 's'} · primary key ${t!.primary_key.join(', ')}` : ' '}</p>
        </div>
        <div className="flex gap-2">
          <Button variant="primary" onClick={() => setAdding(true)} disabled={!t}>Add a row</Button>
          <Button onClick={() => setAddingColumn(true)} disabled={!t}>Add a column</Button>
          <Button variant="danger" onClick={drop}>Delete table</Button>
        </div>
      </div>
      <ErrorBox error={error} />
      {t && (
        <Card className="p-4 text-sm">
          <div className="flex flex-wrap gap-x-6 gap-y-1">
            {t.constraints.map((c) => <span key={c.name}><Badge>{c.kind.replace('_', ' ')}</Badge> <span className="font-mono text-xs">{c.name}</span></span>)}
            {t.indexes.map((x) => <span key={x.name}><Badge tone="green">index</Badge> <span className="font-mono text-xs">{x.name} ({x.columns.join(', ')})</span></span>)}
            {t.constraints.length + t.indexes.length === 0 && <span className="text-muted">No constraints or indexes.</span>}
          </div>
        </Card>
      )}
      {rows === null || !t ? <Spinner /> : (
        <Card className="overflow-x-auto">
          <table className="w-full text-sm">
            <thead className="bg-raised text-left text-xs uppercase tracking-wide text-muted">
              <tr>{t.columns.map((c) => <th key={c.name} className="px-4 py-2 whitespace-nowrap">{c.name} <span className="font-normal normal-case text-faint">{c.type}</span></th>)}<th className="px-4" /></tr>
            </thead>
            <tbody>
              {rows.map((r, i) => (
                <tr key={i} className="border-t border-line">
                  {t.columns.map((c) => {
                    const v = r[c.name];
                    const isFile = typeof v === 'object' && v !== null && 'url' in v;
                    return (
                      <td key={c.name} className="max-w-xs truncate px-4 py-2 font-mono text-[13px]" title={cell(v)}>
                        {isFile ? <button className="text-accent underline" onClick={() => void download(v as TableFile)}>{cell(v)}</button> : cell(v) || <span className="text-faint">—</span>}
                      </td>
                    );
                  })}
                  <td className="whitespace-nowrap px-4 text-right"><Button small variant="danger" onClick={() => remove(r)}>Delete</Button></td>
                </tr>
              ))}
              {rows.length === 0 && <tr><td colSpan={t.columns.length + 1} className="px-4 py-10 text-center text-muted">No rows yet.</td></tr>}
            </tbody>
          </table>
        </Card>
      )}
      {next && <div><Button onClick={more}>Load more</Button></div>}
      {adding && t && <AddRow table={t} onClose={() => setAdding(false)} onAdded={() => { setAdding(false); toast('Row added'); void load(); }} />}
      {addingColumn && <AddColumn table={name} onClose={() => setAddingColumn(false)} onAdded={() => { setAddingColumn(false); toast('Column added'); void load(); }} />}
    </div>
  );
}

function AddRow({ table, onClose, onAdded }: { table: TableInfo; onClose: () => void; onAdded: () => void }) {
  const fileColumns = table.columns.filter((c) => c.type === 'file');
  const [text, setText] = useState(() => JSON.stringify(Object.fromEntries(table.columns.filter((c) => c.type !== 'file' && !c.generate).map((c) => [c.name, null])), null, 2));
  const [files, setFiles] = useState<Record<string, File>>({});
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setBusy(true);
    try {
      const values = JSON.parse(text) as Record<string, unknown>;
      // a null in the template means "leave it out", so defaults and generators apply
      for (const k of Object.keys(values)) if (values[k] === null) delete values[k];
      await db.table(table.name).insert(values, files);
      onAdded();
    } catch (err) { setError(err instanceof SyntaxError ? new Error(`That is not valid JSON: ${err.message}`) : err); setBusy(false); }
  };
  return (
    <Modal title={`Add a row to ${table.name}`} onClose={onClose} wide>
      <form onSubmit={submit} className="space-y-4">
        <ErrorBox error={error} />
        <Field label="Values as JSON" hint="Leave a column out or null to use its default or generated value. 64-bit integers and decimals are strings.">
          <Textarea value={text} onChange={(e) => setText(e.target.value)} rows={Math.min(14, text.split('\n').length + 1)} className="font-mono text-[13px]" spellCheck={false} />
        </Field>
        {fileColumns.map((c) => (
          <Field key={c.name} label={`File for ${c.name}`}>
            <input type="file" aria-label={`File for ${c.name}`} onChange={(e) => { const f = e.target.files?.[0]; setFiles((cur) => { const n = { ...cur }; if (f) n[c.name] = f; else delete n[c.name]; return n; }); }} />
          </Field>
        ))}
        <div className="flex justify-end gap-2">
          <Button type="button" onClick={onClose}>Cancel</Button>
          <Button variant="primary" type="submit" disabled={busy}>Add row</Button>
        </div>
      </form>
    </Modal>
  );
}

function AddColumn({ table, onClose, onAdded }: { table: string; onClose: () => void; onAdded: () => void }) {
  const [name, setName] = useState('');
  const [type, setType] = useState('text');
  const [error, setError] = useState<unknown>(null);
  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    try { await db.tables.alter(table, { op: 'add_column', name, type, nullable: true }); onAdded(); } catch (err) { setError(err); }
  };
  return (
    <Modal title={`Add a column to ${table}`} onClose={onClose}>
      <form onSubmit={submit} className="space-y-4">
        <ErrorBox error={error} />
        <Field label="Name"><Input value={name} onChange={(e) => setName(e.target.value)} className="font-mono" autoFocus spellCheck={false} required /></Field>
        <Field label="Type" hint="Existing rows get an empty value. Rows are not rewritten."><Select value={type} onChange={(e) => setType(e.target.value)}>{TYPES.map((t) => <option key={t}>{t}</option>)}</Select></Field>
        <div className="flex justify-end gap-2"><Button type="button" onClick={onClose}>Cancel</Button><Button variant="primary" type="submit" disabled={!name}>Add column</Button></div>
      </form>
    </Modal>
  );
}

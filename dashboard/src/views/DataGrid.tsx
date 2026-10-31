import { useVirtualizer } from '@tanstack/react-virtual';
import { useCallback, useEffect, useRef, useState } from 'react';
import type { CollectionDef, FileInfo, PowerRecord } from '@sdk';
import { db } from '../session';
import { Button, ErrorBox, Input, Spinner, cn, fmtBytes, fmtDate, useToast } from '../ui';
import { RecordDrawer } from './RecordDrawer';

const PAGE = 100;
const ROW = 60;

type Row = PowerRecord;

/** Image files show a thumbnail; everything else shows a labelled icon. Links last a minute, so an expired one is replaced by asking for the record again. */
function FileCell({ rec, field, refresh }: { rec: Row; field: string; refresh: (id: string) => void }) {
  const f = rec[field] as FileInfo | undefined;
  const toast = useToast();
  if (!f) return <span className="text-faint">—</span>;
  const url = db.fileUrl(f);
  const open = async () => {
    try {
      const fresh = await db.collection(rec.collection).get(rec.id);
      const u = db.fileUrl(fresh[field] as FileInfo | undefined);
      if (u) window.open(u, '_blank', 'noopener');
    } catch (e) {
      toast(e instanceof Error ? e.message : 'Could not open the file', 'err');
    }
  };
  const isImage = f.mime.startsWith('image/');
  return (
    <button onClick={(e) => { e.stopPropagation(); void open(); }} className="flex items-center gap-2 text-left" title={`${f.name ?? field} · ${fmtBytes(f.size)} · click to open`}>
      {isImage && url ? (
        <img src={url} alt={f.name ?? field} loading="lazy" className="h-11 w-11 shrink-0 rounded border border-line object-cover" onError={() => refresh(rec.id)} />
      ) : (
        <span className="flex h-11 w-11 shrink-0 flex-col items-center justify-center rounded border border-line bg-raised text-[10px] font-bold text-muted">
          {f.mime === 'application/pdf' ? 'PDF' : (f.mime.split('/')[1] ?? 'FILE').slice(0, 4).toUpperCase()}
        </span>
      )}
      <span className="min-w-0">
        <span className="block truncate text-xs font-medium">{f.name ?? field}</span>
        <span className="block text-[11px] text-muted">{fmtBytes(f.size)}</span>
      </span>
    </button>
  );
}

function Cell({ rec, def, col, refresh }: { rec: Row; def: CollectionDef; col: string; refresh: (id: string) => void }) {
  if (col === 'id') return <span className="font-mono text-xs text-muted">{rec.id.slice(0, 8)}…</span>;
  if (col === 'created_at') return <span className="text-xs text-muted">{fmtDate(rec.created_at)}</span>;
  const fd = def.fields.find((f) => f.name === col);
  if (fd?.kind === 'file') return <FileCell rec={rec} field={col} refresh={refresh} />;
  const v = rec[col];
  if (v === undefined || v === null || v === '') return <span className="text-faint">—</span>;
  if (typeof v === 'boolean') return <span>{v ? '✓ true' : '✗ false'}</span>;
  if (typeof v === 'object') return <code className="truncate text-xs">{JSON.stringify(v)}</code>;
  return <span className="truncate">{String(v)}</span>;
}

export function DataGrid({ def, onCount }: { def: CollectionDef; onCount?: (n: number | null) => void }) {
  const [rows, setRows] = useState<Row[]>([]);
  const [hasMore, setHasMore] = useState(false);
  const [total, setTotal] = useState<number | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [filterText, setFilterText] = useState('');
  const [filter, setFilter] = useState('');
  const [sort, setSort] = useState('-created_at');
  const [drawer, setDrawer] = useState<{ rec: Row | null } | null>(null);
  const scroller = useRef<HTMLDivElement>(null);
  const gen = useRef(0);

  const columns = ['id', ...def.fields.map((f) => f.name), 'created_at'];
  const widths = columns.map((c) => (c === 'id' ? 120 : c === 'created_at' ? 190 : def.fields.find((f) => f.name === c)?.kind === 'file' ? 220 : 180));
  const template = widths.map((w) => `${w}px`).join(' ');
  const totalWidth = widths.reduce((a, b) => a + b, 0) + 24;

  const load = useCallback(async (reset: boolean, from: number) => {
    const my = ++gen.current;
    setLoading(true);
    try {
      const q = db.collection(def.name).query();
      if (filter) q.where(filter);
      const page = await q.sort(sort).limit(PAGE).offset(from).get();
      if (my !== gen.current) return;
      setRows((r) => (reset ? page.items : [...r, ...page.items]));
      setHasMore(page.has_more);
      setTotal(page.total);
      setError(null);
    } catch (e) {
      if (my === gen.current) { setError(e); setHasMore(false); }
    } finally {
      if (my === gen.current) setLoading(false);
    }
  }, [def.name, filter, sort]);

  useEffect(() => { setRows([]); void load(true, 0); }, [load]);
  useEffect(() => { onCount?.(total); }, [total, onCount]);

  const virt = useVirtualizer({ count: rows.length + (hasMore ? 1 : 0), getScrollElement: () => scroller.current, estimateSize: () => ROW, overscan: 8 });
  const items = virt.getVirtualItems();
  const last = items[items.length - 1];
  useEffect(() => {
    if (last && last.index >= rows.length && hasMore && !loading) void load(false, rows.length);
  }, [last, rows.length, hasMore, loading, load]);

  const refreshRow = useCallback(async (id: string) => {
    try {
      const fresh = await db.collection(def.name).get(id);
      setRows((r) => r.map((x) => (x.id === id ? fresh : x)));
    } catch { /* the record may be gone */ }
  }, [def.name]);

  const toggleSort = (col: string) => setSort((s) => (s === col ? `-${col}` : s === `-${col}` ? '-created_at' : col));
  const arrow = (col: string) => (sort === col ? ' ▲' : sort === `-${col}` ? ' ▼' : '');

  return (
    <div className="flex h-full min-h-0 flex-col">
      <form className="flex flex-wrap items-center gap-2 border-b border-line px-6 py-3" onSubmit={(e) => { e.preventDefault(); setFilter(filterText.trim()); }}>
        <Input className="max-w-xl flex-1 font-mono" value={filterText} onChange={(e) => setFilterText(e.target.value)} placeholder={`filter, e.g. ${def.fields[0] ? `${def.fields[0].name} ~ "acme" && created_at > 0` : 'created_at > 0'}`} aria-label="Filter expression" spellCheck={false} />
        <Button type="submit">Filter</Button>
        {filter && <Button variant="ghost" type="button" onClick={() => { setFilterText(''); setFilter(''); }}>Clear</Button>}
        <span className="ml-auto text-xs text-muted">{total !== null ? `${total} record${total === 1 ? '' : 's'}` : `${rows.length}+ loaded`}</span>
        <Button variant="primary" type="button" onClick={() => setDrawer({ rec: null })}>New record</Button>
      </form>
      {error ? <div className="px-6 pt-3"><ErrorBox error={error} /></div> : null}
      <div ref={scroller} className="min-h-0 flex-1 overflow-auto" role="table" aria-label={`${def.name} records`} aria-rowcount={total ?? rows.length}>
        <div style={{ width: totalWidth }}>
          <div role="row" className="sticky top-0 z-10 grid border-b border-line bg-raised px-3 text-xs font-semibold uppercase tracking-wide text-muted" style={{ gridTemplateColumns: template }}>
            {columns.map((c) => (
              <button key={c} role="columnheader" onClick={() => toggleSort(c)} className="truncate px-2 py-2.5 text-left hover:text-ink">{c}{arrow(c)}</button>
            ))}
          </div>
          <div style={{ height: virt.getTotalSize(), position: 'relative' }}>
            {items.map((vi) => {
              const rec = rows[vi.index];
              return (
                <div key={vi.key} role="row" className={cn('absolute left-0 grid w-full cursor-pointer items-center border-b border-line px-3 text-sm hover:bg-raised', rec ? '' : 'text-faint')} style={{ height: ROW, transform: `translateY(${vi.start}px)`, gridTemplateColumns: template }} onClick={() => rec && setDrawer({ rec })}>
                  {rec ? columns.map((c) => <div key={c} role="cell" className="min-w-0 truncate px-2"><Cell rec={rec} def={def} col={c} refresh={refreshRow} /></div>) : <div className="px-2"><Spinner /></div>}
                </div>
              );
            })}
          </div>
          {!loading && rows.length === 0 && !error && (
            <div className="p-10 text-center text-muted">{filter ? 'No records match this filter.' : 'No records yet. Create the first one.'}</div>
          )}
        </div>
      </div>
      {drawer && (
        <RecordDrawer
          def={def}
          rec={drawer.rec}
          onClose={() => setDrawer(null)}
          onSaved={(r) => { setRows((x) => (drawer.rec ? x.map((y) => (y.id === r.id ? r : y)) : [r, ...x])); if (!drawer.rec) setTotal((t) => (t === null ? t : t + 1)); setDrawer(null); }}
          onDeleted={(id) => { setRows((x) => x.filter((y) => y.id !== id)); setTotal((t) => (t === null ? t : t - 1)); setDrawer(null); }}
        />
      )}
    </div>
  );
}

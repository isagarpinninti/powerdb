import { useCallback, useEffect, useState } from 'react';
import type { CollectionDef } from '@sdk';
import { db } from '../session';
import { Badge, ErrorBox, Spinner, cn } from '../ui';
import { CodeTab } from './CodeTab';
import { DataGrid } from './DataGrid';
import { RulesTab } from './RulesTab';
import { SchemaTab } from './SchemaTab';

type Tab = 'data' | 'schema' | 'rules' | 'code';
const TABS: Array<[Tab, string]> = [['data', 'Data'], ['schema', 'Schema'], ['rules', 'Access rules'], ['code', 'Code']];

export function CollectionView({ name, onChanged, onDropped }: { name: string; onChanged: () => void; onDropped: () => void }) {
  const [def, setDef] = useState<CollectionDef | null>(null);
  const [count, setCount] = useState<number | null>(null);
  const [tab, setTab] = useState<Tab>('data');
  const [error, setError] = useState<unknown>(null);

  useEffect(() => {
    db.admin.collections.get(name).then((r) => { setDef(r.collection); setCount(r.records); }, setError);
  }, [name]);

  const onCount = useCallback((n: number | null) => n !== null && setCount(n), []);
  const changed = (d: CollectionDef) => { setDef(d); onChanged(); };

  if (error) return <div className="p-6"><ErrorBox error={error} /></div>;
  if (!def) return <div className="p-10"><Spinner /></div>;
  const r = def.rules ?? {};
  const locked = !r.list && r.list !== '' && !r.view && r.view !== '' && !r.create && r.create !== '';

  return (
    <div className="flex h-full min-h-0 flex-col">
      <header className="border-b border-line bg-surface px-6 pt-5">
        <div className="flex items-center gap-3">
          <h1 className="text-xl font-bold">{def.name}</h1>
          {def.encrypt && <Badge tone="gold">files encrypted</Badge>}
          {locked && <Badge tone="red">locked: administrators only</Badge>}
          <span className="text-sm text-muted">{count === null ? '' : `${count} record${count === 1 ? '' : 's'}`}</span>
        </div>
        <nav className="mt-4 flex gap-1" role="tablist">
          {TABS.map(([k, label]) => (
            <button key={k} role="tab" aria-selected={tab === k} onClick={() => setTab(k)} className={cn('-mb-px border-b-2 px-4 py-2 text-sm font-medium transition-colors', tab === k ? 'border-ink text-ink' : 'border-transparent text-muted hover:text-ink')}>
              {label}
            </button>
          ))}
        </nav>
      </header>
      <div className="min-h-0 flex-1 overflow-auto">
        {tab === 'data' && <DataGrid def={def} onCount={onCount} />}
        {tab === 'schema' && <SchemaTab def={def} onSaved={changed} onDropped={onDropped} />}
        {tab === 'rules' && <RulesTab def={def} onSaved={changed} />}
        {tab === 'code' && <CodeTab def={def} />}
      </div>
    </div>
  );
}

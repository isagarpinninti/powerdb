import { useCallback, useEffect, useState } from 'react';
import type { BucketInfo, CollectionDef, TableInfo } from '@sdk';
import { Logo } from './Logo';
import { applyTheme, readTheme } from './theme';
import { DESKTOP_ORIGINS, db, embedded, useSession } from './session';
import { Button, ErrorBox, Spinner, cn } from './ui';
import { CollectionView } from './views/CollectionView';
import { BucketView, NewBucket } from './views/Files';
import { Keys } from './views/Keys';
import { Login } from './views/Login';
import { NewCollection } from './views/NewCollection';
import { NewTable, TableView } from './views/Tables';
import { System } from './views/System';
import { Users } from './views/Users';

export type Nav = { page: 'collection'; name: string } | { page: 'bucket'; name: string } | { page: 'table'; name: string } | { page: 'new' } | { page: 'users' } | { page: 'keys' } | { page: 'system' } | { page: 'home' };

function useTheme() {
  const [dark, setDark] = useState(() => readTheme() === 'dark');
  const toggle = () => {
    const next = dark ? 'light' : 'dark';
    applyTheme(next);
    setDark(next === 'dark');
  };
  return { dark, toggle };
}

export function App() {
  const { identity, logout } = useSession();
  const theme = useTheme();
  const [nav, setNav] = useState<Nav>({ page: 'home' });
  const [collections, setCollections] = useState<CollectionDef[] | null>(null);
  const [buckets, setBuckets] = useState<BucketInfo[] | null>(null);
  const [tables, setTables] = useState<TableInfo[] | null>(null);
  const [newBucket, setNewBucket] = useState(false);
  const [newTable, setNewTable] = useState(false);
  const [error, setError] = useState<unknown>(null);

  const refresh = useCallback(async () => {
    try {
      const [cols, bks, tbs] = await Promise.all([db.admin.collections.list(), db.buckets.list(), db.tables.list()]);
      setCollections(cols);
      setBuckets(bks);
      setTables(tbs);
      setError(null);
    } catch (e) {
      setError(e);
    }
  }, []);
  useEffect(() => { if (identity) void refresh(); }, [identity, refresh]);

  // Inside the PowerDB desktop app the app supplies the logo, sign-out and theme switch, and tells this page which theme to use.
  useEffect(() => {
    if (!embedded) return;
    const onMessage = (e: MessageEvent) => {
      if (!DESKTOP_ORIGINS.includes(e.origin) || e.data?.powerdb !== 'theme') return;
      if (e.data.theme === 'light' || e.data.theme === 'dark') applyTheme(e.data.theme);
    };
    window.addEventListener('message', onMessage);
    return () => window.removeEventListener('message', onMessage);
  }, []);

  if (!identity) return <Login />;

  const item = (active: boolean) => cn('flex w-full items-center gap-2 rounded-md px-3 py-1.5 text-left text-sm transition-colors', active ? 'bg-raised font-medium text-ink' : 'text-muted hover:bg-raised hover:text-ink');

  return (
    <div className="flex h-full">
      <aside className={cn('flex shrink-0 flex-col bg-surface text-ink border-r border-line', embedded ? 'w-56' : 'w-64')} aria-label="Navigation">
        {embedded ? <div className="h-4" /> : (
          <button className="flex items-center gap-3 px-5 py-5" onClick={() => setNav({ page: 'home' })}>
            <Logo size={30} />
            <span className="text-lg font-bold tracking-tight text-ink">PowerDB</span>
          </button>
        )}
        <nav className="flex-1 space-y-6 overflow-y-auto px-3 pb-4">
          <div>
            <div className="mb-1 flex items-center justify-between px-3 text-[11px] font-semibold uppercase tracking-wider text-muted">
              Collections
              <button className="rounded px-1.5 text-base leading-none text-muted hover:bg-raised" title="New collection" aria-label="New collection" onClick={() => setNav({ page: 'new' })}>+</button>
            </div>
            {collections === null && !error && <div className="px-3 py-2"><Spinner /></div>}
            {collections?.length === 0 && <p className="px-3 py-2 text-xs text-muted">No collections yet.</p>}
            {collections?.map((c) => (
              <button key={c.name} className={item(nav.page === 'collection' && nav.name === c.name)} onClick={() => setNav({ page: 'collection', name: c.name })}>
                <span className="truncate">{c.name}</span>
                {c.encrypt && <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.9" strokeLinecap="round" strokeLinejoin="round" className="shrink-0 text-faint" role="img" aria-label="Files encrypted at rest"><title>Files encrypted at rest</title><rect x="5" y="11" width="14" height="9" rx="2" /><path d="M8 11V8a4 4 0 018 0v3" /></svg>}
              </button>
            ))}
          </div>
          <div>
            <div className="mb-1 flex items-center justify-between px-3 text-[11px] font-semibold uppercase tracking-wider text-muted">
              Files
              <button className="rounded px-1.5 text-base leading-none text-muted hover:bg-raised" title="New bucket" aria-label="New bucket" onClick={() => setNewBucket(true)}>+</button>
            </div>
            {buckets?.length === 0 && <p className="px-3 py-2 text-xs text-muted">No buckets yet.</p>}
            {buckets?.map((b) => (
              <button key={b.name} className={item(nav.page === 'bucket' && nav.name === b.name)} onClick={() => setNav({ page: 'bucket', name: b.name })}>
                <span className="truncate">{b.name}</span>
              </button>
            ))}
          </div>
          <div>
            <div className="mb-1 flex items-center justify-between px-3 text-[11px] font-semibold uppercase tracking-wider text-muted">
              Tables
              <button className="rounded px-1.5 text-base leading-none text-muted hover:bg-raised" title="New table" aria-label="New table" onClick={() => setNewTable(true)}>+</button>
            </div>
            {tables?.length === 0 && <p className="px-3 py-2 text-xs text-muted">No tables yet.</p>}
            {tables?.map((t) => (
              <button key={t.name} className={item(nav.page === 'table' && nav.name === t.name)} onClick={() => setNav({ page: 'table', name: t.name })}>
                <span className="truncate">{t.name}</span>
              </button>
            ))}
          </div>
          <div>
            <div className="mb-1 px-3 text-[11px] font-semibold uppercase tracking-wider text-muted">Administration</div>
            <button className={item(nav.page === 'users')} onClick={() => setNav({ page: 'users' })}>Users</button>
            <button className={item(nav.page === 'keys')} onClick={() => setNav({ page: 'keys' })}>API keys</button>
            <button className={item(nav.page === 'system')} onClick={() => setNav({ page: 'system' })}>System</button>
          </div>
        </nav>
        {!embedded && (
        <div className="space-y-2 border-t border-line p-4 text-xs">
          <div className="truncate text-faint" title={identity.label}>{identity.label}</div>
          <div className="flex gap-2">
            <Button small variant="ghost" className="!text-muted hover:!bg-raised hover:!text-ink" onClick={theme.toggle}>{theme.dark ? 'Light' : 'Dark'}</Button>
            <Button small variant="ghost" className="!text-muted hover:!bg-raised hover:!text-ink" onClick={logout}>Sign out</Button>
          </div>
        </div>
        )}
      </aside>
      <main className="min-w-0 flex-1 overflow-hidden">
        <div className="h-full overflow-auto">
          {error ? <div className="p-6"><ErrorBox error={error} /></div> : null}
          {nav.page === 'home' && <Home collections={collections} go={setNav} />}
          {nav.page === 'new' && <NewCollection onCreated={async (name) => { await refresh(); setNav({ page: 'collection', name }); }} />}
          {nav.page === 'collection' && <CollectionView key={nav.name} name={nav.name} onChanged={refresh} onDropped={async () => { await refresh(); setNav({ page: 'home' }); }} />}
          {nav.page === 'bucket' && <BucketView key={nav.name} name={nav.name} onChanged={refresh} onDropped={async () => { await refresh(); setNav({ page: 'home' }); }} />}
          {nav.page === 'table' && <TableView key={nav.name} name={nav.name} onDropped={async () => { await refresh(); setNav({ page: 'home' }); }} />}
          {nav.page === 'users' && <Users />}
          {nav.page === 'keys' && <Keys collections={collections ?? []} />}
          {nav.page === 'system' && <System />}
        </div>
      </main>
      {newBucket && <NewBucket onClose={() => setNewBucket(false)} onCreated={async (name) => { setNewBucket(false); await refresh(); setNav({ page: 'bucket', name }); }} />}
      {newTable && <NewTable onClose={() => setNewTable(false)} onCreated={async (name) => { setNewTable(false); await refresh(); setNav({ page: 'table', name }); }} />}
    </div>
  );
}

function Home({ collections, go }: { collections: CollectionDef[] | null; go: (n: Nav) => void }) {
  return (
    <div className="space-y-6 p-10">
      <h1 className="text-2xl font-bold">Collections</h1>
      <p className="text-muted">
        Records and the files that belong to them live in one database, and a record appears together with its files or not at all. Collections are <b>locked</b> until you give them rules: until then only administrators can read or write them.
      </p>
      {collections?.length === 0 ? (
        <Button variant="primary" onClick={() => go({ page: 'new' })}>Create your first collection</Button>
      ) : (
        <ul className="grid gap-3 sm:grid-cols-2">
          {collections?.map((c) => (
            <li key={c.name}>
              <button onClick={() => go({ page: 'collection', name: c.name })} className="w-full rounded-lg border border-line bg-surface p-4 text-left shadow-sm hover:border-ink">
                <div className="font-semibold">{c.name}</div>
                <div className="text-xs text-muted">{c.fields.length} fields{c.encrypt ? ' · files encrypted' : ''}</div>
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

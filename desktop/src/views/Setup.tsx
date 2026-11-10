import { useState } from 'react';
import { open } from '@tauri-apps/plugin-dialog';
import { Button, Card, CopyButton, Field, Input, cn } from '@dash/ui';
import { Logo } from '@dash/Logo';
import { api, errorText } from '../api';
import { useApp } from '../app';
import { ErrorLine, Toggle } from '../common';

type Mode = 'new' | 'existing';
const steps = ['Welcome', 'Database', 'Ready'];

export function Setup() {
  const { snap, refresh } = useApp();
  const [step, setStep] = useState(0);
  const [mode, setMode] = useState<Mode>('new');
  const [dir, setDir] = useState(snap.default_data_dir);
  const [port, setPort] = useState(String(snap.config.port));
  const [login, setLogin] = useState(true);
  const [existingKey, setExistingKey] = useState('');
  const [key, setKey] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const pick = async () => {
    const p = await open({ directory: true, title: mode === 'new' ? 'Choose an empty folder for the database' : 'Choose the database folder' });
    if (typeof p === 'string') setDir(p);
  };

  const create = async () => {
    setBusy(true); setError(null);
    try {
      const p = Number(port);
      if (!Number.isInteger(p) || p < 1024 || p > 65535) throw new Error('The port must be a number from 1024 to 65535.');
      if (mode === 'new') { setKey(await api.setupNew(dir, p, login)); setStep(2); }
      else { await api.setupExisting(dir, p, existingKey, login); await api.start(); await refresh(); }
    } catch (e) { setError(errorText(e)); } finally { setBusy(false); }
  };

  const finish = async () => {
    setBusy(true); setError(null);
    try { await api.start(); await refresh(); } catch (e) { setError(errorText(e)); setBusy(false); }
  };

  return (
    <div className="grid h-full grid-cols-[320px_1fr] no-select">
      <aside className="flex flex-col justify-between border-r border-line bg-surface p-8">
        <div>
          <Logo size={44} />
          <h1 className="mt-5 text-2xl font-bold tracking-tight">PowerDB</h1>
          <p className="mt-1 text-sm text-muted">One database for records and the files that belong to them.</p>
          <ol className="mt-10 space-y-4">
            {steps.map((s, i) => (
              <li key={s} className="flex items-center gap-3 text-sm">
                <span className={cn('flex h-6 w-6 items-center justify-center rounded-full border text-xs font-semibold', i < step ? 'border-ok bg-ok/15 text-ok' : i === step ? 'border-ink bg-ink text-bg' : 'border-line-strong text-faint')}>{i < step ? '✓' : i + 1}</span>
                <span className={i === step ? 'font-medium' : 'text-muted'}>{s}</span>
              </li>
            ))}
          </ol>
        </div>
        <p className="text-xs text-faint">Version {snap.app_version} · MIT licensed</p>
      </aside>

      <main className="overflow-auto p-12">
        <div className="mx-auto max-w-xl space-y-6">
          {step === 0 && (
            <>
              <div>
                <h2 className="text-3xl font-bold tracking-tight">Set up PowerDB</h2>
                <p className="mt-3 text-muted">PowerDB stores records and the files attached to them together. A record and its files are saved in one step, so you never end up with one and not the other.</p>
              </div>
              <div className="space-y-3 text-sm text-muted">
                <p>This sets up a server that runs on this computer. Other computers cannot reach it unless you change that later.</p>
                <p>Everything is kept in one folder that you pick next. Copying that folder while the server is stopped is a valid backup.</p>
              </div>
              <Button variant="primary" onClick={() => setStep(1)}>Continue</Button>
            </>
          )}

          {step === 1 && (
            <>
              <div><h2 className="text-3xl font-bold tracking-tight">Your database</h2><p className="mt-2 text-muted">Create a new database, or open one you already have.</p></div>
              <div className="grid grid-cols-2 gap-3" role="radiogroup" aria-label="Database">
                {([['new', 'Create a new database', 'Start fresh in an empty folder.'], ['existing', 'Open an existing database', 'Use a folder made by PowerDB before.']] as [Mode, string, string][]).map(([m, t, d]) => (
                  <button key={m} type="button" role="radio" aria-checked={mode === m} onClick={() => { setMode(m); setError(null); if (m === 'new') setDir(snap.default_data_dir); else setDir(''); }}
                    className={cn('rounded-lg border p-4 text-left transition-colors', mode === m ? 'border-ink bg-raised' : 'border-line hover:bg-raised')}>
                    <div className="text-sm font-semibold">{t}</div><div className="mt-0.5 text-xs text-muted">{d}</div>
                  </button>))}
              </div>
              <Field label="Data folder" hint={mode === 'new' ? 'It must be new or empty.' : 'The folder that contains the FORMAT file.'}>
                <div className="flex gap-2"><Input value={dir} onChange={(e) => setDir(e.target.value)} placeholder="Choose a folder" aria-label="Data folder" /><Button onClick={pick}>Browse…</Button></div>
              </Field>
              {mode === 'existing' && <Field label="Administrator key" hint="It starts with pdb_live_. It is kept in your system's credential store."><Input type="password" value={existingKey} onChange={(e) => setExistingKey(e.target.value)} placeholder="pdb_live_…" /></Field>}
              <Field label="Port" hint="The server answers on http://127.0.0.1:<port>."><Input value={port} onChange={(e) => setPort(e.target.value)} className="w-32" inputMode="numeric" /></Field>
              <label className="flex items-center justify-between gap-4 rounded-lg border border-line px-4 py-3 text-sm"><span><span className="font-medium">Open PowerDB when I sign in</span><span className="block text-xs text-muted">It starts hidden in the menu bar or system tray.</span></span><Toggle checked={login} onChange={setLogin} label="Open PowerDB at sign-in" /></label>
              <ErrorLine error={error} />
              <div className="flex gap-2"><Button variant="ghost" onClick={() => setStep(0)}>Back</Button><Button variant="primary" disabled={busy || !dir || (mode === 'existing' && !existingKey)} onClick={create}>{busy ? 'Working…' : mode === 'new' ? 'Create database' : 'Open database'}</Button></div>
            </>
          )}

          {step === 2 && key && (
            <>
              <div><h2 className="text-3xl font-bold tracking-tight">Your database is ready</h2><p className="mt-2 text-muted">This is the administrator key. It is shown <b>once</b> here and also kept in your system's credential store, which signs the dashboard in for you. Save a copy somewhere safe: it is the only way to administer this database from another computer.</p></div>
              <Card className="space-y-3 p-5">
                <code className="block break-all rounded-md bg-raised p-3 text-sm select-text" data-testid="admin-key">{key}</code>
                <div className="flex items-center justify-between"><CopyButton text={key} label="Copy key" /><label className="flex items-center gap-2 text-sm"><input type="checkbox" checked={saved} onChange={(e) => setSaved(e.target.checked)} /> I have saved this key</label></div>
              </Card>
              <ErrorLine error={error} />
              <Button variant="primary" disabled={!saved || busy} onClick={finish}>{busy ? 'Starting the server…' : 'Start PowerDB'}</Button>
            </>
          )}
        </div>
      </main>
    </div>
  );
}

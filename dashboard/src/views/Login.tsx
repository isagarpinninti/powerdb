import { useState } from 'react';
import { Logo } from '../Logo';
import { useSession } from '../session';
import { Button, ErrorBox, Field, Input, cn } from '../ui';

export function Login() {
  const { loginWithKey, loginWithPassword } = useSession();
  const [mode, setMode] = useState<'key' | 'password'>('key');
  const [key, setKey] = useState('');
  const [email, setEmail] = useState('');
  const [password, setPassword] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      if (mode === 'key') await loginWithKey(key);
      else await loginWithPassword(email, password);
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="flex min-h-full items-center justify-center bg-bg p-4">
      <form onSubmit={submit} className="w-full max-w-sm space-y-5 rounded-2xl bg-surface p-7 shadow-2xl">
        <div className="flex items-center gap-3">
          <Logo size={36} />
          <div>
            <h1 className="text-xl font-bold leading-tight">PowerDB</h1>
            <p className="text-xs text-muted">Sign in as an administrator</p>
          </div>
        </div>
        <div className="grid grid-cols-2 rounded-lg bg-raised p-1 text-sm border border-line" role="tablist">
          {(['key', 'password'] as const).map((m) => (
            <button key={m} type="button" role="tab" aria-selected={mode === m} onClick={() => { setMode(m); setError(null); }} className={cn('rounded-md py-1.5 font-medium transition-colors', mode === m ? 'bg-surface text-ink shadow-sm border border-line-strong' : 'text-muted')}>
              {m === 'key' ? 'Admin key' : 'Email & password'}
            </button>
          ))}
        </div>
        {mode === 'key' ? (
          <Field label="Admin API key" hint="Printed once in the terminal the first time the server started.">
            <Input autoFocus type="password" autoComplete="off" value={key} onChange={(e) => setKey(e.target.value)} placeholder="pdb_live_…" required />
          </Field>
        ) : (
          <>
            <Field label="Email"><Input autoFocus type="email" autoComplete="username" value={email} onChange={(e) => setEmail(e.target.value)} required /></Field>
            <Field label="Password"><Input type="password" autoComplete="current-password" value={password} onChange={(e) => setPassword(e.target.value)} required /></Field>
          </>
        )}
        <ErrorBox error={error} />
        <Button type="submit" variant="primary" className="w-full" disabled={busy}>{busy ? 'Signing in…' : 'Sign in'}</Button>
      </form>
    </div>
  );
}

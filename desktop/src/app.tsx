import { createContext, useCallback, useContext, useEffect, useState, type ReactNode } from 'react';
import { api, errorText, onLog, onStatus, type Snapshot, type Status } from './api';
import { logUi } from './boot';

interface AppCtx {
  snap: Snapshot;
  status: Status;
  logs: string[];
  refresh: () => Promise<void>;
}
const Ctx = createContext<AppCtx | null>(null);
export const useApp = () => {
  const c = useContext(Ctx);
  if (!c) throw new Error('outside the app');
  return c;
};

/** Holds the app's state and keeps it live from the events the native side emits. */
export function AppProvider({ children, fallback }: { children: ReactNode; fallback: ReactNode }) {
  const [snap, setSnap] = useState<Snapshot | null>(null);
  const [status, setStatus] = useState<Status | null>(null);
  const [logs, setLogs] = useState<string[]>([]);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      logUi('info', 'snapshot: asking');
      const s = await api.snapshot();
      logUi('info', `snapshot: got it (configured=${s.config.configured}, phase=${s.status.phase})`);
      setSnap(s);
      setStatus(s.status);
      setError(null);
    } catch (e) {
      logUi('error', `snapshot failed: ${errorText(e)}`);
      setError(errorText(e));
    }
  }, []);

  useEffect(() => {
    void refresh();
    void api.logs().then(setLogs);
    const subs = [onStatus(setStatus), onLog((l) => setLogs((p) => (p.length > 3000 ? [...p.slice(-2500), l] : [...p, l])))];
    return () => { subs.forEach((s) => void Promise.resolve(s).then((u) => typeof u === 'function' && u())); };
  }, [refresh]);

  if (error && !snap) {
    return (
      <div className="flex h-full items-center justify-center p-8" role="alert">
        <div className="max-w-lg space-y-3 rounded-lg border border-line bg-surface p-6">
          <div className="text-lg font-bold">PowerDB could not read its state</div>
          <pre className="whitespace-pre-wrap text-sm text-danger">{error}</pre>
          <button className="rounded-md border border-line-strong bg-raised px-3.5 py-2 text-sm font-medium" onClick={() => void refresh()}>Try again</button>
        </div>
      </div>
    );
  }
  if (!snap || !status) return <>{fallback}</>;
  return <Ctx.Provider value={{ snap, status, logs, refresh }}>{children}</Ctx.Provider>;
}

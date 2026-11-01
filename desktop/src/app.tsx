import { createContext, useCallback, useContext, useEffect, useState, type ReactNode } from 'react';
import { api, onLog, onStatus, type Snapshot, type Status } from './api';

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

  const refresh = useCallback(async () => {
    const s = await api.snapshot();
    setSnap(s);
    setStatus(s.status);
  }, []);

  useEffect(() => {
    void refresh();
    void api.logs().then(setLogs);
    const subs = [onStatus(setStatus), onLog((l) => setLogs((p) => (p.length > 3000 ? [...p.slice(-2500), l] : [...p, l])))];
    return () => { subs.forEach((s) => void Promise.resolve(s).then((u) => typeof u === 'function' && u())); };
  }, [refresh]);

  if (!snap || !status) return <>{fallback}</>;
  return <Ctx.Provider value={{ snap, status, logs, refresh }}>{children}</Ctx.Provider>;
}

import { createContext, useCallback, useContext, useEffect, useMemo, useState, type ReactNode } from 'react';
import { PowerDB, PowerDBError } from '@sdk';

export const db = new PowerDB(window.location.origin);
const KEY = 'powerdb.token';

export interface Identity {
  label: string;
  kind: 'user' | 'key';
}

interface Session {
  identity: Identity | null;
  loginWithKey: (key: string) => Promise<void>;
  loginWithPassword: (email: string, password: string) => Promise<void>;
  logout: () => void;
}

const Ctx = createContext<Session | null>(null);
export const useSession = () => {
  const s = useContext(Ctx);
  if (!s) throw new Error('no session');
  return s;
};

/** Checks that whoever signed in can administer the database, and describes them. The token lives in sessionStorage: it ends with the tab. */
async function identify(): Promise<Identity> {
  const me = await db.auth.me();
  if (me.kind === 'user' && me.user) {
    if (me.user.role !== 'admin') throw new PowerDBError(403, 'forbidden', 'This dashboard is for administrators. Ask one to give you an administrator account.');
    return { label: me.user.email, kind: 'user' };
  }
  const key = (me as unknown as { key?: { name: string; admin: boolean } }).key;
  if (!key?.admin) throw new PowerDBError(403, 'forbidden', 'This API key is not an administrator key.');
  return { label: `key: ${key.name}`, kind: 'key' };
}

/** When the dashboard is shown inside the PowerDB desktop app, tell that window how sign-in went (it shows progress or an error). Only its own origins are addressed. */
export const DESKTOP_ORIGINS = ['tauri://localhost', 'http://tauri.localhost', 'https://tauri.localhost'];
/** True when this page is shown inside another page: in practice, the PowerDB desktop app's Data section. */
export const embedded = window.parent !== window;

function notifyDesktop(state: 'signed-in' | 'signed-out'): void {
  if (window.parent === window) return;
  for (const origin of DESKTOP_ORIGINS) {
    try { window.parent.postMessage({ powerdb: 'dashboard', state }, origin); } catch { /* not that origin */ }
  }
}

export function SessionProvider({ children }: { children: ReactNode }) {
  const [identity, setIdentity] = useState<Identity | null>(null);
  const [ready, setReady] = useState(false);

  // resume a session that was open in this tab
  useMemo(() => {
    // The desktop app opens the dashboard as `/#key=pdb_live_…`. A fragment is never sent to a server; it is removed at once.
    const fromApp = /^#key=(pdb_live_[A-Za-z0-9_-]+)$/.exec(window.location.hash)?.[1];
    if (fromApp) window.history.replaceState(null, '', window.location.pathname + window.location.search);
    const t = fromApp ?? sessionStorage.getItem(KEY);
    if (t) {
      db.setToken(t);
      identify().then((who) => { setIdentity(who); sessionStorage.setItem(KEY, t); }, () => { sessionStorage.removeItem(KEY); db.setToken(undefined); }).finally(() => setReady(true));
    } else setReady(true);
  }, []);

  const finish = useCallback(async (token: string) => {
    db.setToken(token);
    try {
      setIdentity(await identify());
      sessionStorage.setItem(KEY, token);
    } catch (e) {
      db.setToken(undefined);
      throw e;
    }
  }, []);

  useEffect(() => { if (ready) notifyDesktop(identity ? 'signed-in' : 'signed-out'); }, [ready, identity]);

  const value: Session = {
    identity,
    loginWithKey: (key) => finish(key.trim()),
    loginWithPassword: async (email, password) => {
      const { token } = await db.auth.login(email, password);
      await finish(token);
    },
    logout: () => { db.setToken(undefined); sessionStorage.removeItem(KEY); setIdentity(null); },
  };
  if (!ready) return null;
  return <Ctx.Provider value={value}>{children}</Ctx.Provider>;
}

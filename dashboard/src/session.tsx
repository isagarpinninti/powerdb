import { createContext, useCallback, useContext, useMemo, useState, type ReactNode } from 'react';
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

export function SessionProvider({ children }: { children: ReactNode }) {
  const [identity, setIdentity] = useState<Identity | null>(null);
  const [ready, setReady] = useState(false);

  // resume a session that was open in this tab
  useMemo(() => {
    const t = sessionStorage.getItem(KEY);
    if (t) {
      db.setToken(t);
      identify().then(setIdentity, () => { sessionStorage.removeItem(KEY); db.setToken(undefined); }).finally(() => setReady(true));
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

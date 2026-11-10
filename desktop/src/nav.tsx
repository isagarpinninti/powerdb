import { createContext, useContext } from 'react';

export type View = 'server' | 'data' | 'backups' | 'logs' | 'settings' | 'about';

/** Lets any screen send the person to another section (for example "Browse data" on the Server page). */
export const NavCtx = createContext<(v: View) => void>(() => {});
export const useNav = () => useContext(NavCtx);

import { invoke as tauriInvoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';

export type Phase = 'stopped' | 'starting' | 'running' | 'stopping' | 'failed';
export interface Status { phase: Phase; pid: number | null; since: number | null; message: string | null }
export interface Config { configured: boolean; data_dir: string; port: number; auto_start_server: boolean }
export interface Snapshot {
  config: Config; status: Status; app_version: string; server_version: string; has_key: boolean;
  default_data_dir: string; platform: string; start_at_login: boolean;
}
export interface Stats { segments: number; log_bytes: number; dead_bytes: number; gc_pending: number }
export interface BackupInfo { name: string; created_at: number; files: number; bytes: number; includes_secret: boolean; powerdb_version: string }

/** Inside the app this is the Tauri bridge. In a plain browser (design work, screenshots) a small in-memory stand-in answers instead. */
const inApp = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
export const invoke: <T>(cmd: string, args?: Record<string, unknown>) => Promise<T> = inApp
  ? (cmd, args) => tauriInvoke(cmd, args)
  : (cmd, args) => import('./mock').then((m) => m.mockInvoke(cmd, args) as never);

export const onStatus = (f: (s: Status) => void) => inApp ? listen<Status>('server-status', (e) => f(e.payload)) : import('./mock').then((m) => m.mockListen('server-status', f as never));
export const onGoto = (f: (view: string) => void) => inApp ? listen<string>('goto', (e) => f(e.payload)) : Promise.resolve(() => {});
export const onLog = (f: (l: string) => void) => inApp ? listen<string>('server-log', (e) => f(e.payload)) : import('./mock').then((m) => m.mockListen('server-log', f as never));

export const api = {
  snapshot: () => invoke<Snapshot>('snapshot'),
  setupNew: (dataDir: string, port: number, startAtLogin: boolean) => invoke<string>('setup_new', { dataDir, port, startAtLogin }),
  setupExisting: (dataDir: string, port: number, key: string, startAtLogin: boolean) => invoke<void>('setup_existing', { dataDir, port, key, startAtLogin }),
  start: () => invoke<void>('start_server'),
  stop: () => invoke<void>('stop_server'),
  restart: () => invoke<void>('restart_server'),
  saveSettings: (port: number, autoStartServer: boolean, startAtLogin: boolean) => invoke<void>('save_settings', { port, autoStartServer, startAtLogin }),
  logs: () => invoke<string[]>('logs'),
  dashboardUrl: () => invoke<string>('dashboard_url'),
  stats: async () => JSON.parse(await invoke<string>('server_stats')) as Stats,
  backups: async () => (JSON.parse(await invoke<string>('list_backups')) as { backups: BackupInfo[] }).backups,
  backup: async (deep: boolean) => JSON.parse(await invoke<string>('create_backup', { deep })) as { name: string; files: number; bytes: number },
  check: (quick: boolean) => invoke<string>('run_check', { quick }),
  restore: (name: string, target: string) => invoke<string>('restore_backup', { name, target }),
  useDatabase: (dataDir: string) => invoke<void>('use_database', { dataDir }),
  revealKey: () => invoke<string>('reveal_key'),
};

export const errorText = (e: unknown) => (e instanceof Error ? e.message : typeof e === 'string' ? e : JSON.stringify(e));

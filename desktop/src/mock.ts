// Stands in for the native app when the interface is opened in a plain browser: `npm run dev`, then
// http://localhost:1420/?scenario=setup | running | stopped | failed. Never used inside the real app.
import type { BackupInfo, Snapshot, Stats, Status } from './api';

const scenario = new URLSearchParams(location.search).get('scenario') ?? 'running';
const listeners = new Map<string, Set<(v: unknown) => void>>();
const emit = (k: string, v: unknown) => listeners.get(k)?.forEach((f) => f(v));
export const mockListen = (k: string, f: (v: unknown) => void) => {
  if (!listeners.has(k)) listeners.set(k, new Set());
  listeners.get(k)!.add(f);
  return () => listeners.get(k)!.delete(f);
};

let status: Status = scenario === 'stopped' ? { phase: 'stopped', pid: null, since: null, message: null }
  : scenario === 'failed' ? { phase: 'failed', pid: null, since: null, message: 'the server exited unexpectedly (exit status: 1). powerdb: conflict: /Users/me/Library/Application Support/PowerDB/data is in use by another PowerDB process; stop it first' }
  : { phase: 'running', pid: 4242, since: Date.now() - 2 * 3600_000 - 14 * 60_000, message: null };
const q = new URLSearchParams(location.search);
const config = { configured: scenario !== 'setup', data_dir: '/Users/me/Library/Application Support/PowerDB/data', port: Number(q.get('port')) || 8090, auto_start_server: true };
const logs = [
  '2026-11-01T09:12:03Z  INFO powerdb: PowerDB 0.3.1 listening on http://127.0.0.1:8090 (data: /Users/me/Library/Application Support/PowerDB/data)',
  '2026-11-01T09:12:03Z  INFO powerdb::server: maintenance thread started',
  '2026-11-01T10:40:11Z  WARN powerdb::api: login throttled for 127.0.0.1',
  '2026-11-01T11:02:47Z  INFO powerdb::storage::gc: released 3 files, reclaimed 12.4 MB',
];
const backups: BackupInfo[] = [
  { name: 'backup-20261101-070000', created_at: Date.now() - 4 * 3600_000, files: 31, bytes: 48_201_332, includes_secret: false, powerdb_version: '0.3.1' },
  { name: 'nightly', created_at: Date.now() - 28 * 3600_000, files: 29, bytes: 47_900_100, includes_secret: false, powerdb_version: '0.3.1' },
];
const set = (s: Partial<Status>) => { status = { ...status, ...s }; emit('server-status', status); };

export async function mockInvoke(cmd: string, args: Record<string, unknown> = {}): Promise<unknown> {
  await new Promise((r) => setTimeout(r, 120));
  switch (cmd) {
    case 'snapshot':
      return { config, status, app_version: '0.3.1', server_version: 'powerdb 0.3.1', has_key: true, default_data_dir: config.data_dir, platform: 'macos', start_at_login: true } satisfies Snapshot;
    case 'setup_new': config.configured = true; return 'pdb_live_Zx3Qk9v2mR7sT1nLhY4cWdPaB8eUfJgO5iKqNtVyXAs';
    case 'start_server': set({ phase: 'starting' }); setTimeout(() => set({ phase: 'running', pid: 4243, since: Date.now() }), 900); return;
    case 'stop_server': set({ phase: 'stopping' }); setTimeout(() => set({ phase: 'stopped', pid: null, since: null }), 700); return;
    case 'restart_server': set({ phase: 'stopping' }); setTimeout(() => set({ phase: 'starting' }), 500); setTimeout(() => set({ phase: 'running', since: Date.now() }), 1400); return;
    case 'logs': return logs;
    case 'server_stats': return JSON.stringify({ segments: 3, log_bytes: 184_320_000, dead_bytes: 12_400_000, gc_pending: 2 } satisfies Stats);
    case 'list_backups': return JSON.stringify({ backups });
    case 'create_backup': { const b = { name: `backup-${Date.now()}`, files: 31, bytes: 48_300_000 }; backups.unshift({ ...b, created_at: Date.now(), includes_secret: false, powerdb_version: '0.3.1' }); return JSON.stringify(b); }
    case 'run_check': return '3 records, 3 files, 41 chunks, 183.2 MiB of chunk data re-read and verified\nNo problems found.';
    case 'restore_backup': return `Restored 31 files into ${String(args.target)} and the integrity check passed.`;
    case 'dashboard_url': return q.get('dash') ?? `http://127.0.0.1:${config.port}/#key=pdb_live_mock`;
    case 'reveal_key': return 'pdb_live_Zx3Qk9v2mR7sT1nLhY4cWdPaB8eUfJgO5iKqNtVyXAs';
    default: return undefined;
  }
}

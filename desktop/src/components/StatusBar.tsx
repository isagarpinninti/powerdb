import { useEffect, useState } from 'react';
import { CopyButton } from '@dash/ui';
import { useApp } from '../app';
import { Dot, phaseLabel, uptime } from '../common';

/** Always-visible facts about the server: the one place to look to know what state things are in. */
export function StatusBar() {
  const { snap, status } = useApp();
  const [, tick] = useState(0);
  useEffect(() => { const t = setInterval(() => tick((n) => n + 1), 30_000); return () => clearInterval(t); }, []);
  const addr = `127.0.0.1:${snap.config.port}`;
  return (
    <footer className="flex h-7 shrink-0 items-center gap-4 border-t border-line bg-surface px-3 text-xs text-muted" role="contentinfo" aria-label="Server status">
      <span className="flex items-center gap-1.5 text-ink"><Dot phase={status.phase} size={8} />{phaseLabel[status.phase]}</span>
      <span className="flex items-center gap-1 font-mono"><span>{addr}</span><CopyButton text={`http://${addr}`} label="Copy" /></span>
      {status.phase === 'running' && status.since && <span>up {uptime(status.since)}</span>}
      <span className="min-w-0 flex-1 truncate" title={snap.config.data_dir}>{snap.config.data_dir}</span>
      <span>{snap.server_version.replace('powerdb ', 'server ')}</span>
      <span>app {snap.app_version}</span>
    </footer>
  );
}

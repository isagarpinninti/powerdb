import { useCallback, useEffect, useRef, useState } from 'react';
import { readTheme } from '@dash/theme';
import { openUrl } from '@tauri-apps/plugin-opener';
import { Button, Spinner } from '@dash/ui';
import { api, errorText } from '../api';
import { useApp } from '../app';
import { logUi } from '../boot';

type Phase = 'loading' | 'ready' | 'signed-out' | 'error';

/** The PowerDB dashboard (collections, records, files, users, API keys) inside this window, already signed in. */
export function Data() {
  const { snap, status } = useApp();
  const running = status.phase === 'running';
  const origin = `http://127.0.0.1:${snap.config.port}`;
  const [src, setSrc] = useState<string | null>(null);
  const [phase, setPhase] = useState<Phase>('loading');
  const [error, setError] = useState<string | null>(null);
  const frame = useRef<HTMLIFrameElement>(null);
  // the dashboard follows this window's theme (it has its own storage, so it has to be told)
  const sendTheme = useCallback(() => { frame.current?.contentWindow?.postMessage({ powerdb: 'theme', theme: readTheme() }, origin); }, [origin]);
  useEffect(() => { window.addEventListener('powerdb-theme', sendTheme); return () => window.removeEventListener('powerdb-theme', sendTheme); }, [sendTheme]);

  useEffect(() => {
    setSrc(null); setError(null); setPhase('loading');
    if (!running) return;
    let live = true;
    api.dashboardUrl().then((u) => { if (live) setSrc(u); }, (e) => { if (live) { setError(errorText(e)); setPhase('error'); } });
    return () => { live = false; };
  }, [running, snap.config.port]);

  // the dashboard says when it has signed in (it only ever talks to this window, and only from the server's own address)
  useEffect(() => {
    const onMessage = (e: MessageEvent) => {
      if (e.origin !== origin || !e.data || e.data.powerdb !== 'dashboard') return;
      logUi('info', `dashboard: ${e.data.state}`);
      setPhase(e.data.state === 'signed-in' ? 'ready' : 'signed-out');
      if (e.data.state === 'signed-in') sendTheme();
    };
    window.addEventListener('message', onMessage);
    return () => window.removeEventListener('message', onMessage);
  }, [origin, sendTheme]);

  useEffect(() => {
    if (!src || phase !== 'loading') return;
    const t = setTimeout(() => { setPhase((p) => (p === 'loading' ? 'error' : p)); setError((m) => m ?? 'The dashboard did not answer within 15 seconds.'); }, 15000);
    return () => clearTimeout(t);
  }, [src, phase]);

  if (!running) {
    return (
      <div className="flex h-full flex-col items-center justify-center gap-3 p-8 text-center">
        <div className="text-lg font-semibold">The server is not running</div>
        <p className="max-w-md text-sm text-muted">Your data is browsed through the server. Start it to see your collections, records and files.</p>
        <Button variant="primary" onClick={() => void api.start()}>Start Server</Button>
      </div>
    );
  }

  return (
    <div className="relative h-full w-full">
      {src && <iframe ref={frame} title="PowerDB dashboard" src={src} className="h-full w-full border-0 bg-bg" onLoad={() => { logUi('info', 'dashboard frame loaded'); sendTheme(); }} />}
      {phase !== 'ready' && (
        <div className="absolute inset-0 flex flex-col items-center justify-center gap-3 bg-bg p-8 text-center" role={phase === 'loading' ? 'status' : 'alert'}>
          {phase === 'loading' && <div className="flex items-center gap-3 text-sm text-muted"><Spinner /> Opening your data…</div>}
          {phase === 'signed-out' && (<>
            <div className="text-lg font-semibold">The dashboard did not accept the saved administrator key</div>
            <p className="max-w-md text-sm text-muted">The key kept by this app may be out of date. You can still sign in yourself in your browser.</p>
          </>)}
          {phase === 'error' && (<>
            <div className="text-lg font-semibold">Could not open your data</div>
            <p className="max-w-lg whitespace-pre-wrap text-sm text-danger">{error}</p>
          </>)}
          {phase !== 'loading' && <Button onClick={() => openUrl(origin).catch(() => {})}>Open in your browser</Button>}
        </div>
      )}
    </div>
  );
}

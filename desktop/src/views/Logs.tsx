import { useEffect, useMemo, useRef, useState } from 'react';
import { Button, Card, CopyButton, Input, cn } from '@dash/ui';
import { useApp } from '../app';
import { Page, logLevel } from '../common';
import { Icon } from '../icons';

type Level = 'all' | 'info' | 'warn' | 'error';
const colour = { error: 'text-danger', warn: 'text-warn', info: 'text-muted', debug: 'text-faint' } as const;

/** Splits `2026-11-01T09:12:03Z  INFO powerdb::api: message` into its parts, so the time and level can be styled apart. */
function parts(line: string) {
  const m = /^(\S+)\s+(INFO|WARN|ERROR|DEBUG|TRACE)\s+(.*)$/.exec(line);
  return m ? { time: m[1].replace(/^\d{4}-\d{2}-\d{2}T/, '').replace(/\.\d+Z?$|Z$/, ''), text: m[3] } : { time: '', text: line };
}

export function Logs() {
  const { logs } = useApp();
  const [filter, setFilter] = useState('');
  const [level, setLevel] = useState<Level>('all');
  const [follow, setFollow] = useState(true);
  const [wrap, setWrap] = useState(true);
  const [cleared, setCleared] = useState(0);
  const box = useRef<HTMLDivElement>(null);
  const visible = useMemo(() => logs.slice(cleared), [logs, cleared]);
  const counts = useMemo(() => ({ info: visible.filter((l) => ['info', 'debug'].includes(logLevel(l))).length, warn: visible.filter((l) => logLevel(l) === 'warn').length, error: visible.filter((l) => logLevel(l) === 'error').length }), [visible]);
  const shown = useMemo(() => {
    const f = filter.trim().toLowerCase();
    return visible.filter((l) => (level === 'all' || (level === 'info' ? ['info', 'debug'].includes(logLevel(l)) : logLevel(l) === level)) && (!f || l.toLowerCase().includes(f)));
  }, [visible, filter, level]);
  useEffect(() => { if (follow && box.current) box.current.scrollTop = box.current.scrollHeight; }, [shown, follow]);

  const chips: [Level, string, number | null][] = [['all', 'All', visible.length], ['info', 'Info', counts.info], ['warn', 'Warnings', counts.warn], ['error', 'Errors', counts.error]];
  return (
    <Page title="Logs" subtitle="What the server prints while it runs. The latest 3,000 lines are kept."
      actions={<><CopyButton text={shown.join('\n')} label="Copy shown" /><Button small onClick={() => setCleared(logs.length)}>Clear view</Button></>}>
      <div className="flex flex-wrap items-center gap-3">
        <div className="relative"><Icon name="search" size={14} className="pointer-events-none absolute left-2.5 top-1/2 -translate-y-1/2 text-faint" /><Input placeholder="Filter lines…" value={filter} onChange={(e) => setFilter(e.target.value)} className="w-64 !pl-8" aria-label="Filter log lines" /></div>
        <div className="flex rounded-lg border border-line bg-raised p-0.5" role="group" aria-label="Level">
          {chips.map(([id, label, n]) => (
            <button key={id} onClick={() => setLevel(id)} aria-pressed={level === id} className={cn('rounded-md px-2.5 py-1 text-xs font-medium transition-colors', level === id ? 'bg-surface text-ink shadow-sm' : 'text-muted hover:text-ink')}>
              {label}{n !== null && <span className={cn('ml-1.5', id === 'error' && n ? 'text-danger' : id === 'warn' && n ? 'text-warn' : 'text-faint')}>{n}</span>}
            </button>))}
        </div>
        <label className="flex items-center gap-2 text-xs text-muted"><input type="checkbox" checked={follow} onChange={(e) => setFollow(e.target.checked)} /> Follow</label>
        <label className="flex items-center gap-2 text-xs text-muted"><input type="checkbox" checked={wrap} onChange={(e) => setWrap(e.target.checked)} /> Wrap lines</label>
        <span className="ml-auto text-xs text-faint">{shown.length} of {visible.length} lines</span>
      </div>
      <Card className="overflow-hidden">
        <div ref={box} className="h-[calc(100vh-330px)] min-h-64 overflow-auto bg-bg p-3 font-mono text-xs leading-5" role="log" aria-live="off" tabIndex={0}>
          {shown.length === 0 ? (
            <div className="flex h-full flex-col items-center justify-center gap-2 text-center text-muted"><Icon name="logs" size={28} className="text-faint" />{visible.length ? 'No lines match these filters.' : 'Nothing logged yet. Start the server to see its output.'}</div>
          ) : shown.map((l, i) => {
            const p = parts(l);
            return (
              <div key={i} className={cn('flex gap-3 hover:bg-raised/50', wrap ? '' : 'whitespace-nowrap')}>
                <span className="w-16 shrink-0 text-faint">{p.time}</span>
                <span className={cn('w-12 shrink-0 font-semibold uppercase', colour[logLevel(l)])}>{logLevel(l)}</span>
                <span className={cn('min-w-0 flex-1', wrap ? 'whitespace-pre-wrap break-all' : '', logLevel(l) === 'error' ? 'text-danger' : logLevel(l) === 'warn' ? 'text-warn' : 'text-muted')}>{p.text}</span>
              </div>);
          })}
        </div>
      </Card>
    </Page>
  );
}

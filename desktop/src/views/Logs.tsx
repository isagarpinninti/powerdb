import { useEffect, useMemo, useRef, useState } from 'react';
import { Button, Card, CopyButton, Input, cn } from '@dash/ui';
import { useApp } from '../app';
import { Page } from '../common';

const level = (l: string) => (/\bERROR\b/.test(l) ? 'text-danger' : /\bWARN\b/.test(l) ? 'text-warn' : /\bDEBUG|TRACE\b/.test(l) ? 'text-faint' : 'text-ink');

export function Logs() {
  const { logs } = useApp();
  const [filter, setFilter] = useState('');
  const [follow, setFollow] = useState(true);
  const [cleared, setCleared] = useState(0);
  const box = useRef<HTMLDivElement>(null);
  const shown = useMemo(() => {
    const f = filter.trim().toLowerCase();
    return logs.slice(cleared).filter((l) => !f || l.toLowerCase().includes(f));
  }, [logs, filter, cleared]);

  useEffect(() => { if (follow && box.current) box.current.scrollTop = box.current.scrollHeight; }, [shown, follow]);

  return (
    <Page title="Logs" subtitle="What the server prints while it runs. The last 3,000 lines are kept."
      actions={<><CopyButton text={shown.join('\n')} label="Copy all" /><Button small onClick={() => setCleared(logs.length)}>Clear view</Button></>}>
      <div className="flex items-center gap-3">
        <Input placeholder="Filter…" value={filter} onChange={(e) => setFilter(e.target.value)} className="max-w-xs" aria-label="Filter log lines" />
        <label className="flex items-center gap-2 text-sm text-muted"><input type="checkbox" checked={follow} onChange={(e) => setFollow(e.target.checked)} /> Follow new lines</label>
        <span className="ml-auto text-xs text-faint">{shown.length} line{shown.length === 1 ? '' : 's'}</span>
      </div>
      <Card className="overflow-hidden">
        <div ref={box} className="h-[calc(100vh-290px)] min-h-64 overflow-auto bg-bg p-3 font-mono text-xs leading-5" role="log" aria-live="off">
          {shown.length === 0 ? <div className="p-6 text-center text-muted">{logs.length ? 'No lines match.' : 'Nothing logged yet. Start the server to see its output.'}</div>
            : shown.map((l, i) => <div key={i} className={cn('whitespace-pre-wrap break-all', level(l))}>{l}</div>)}
        </div>
      </Card>
    </Page>
  );
}

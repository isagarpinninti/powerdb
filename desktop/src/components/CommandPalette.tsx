import { useEffect, useMemo, useRef, useState } from 'react';
import { cn } from '@dash/ui';
import { Icon, type IconName } from '../icons';

export interface Command {
  id: string;
  label: string;
  hint?: string;
  icon: IconName;
  group: 'Go to' | 'Server' | 'Actions';
  keywords?: string;
  shortcut?: string;
  run: () => void;
}

/** ⌘K / Ctrl+K: jump anywhere or run an action from the keyboard. */
export function CommandPalette({ commands, onClose }: { commands: Command[]; onClose: () => void }) {
  const [q, setQ] = useState('');
  const [active, setActive] = useState(0);
  const input = useRef<HTMLInputElement>(null);
  const list = useRef<HTMLUListElement>(null);

  const shown = useMemo(() => {
    const terms = q.toLowerCase().split(/\s+/).filter(Boolean);
    return commands.filter((c) => terms.every((t) => `${c.label} ${c.group} ${c.keywords ?? ''}`.toLowerCase().includes(t)));
  }, [commands, q]);

  useEffect(() => { input.current?.focus(); }, []);
  useEffect(() => { setActive(0); }, [q]);
  useEffect(() => { list.current?.querySelector('[aria-selected="true"]')?.scrollIntoView({ block: 'nearest' }); }, [active]);

  const run = (c: Command | undefined) => { if (!c) return; onClose(); c.run(); };
  const onKey = (e: React.KeyboardEvent) => {
    if (e.key === 'Escape') { e.preventDefault(); onClose(); }
    else if (e.key === 'ArrowDown') { e.preventDefault(); setActive((i) => Math.min(shown.length - 1, i + 1)); }
    else if (e.key === 'ArrowUp') { e.preventDefault(); setActive((i) => Math.max(0, i - 1)); }
    else if (e.key === 'Enter') { e.preventDefault(); run(shown[active]); }
  };

  let lastGroup = '';
  return (
    <div className="fixed inset-0 z-50 flex items-start justify-center bg-black/50 p-4 pt-[14vh]" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div role="dialog" aria-modal="true" aria-label="Command palette" className="w-full max-w-xl overflow-hidden rounded-xl border border-line-strong bg-surface shadow-2xl" onKeyDown={onKey}>
        <div className="flex items-center gap-3 border-b border-line px-4">
          <Icon name="search" className="shrink-0 text-muted" />
          <input ref={input} value={q} onChange={(e) => setQ(e.target.value)} placeholder="Type a command or the name of a section…" aria-label="Command" role="combobox" aria-expanded="true" aria-controls="palette-list" aria-activedescendant={shown[active] ? `cmd-${shown[active].id}` : undefined}
            className="h-12 flex-1 bg-transparent text-sm text-ink placeholder:text-faint focus:outline-none" />
          <kbd className="rounded border border-line-strong px-1.5 py-0.5 text-[11px] text-muted">esc</kbd>
        </div>
        <ul id="palette-list" ref={list} role="listbox" className="max-h-80 overflow-auto p-2">
          {shown.length === 0 && <li className="px-3 py-8 text-center text-sm text-muted">Nothing matches “{q}”.</li>}
          {shown.map((c, i) => {
            const header = c.group !== lastGroup ? (lastGroup = c.group) : null;
            return (
              <li key={c.id} role="presentation">
                {header && <div className="px-3 pb-1 pt-2 text-[11px] font-semibold uppercase tracking-wide text-faint">{header}</div>}
                <div id={`cmd-${c.id}`} role="option" aria-selected={i === active} onMouseEnter={() => setActive(i)} onClick={() => run(c)}
                  className={cn('flex cursor-pointer items-center gap-3 rounded-lg px-3 py-2 text-sm', i === active ? 'bg-raised text-ink' : 'text-muted')}>
                  <Icon name={c.icon} size={16} />
                  <span className="flex-1 text-ink">{c.label}</span>
                  {c.hint && <span className="text-xs text-faint">{c.hint}</span>}
                  {c.shortcut && <kbd className="rounded border border-line-strong px-1.5 py-0.5 text-[11px] text-muted">{c.shortcut}</kbd>}
                </div>
              </li>
            );
          })}
        </ul>
      </div>
    </div>
  );
}

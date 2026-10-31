import { createContext, useCallback, useContext, useEffect, useRef, useState, type ButtonHTMLAttributes, type InputHTMLAttributes, type ReactNode, type SelectHTMLAttributes, type TextareaHTMLAttributes } from 'react';

export const cn = (...c: Array<string | false | null | undefined>) => c.filter(Boolean).join(' ');

const inputBase = 'rounded-md border border-line-strong bg-bg px-3 py-2 text-sm text-ink placeholder:text-faint focus:border-ink focus:outline-none disabled:opacity-50';

// full width unless the caller sets a width of its own
const full = (c?: string) => (c && /(^|\s)(max-)?w-/.test(c) ? '' : 'w-full');
export function Input(p: InputHTMLAttributes<HTMLInputElement>) {
  return <input {...p} className={cn(inputBase, full(p.className), p.className)} />;
}
export function Textarea(p: TextareaHTMLAttributes<HTMLTextAreaElement>) {
  return <textarea {...p} className={cn(inputBase, full(p.className), 'font-mono', p.className)} />;
}
export function Select(p: SelectHTMLAttributes<HTMLSelectElement>) {
  return <select {...p} className={cn(inputBase, full(p.className), 'pr-8', p.className)} />;
}

type BtnProps = ButtonHTMLAttributes<HTMLButtonElement> & { variant?: 'primary' | 'ghost' | 'danger' | 'outline'; small?: boolean };
export function Button({ variant = 'outline', small, className, ...p }: BtnProps) {
  const v = {
    primary: 'bg-ink text-bg font-medium hover:opacity-90 disabled:hover:opacity-50',
    outline: 'border border-line-strong bg-raised text-ink hover:bg-line',
    ghost: 'text-muted hover:bg-raised hover:text-ink',
    danger: 'border border-danger/40 bg-surface text-danger hover:bg-danger/10',
  }[variant];
  return <button {...p} className={cn('inline-flex items-center justify-center gap-1.5 rounded-md font-medium transition-colors disabled:cursor-not-allowed disabled:opacity-50', small ? 'px-2.5 py-1 text-xs' : 'px-3.5 py-2 text-sm', v, className)} />;
}

export function Field({ label, hint, children }: { label: string; hint?: ReactNode; children: ReactNode }) {
  return (
    <label className="block">
      <span className="mb-1 block text-xs font-semibold uppercase tracking-wide text-muted">{label}</span>
      {children}
      {hint && <span className="mt-1 block text-xs text-muted">{hint}</span>}
    </label>
  );
}

export function Badge({ children, tone = 'slate' }: { children: ReactNode; tone?: 'slate' | 'gold' | 'green' | 'red' }) {
  const t = {
    slate: 'bg-raised text-muted border border-line',
    gold: 'bg-live/15 text-live',
    green: 'bg-ok/15 text-ok',
    red: 'bg-danger/15 text-danger',
  }[tone];
  return <span className={cn('inline-flex items-center rounded px-1.5 py-0.5 text-[11px] font-medium', t)}>{children}</span>;
}

export function Card({ children, className }: { children: ReactNode; className?: string }) {
  return <div className={cn('rounded-lg border border-line bg-surface shadow-sm', className)}>{children}</div>;
}

export function Modal({ title, onClose, children, wide }: { title: string; onClose: () => void; children: ReactNode; wide?: boolean }) {
  useEffect(() => {
    const k = (e: KeyboardEvent) => e.key === 'Escape' && onClose();
    window.addEventListener('keydown', k);
    return () => window.removeEventListener('keydown', k);
  }, [onClose]);
  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-4" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div role="dialog" aria-modal="true" aria-label={title} className={cn('max-h-[90vh] w-full overflow-auto rounded-xl bg-surface p-6 shadow-2xl', wide ? 'max-w-3xl' : 'max-w-lg')}>
        <div className="mb-4 flex items-center justify-between">
          <h2 className="text-lg font-semibold">{title}</h2>
          <Button variant="ghost" small onClick={onClose} aria-label="Close">✕</Button>
        </div>
        {children}
      </div>
    </div>
  );
}

export function ErrorBox({ error }: { error: unknown }) {
  if (!error) return null;
  const msg = error instanceof Error ? error.message : String(error);
  return <div role="alert" className="rounded-md border border-danger/40 bg-danger/10 px-3 py-2 text-sm text-danger">{msg}</div>;
}

// ---- toasts ----
type Toast = { id: number; text: string; tone: 'ok' | 'err' };
const ToastCtx = createContext<(text: string, tone?: 'ok' | 'err') => void>(() => {});
export const useToast = () => useContext(ToastCtx);
export function ToastProvider({ children }: { children: ReactNode }) {
  const [items, setItems] = useState<Toast[]>([]);
  const n = useRef(0);
  const push = useCallback((text: string, tone: 'ok' | 'err' = 'ok') => {
    const id = ++n.current;
    setItems((x) => [...x, { id, text, tone }]);
    setTimeout(() => setItems((x) => x.filter((t) => t.id !== id)), 4000);
  }, []);
  return (
    <ToastCtx.Provider value={push}>
      {children}
      <div className="pointer-events-none fixed bottom-4 right-4 z-[60] flex flex-col gap-2" aria-live="polite">
        {items.map((t) => (
          <div key={t.id} className={cn('pointer-events-auto rounded-md px-4 py-2 text-sm shadow-lg', t.tone === 'ok' ? 'bg-ink text-bg' : 'bg-danger text-bg')}>{t.text}</div>
        ))}
      </div>
    </ToastCtx.Provider>
  );
}

export function Spinner() {
  return <span className="inline-block h-4 w-4 animate-spin rounded-full border-2 border-line-strong border-t-ink" role="status" aria-label="Loading" />;
}

export function CopyButton({ text, label = 'Copy' }: { text: string; label?: string }) {
  const toast = useToast();
  return (
    <Button small variant="outline" onClick={() => navigator.clipboard.writeText(text).then(() => toast('Copied'), () => toast('Could not copy', 'err'))}>
      {label}
    </Button>
  );
}

export const fmtBytes = (n: number) => (n < 1024 ? `${n} B` : n < 1048576 ? `${(n / 1024).toFixed(1)} KB` : n < 1073741824 ? `${(n / 1048576).toFixed(1)} MB` : `${(n / 1073741824).toFixed(2)} GB`);
export const fmtDate = (ms: number) => new Date(ms).toLocaleString();

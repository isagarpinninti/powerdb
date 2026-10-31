import type { FieldDef } from '@sdk';
import { Button, Input, Select, cn, fmtBytes } from '../ui';

export const NAME_RE = /^[a-z][a-z0-9_]{0,62}$/;
const RESERVED = ['id', 'collection', 'created_at', 'updated_at'];
const KINDS: FieldDef['kind'][] = ['text', 'number', 'bool', 'json', 'relation', 'file'];

export function fieldProblems(fields: FieldDef[]): string[] {
  const out: string[] = [];
  const seen = new Set<string>();
  fields.forEach((f, i) => {
    if (!NAME_RE.test(f.name)) out.push(`Field ${i + 1}: names are lowercase letters, digits and underscores, starting with a letter.`);
    else if (RESERVED.includes(f.name)) out.push(`Field "${f.name}" is a reserved name.`);
    else if (seen.has(f.name)) out.push(`Field "${f.name}" appears twice.`);
    seen.add(f.name);
  });
  return out;
}

const MIME_PRESETS: Array<[string, string[]]> = [
  ['Any type', []],
  ['Images', ['image/*']],
  ['PDF documents', ['application/pdf']],
  ['Images and PDF', ['image/*', 'application/pdf']],
  ['Video', ['video/*']],
  ['Audio', ['audio/*']],
];

export function FieldsEditor({ fields, onChange }: { fields: FieldDef[]; onChange: (f: FieldDef[]) => void }) {
  const set = (i: number, patch: Partial<FieldDef>) => onChange(fields.map((f, j) => (j === i ? { ...f, ...patch } : f)));
  const mimeKey = (f: FieldDef) => JSON.stringify(f.mime ?? []);
  const preset = (f: FieldDef) => MIME_PRESETS.find(([, m]) => JSON.stringify(m) === mimeKey(f))?.[0] ?? 'Custom';
  return (
    <div className="space-y-3">
      {fields.length === 0 && <p className="rounded-md border border-dashed border-line-strong p-4 text-center text-sm text-muted">No fields yet. Add the first one below.</p>}
      {fields.map((f, i) => {
        const bad = !NAME_RE.test(f.name) || RESERVED.includes(f.name);
        return (
          <div key={i} className="rounded-lg border border-line bg-surface p-3">
            <div className="flex flex-wrap items-center gap-2">
              <Input className={cn('w-44 font-mono', bad && 'border-danger')} value={f.name} onChange={(e) => set(i, { name: e.target.value })} placeholder="field_name" aria-label={`Field ${i + 1} name`} spellCheck={false} />
              <Select className="w-32" value={f.kind} onChange={(e) => set(i, { kind: e.target.value as FieldDef['kind'], ...(e.target.value === 'file' ? {} : { mime: undefined }) })} aria-label={`Field ${i + 1} type`}>
                {KINDS.map((k) => <option key={k} value={k}>{k}</option>)}
              </Select>
              <label className="flex items-center gap-1.5 text-sm"><input type="checkbox" className="h-4 w-4 accent-ink" checked={!!f.required} onChange={(e) => set(i, { required: e.target.checked })} /> required</label>
              <div className="ml-auto flex gap-1">
                <Button small variant="ghost" disabled={i === 0} onClick={() => onChange(move(fields, i, i - 1))} aria-label="Move up">↑</Button>
                <Button small variant="ghost" disabled={i === fields.length - 1} onClick={() => onChange(move(fields, i, i + 1))} aria-label="Move down">↓</Button>
                <Button small variant="danger" onClick={() => onChange(fields.filter((_, j) => j !== i))} aria-label={`Remove ${f.name || 'field'}`}>Remove</Button>
              </div>
            </div>
            {(f.kind === 'text' || f.kind === 'file' || f.kind === 'relation') && (
              <div className="mt-3 grid gap-3 sm:grid-cols-2">
                {f.kind !== 'relation' && (
                  <label className="block text-xs text-muted">
                    {f.kind === 'file' ? 'Largest file (MB)' : 'Longest text (bytes)'}
                    <Input type="number" min={0} className="mt-1" value={f.max === undefined ? '' : f.kind === 'file' ? Math.round(f.max / 1048576) : f.max} onChange={(e) => set(i, { max: e.target.value === '' ? undefined : f.kind === 'file' ? Number(e.target.value) * 1048576 : Number(e.target.value) })} placeholder="no limit" />
                    {f.kind === 'file' && f.max ? <span className="mt-0.5 block">{fmtBytes(f.max)}</span> : null}
                  </label>
                )}
                {f.kind === 'file' && (
                  <label className="block text-xs text-muted">
                    Allowed file types
                    <Select className="mt-1" value={preset(f)} onChange={(e) => { const p = MIME_PRESETS.find(([n]) => n === e.target.value); if (p) set(i, { mime: p[1] }); }}>
                      {MIME_PRESETS.map(([n]) => <option key={n}>{n}</option>)}
                      {preset(f) === 'Custom' && <option>Custom</option>}
                    </Select>
                    <span className="mt-0.5 block">Checked against the file's actual bytes, not its name{f.mime?.length ? `: ${f.mime.join(', ')}` : ''}.</span>
                  </label>
                )}
                {f.kind === 'relation' && (
                  <label className="block text-xs text-muted">
                    Points to collection
                    <Input className="mt-1 font-mono" value={f.relation ?? ''} onChange={(e) => set(i, { relation: e.target.value || undefined })} placeholder="collection_name" />
                  </label>
                )}
              </div>
            )}
          </div>
        );
      })}
      <Button onClick={() => onChange([...fields, { name: '', kind: 'text' }])}>+ Add field</Button>
    </div>
  );
}

function move<T>(a: T[], from: number, to: number): T[] {
  const b = [...a];
  const [x] = b.splice(from, 1);
  b.splice(to, 0, x as T);
  return b;
}

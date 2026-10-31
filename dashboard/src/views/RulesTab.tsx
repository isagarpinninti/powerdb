import { useState } from 'react';
import type { CollectionDef } from '@sdk';
import { db } from '../session';
import { Badge, Button, Card, ErrorBox, Select, Textarea, cn, useToast } from '../ui';

type Action = 'list' | 'view' | 'create' | 'update' | 'delete';
type Rule = string | null; // null: administrators only. "": public. Otherwise an expression.

const ACTIONS: Array<{ key: Action; title: string; what: string; vars: string }> = [
  { key: 'list', title: 'List', what: 'See the collection as a list', vars: 'Evaluated for each record; fields are written by name (status, owner_id).' },
  { key: 'view', title: 'View', what: 'Open one record and get its file links', vars: 'Fields by name, e.g. owner_id.' },
  { key: 'create', title: 'Create', what: 'Add a record', vars: 'There is no record yet: read the incoming data as @request.body.field.' },
  { key: 'update', title: 'Update', what: 'Change a record or replace its files', vars: 'The stored record by name; the incoming change as @request.body.field.' },
  { key: 'delete', title: 'Delete', what: 'Remove a record and its files', vars: 'Fields of the stored record by name.' },
];

const SIGNED = '@request.auth.id != ""';

function ownerRule(a: Action, f: string): string {
  if (a === 'create') return `${SIGNED} && @request.body.${f} == @request.auth.id`;
  if (a === 'update') return `${SIGNED} && ${f} == @request.auth.id && (@request.body.${f} == "" || @request.body.${f} == @request.auth.id)`;
  return `${SIGNED} && ${f} == @request.auth.id`;
}

function preset(a: Action, rule: Rule, owner: string | null): string {
  if (rule === null) return 'locked';
  if (rule === '') return 'public';
  if (rule === SIGNED) return 'signed';
  if (owner && rule === ownerRule(a, owner)) return 'owner';
  return 'custom';
}

function describe(a: Action, rule: Rule, owner: string | null): string {
  const verb = { list: 'list records', view: 'open a record', create: 'create records', update: 'change records', delete: 'delete records' }[a];
  switch (preset(a, rule, owner)) {
    case 'locked': return `Only administrators can ${verb}.`;
    case 'public': return `Anyone, signed in or not, can ${verb}.`;
    case 'signed': return `Anyone who is signed in can ${verb}.`;
    case 'owner': return a === 'create' ? `A signed-in user can create a record only if they set ${owner} to their own id, so nobody can create records in someone else's name.` : `A signed-in user can ${verb} only where ${owner} is their own id.`;
    default: return `Custom rule: allowed when the expression is true.`;
  }
}

export function RulesTab({ def, onSaved }: { def: CollectionDef; onSaved: (d: CollectionDef) => void }) {
  const initial = (): Record<Action, Rule> => ({ list: def.rules?.list ?? null, view: def.rules?.view ?? null, create: def.rules?.create ?? null, update: def.rules?.update ?? null, delete: def.rules?.delete ?? null });
  const [rules, setRules] = useState<Record<Action, Rule>>(initial);
  const ownerFields = def.fields.filter((f) => f.kind === 'text' || f.kind === 'relation').map((f) => f.name);
  const [owner, setOwner] = useState<string | null>(ownerFields.includes('owner_id') ? 'owner_id' : (ownerFields[0] ?? null));
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const toast = useToast();
  const dirty = JSON.stringify(rules) !== JSON.stringify(initial());

  const choose = (a: Action, p: string) => {
    const next: Rule = p === 'locked' ? null : p === 'public' ? '' : p === 'signed' ? SIGNED : p === 'owner' && owner ? ownerRule(a, owner) : (rules[a] || SIGNED);
    setRules((r) => ({ ...r, [a]: next }));
  };

  const save = async () => {
    setBusy(true);
    setError(null);
    try {
      const out: NonNullable<CollectionDef['rules']> = {};
      for (const a of ACTIONS) if (rules[a.key] !== null) out[a.key] = rules[a.key];
      onSaved(await db.admin.collections.update({ ...def, rules: out }));
      toast('Rules saved');
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="mx-auto max-w-3xl space-y-5 p-6">
      <div>
        <h2 className="text-lg font-semibold">Who can do what</h2>
        <p className="text-sm text-muted">Each action has a rule. With no rule, only administrators can do it. Rules are checked on the server before anything is read or written; administrators and administrator keys always pass.</p>
      </div>
      {ownerFields.length > 0 && (
        <div className="flex items-center gap-2 text-sm">
          <span className="text-muted">Owner field for "owner only" rules:</span>
          <Select className="w-44 font-mono" value={owner ?? ''} onChange={(e) => setOwner(e.target.value)}>
            {ownerFields.map((f) => <option key={f}>{f}</option>)}
          </Select>
        </div>
      )}
      {ACTIONS.map(({ key, title, what, vars }) => {
        const p = preset(key, rules[key], owner);
        return (
          <Card key={key} className="p-4">
            <div className="flex flex-wrap items-center gap-3">
              <div className="w-28 font-semibold">{title}</div>
              <Select className="w-64" value={p} onChange={(e) => choose(key, e.target.value)} aria-label={`${title} rule`}>
                <option value="locked">Locked: administrators only</option>
                <option value="public">Public: anyone</option>
                <option value="signed">Signed-in users</option>
                <option value="owner" disabled={!owner}>Owner only{owner ? ` (${owner})` : ''}</option>
                <option value="custom">Custom expression…</option>
              </Select>
              <Badge tone={p === 'locked' ? 'red' : p === 'public' ? 'gold' : 'green'}>{p === 'locked' ? 'locked' : p === 'public' ? 'open to everyone' : 'restricted'}</Badge>
            </div>
            <p className="mt-2 text-sm text-muted">{what}. {describe(key, rules[key], owner)}</p>
            {p === 'custom' && (
              <div className="mt-3">
                <Textarea rows={2} value={rules[key] ?? ''} onChange={(e) => setRules((r) => ({ ...r, [key]: e.target.value }))} spellCheck={false} aria-label={`${title} rule expression`} className={cn(!rules[key] && 'border-danger/40')} placeholder={SIGNED} />
                <p className="mt-1 text-xs text-muted">{vars} Operators: == != &lt; &lt;= &gt; &gt;= ~ (contains) in, &amp;&amp; || !. The signed-in user is @request.auth.id (empty when signed out).</p>
              </div>
            )}
          </Card>
        );
      })}
      <ErrorBox error={error} />
      <div className="flex items-center gap-3">
        <Button variant="primary" disabled={busy || !dirty} onClick={save}>{busy ? 'Saving…' : 'Save rules'}</Button>
        {dirty && <Button variant="ghost" onClick={() => setRules(initial())}>Discard changes</Button>}
      </div>
    </div>
  );
}

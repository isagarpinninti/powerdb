import { useState } from 'react';
import type { FieldDef } from '@sdk';
import { db } from '../session';
import { Button, ErrorBox, Field, Input, useToast } from '../ui';
import { FieldsEditor, NAME_RE, fieldProblems } from './FieldsEditor';

export function NewCollection({ onCreated }: { onCreated: (name: string) => void | Promise<void> }) {
  const [name, setName] = useState('');
  const [encrypt, setEncrypt] = useState(false);
  const [fields, setFields] = useState<FieldDef[]>([{ name: 'title', kind: 'text', required: true }]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const toast = useToast();
  const problems = fieldProblems(fields);
  const nameOk = NAME_RE.test(name);

  const create = async (e: React.FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      await db.admin.collections.create({ name, fields, encrypt });
      toast(`Created ${name}`);
      await onCreated(name);
    } catch (err) {
      setError(err);
      setBusy(false);
    }
  };

  return (
    <form onSubmit={create} className="space-y-6 p-8">
      <div>
        <h1 className="text-2xl font-bold">New collection</h1>
        <p className="text-sm text-muted">New collections are locked: only administrators can use them until you set access rules.</p>
      </div>
      <Field label="Name" hint="Lowercase letters, digits and underscores; used in URLs and code.">
        <Input value={name} onChange={(e) => setName(e.target.value)} placeholder="invoices" className="max-w-sm font-mono" autoFocus spellCheck={false} />
      </Field>
      <label className="flex items-start gap-3 rounded-lg border border-line p-4">
        <input type="checkbox" className="mt-0.5 h-4 w-4 accent-ink" checked={encrypt} onChange={(e) => setEncrypt(e.target.checked)} />
        <span className="text-sm">
          <b>Encrypt files at rest</b> (private files)
          <span className="block text-muted">ChaCha20-Poly1305 per chunk. Encrypted files are decrypted while streaming, so they give up the zero-copy shortcut that unencrypted files can use. Cannot be changed later.</span>
        </span>
      </label>
      <div>
        <h2 className="mb-2 text-lg font-semibold">Fields</h2>
        <FieldsEditor fields={fields} onChange={setFields} />
      </div>
      {problems.length > 0 && <ul className="list-disc space-y-1 pl-5 text-sm text-danger">{problems.map((p) => <li key={p}>{p}</li>)}</ul>}
      <ErrorBox error={error} />
      <Button type="submit" variant="primary" disabled={busy || !nameOk || problems.length > 0}>{busy ? 'Creating…' : 'Create collection'}</Button>
    </form>
  );
}

import { useState } from 'react';
import type { CollectionDef, FieldDef } from '@sdk';
import { db } from '../session';
import { Button, Card, ErrorBox, useToast } from '../ui';
import { FieldsEditor, fieldProblems } from './FieldsEditor';

export function SchemaTab({ def, onSaved, onDropped }: { def: CollectionDef; onSaved: (d: CollectionDef) => void; onDropped: () => void }) {
  const [fields, setFields] = useState<FieldDef[]>(def.fields);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const toast = useToast();
  const problems = fieldProblems(fields);
  const dirty = JSON.stringify(fields) !== JSON.stringify(def.fields);

  const save = async () => {
    setBusy(true);
    setError(null);
    try {
      onSaved(await db.admin.collections.update({ ...def, fields }));
      toast('Schema saved');
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  };

  const drop = async () => {
    if (!confirm(`Delete the collection "${def.name}" and every record and file in it? This cannot be undone.`)) return;
    try {
      const n = await db.admin.collections.drop(def.name);
      toast(`Deleted ${def.name} (${n} records)`);
      onDropped();
    } catch (e) {
      setError(e);
    }
  };

  return (
    <div className="space-y-6 p-6">
      <div>
        <h2 className="text-lg font-semibold">Fields</h2>
        <p className="mb-4 text-sm text-muted">Records are checked against these when they are created or changed. Removing a field hides it from new writes; data already stored under it stays on disk.</p>
        <FieldsEditor fields={fields} onChange={setFields} />
      </div>
      {problems.length > 0 && <ul className="list-disc space-y-1 pl-5 text-sm text-danger">{problems.map((p) => <li key={p}>{p}</li>)}</ul>}
      <ErrorBox error={error} />
      <div className="flex items-center gap-3">
        <Button variant="primary" disabled={busy || !dirty || problems.length > 0} onClick={save}>{busy ? 'Saving…' : 'Save schema'}</Button>
        {dirty && <Button variant="ghost" onClick={() => setFields(def.fields)}>Discard changes</Button>}
      </div>
      <Card className="p-5">
        <h3 className="font-semibold">Storage</h3>
        <p className="mt-1 text-sm text-muted">
          {def.encrypt
            ? 'Files in this collection are encrypted at rest (ChaCha20-Poly1305). They are decrypted as they stream out, so they cannot use the kernel-to-socket shortcut that unencrypted files can. This setting is fixed when the collection is created.'
            : 'Files in this collection are stored unencrypted and can be streamed straight from the log. Encryption is chosen when a collection is created and cannot be added later.'}
        </p>
      </Card>
      <Card className="border-danger/40 p-5">
        <h3 className="font-semibold text-danger">Delete collection</h3>
        <p className="mt-1 text-sm text-muted">Removes every record immediately. File space is reclaimed in the background.</p>
        <Button variant="danger" className="mt-3" onClick={drop}>Delete {def.name}</Button>
      </Card>
    </div>
  );
}

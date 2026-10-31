import { useCallback, useEffect, useState } from 'react';
import type { User } from '@sdk';
import { db } from '../session';
import { Badge, Button, Card, ErrorBox, Field, Input, Modal, Select, Spinner, fmtDate, useToast } from '../ui';

export function Users() {
  const [users, setUsers] = useState<User[] | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [adding, setAdding] = useState(false);
  const toast = useToast();
  const load = useCallback(() => db.admin.users.list().then(setUsers, setError), []);
  useEffect(() => { void load(); }, [load]);

  const remove = async (u: User) => {
    if (!confirm(`Delete ${u.email}? Their sessions end immediately.`)) return;
    try { await db.admin.users.delete(u.id); toast('User deleted'); void load(); } catch (e) { toast(e instanceof Error ? e.message : 'Failed', 'err'); }
  };

  return (
    <div className="mx-auto max-w-4xl space-y-4 p-8">
      <div className="flex items-center justify-between">
        <div><h1 className="text-2xl font-bold">Users</h1><p className="text-sm text-muted">Accounts that can sign in. Registration is closed unless the server was started with --open-registration.</p></div>
        <Button variant="primary" onClick={() => setAdding(true)}>Add user</Button>
      </div>
      <ErrorBox error={error} />
      {users === null ? <Spinner /> : (
        <Card className="overflow-hidden">
          <table className="w-full text-sm">
            <thead className="bg-raised text-left text-xs uppercase tracking-wide text-muted"><tr><th className="px-4 py-2">Email</th><th>Role</th><th>Created</th><th className="px-4" /></tr></thead>
            <tbody>
              {users.map((u) => (
                <tr key={u.id} className="border-t border-line">
                  <td className="px-4 py-2.5"><div>{u.email}</div><div className="font-mono text-xs text-muted">{u.id}</div></td>
                  <td><Badge tone={u.role === 'admin' ? 'gold' : 'slate'}>{u.role}</Badge></td>
                  <td className="text-muted">{fmtDate(u.created_at)}</td>
                  <td className="px-4 text-right"><Button small variant="danger" onClick={() => remove(u)}>Delete</Button></td>
                </tr>
              ))}
              {users.length === 0 && <tr><td colSpan={4} className="px-4 py-8 text-center text-muted">No users. Sign in with an admin key and add one.</td></tr>}
            </tbody>
          </table>
        </Card>
      )}
      {adding && <AddUser onClose={() => setAdding(false)} onAdded={() => { setAdding(false); void load(); }} />}
    </div>
  );
}

function AddUser({ onClose, onAdded }: { onClose: () => void; onAdded: () => void }) {
  const [email, setEmail] = useState('');
  const [password, setPassword] = useState('');
  const [role, setRole] = useState<'user' | 'admin'>('user');
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setBusy(true);
    try { await db.admin.users.create({ email, password, role }); onAdded(); } catch (err) { setError(err); setBusy(false); }
  };
  return (
    <Modal title="Add user" onClose={onClose}>
      <form onSubmit={submit} className="space-y-4">
        <Field label="Email"><Input type="email" value={email} onChange={(e) => setEmail(e.target.value)} required autoFocus /></Field>
        <Field label="Password" hint="At least 8 characters."><Input type="password" value={password} onChange={(e) => setPassword(e.target.value)} required minLength={8} autoComplete="new-password" /></Field>
        <Field label="Role"><Select value={role} onChange={(e) => setRole(e.target.value as 'user' | 'admin')}><option value="user">user: subject to collection rules</option><option value="admin">admin: full access, bypasses rules</option></Select></Field>
        <ErrorBox error={error} />
        <div className="flex justify-end gap-2"><Button type="button" onClick={onClose}>Cancel</Button><Button type="submit" variant="primary" disabled={busy}>Add user</Button></div>
      </form>
    </Modal>
  );
}

import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { File as NodeFile } from 'node:buffer';
import { beforeEach, describe, expect, it } from 'vitest';
import { App } from '../src/App';
import { SessionProvider, db } from '../src/session';
import { ToastProvider } from '../src/ui';

const KEY = process.env.POWERDB_TEST_KEY!;

function open() {
  return render(
    <ToastProvider>
      <SessionProvider>
        <App />
      </SessionProvider>
    </ToastProvider>,
  );
}

async function signIn(user: ReturnType<typeof userEvent.setup>) {
  open();
  await user.type(await screen.findByLabelText(/admin api key/i), KEY);
  await user.click(screen.getByRole('button', { name: /sign in/i }));
  await screen.findByRole('button', { name: 'System' });
}

beforeEach(() => db.setToken(undefined));

describe('signing in', () => {
  it('refuses a wrong key and says so', async () => {
    const user = userEvent.setup();
    open();
    await user.type(await screen.findByLabelText(/admin api key/i), 'pdb_live_notarealkeynotarealkeynotarealkey00000');
    await user.click(screen.getByRole('button', { name: /sign in/i }));
    expect(await screen.findByRole('alert')).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'System' })).not.toBeInTheDocument();
  });

  it('signs in with the administrator key', async () => {
    await signIn(userEvent.setup());
    expect(screen.getByRole('button', { name: 'API keys' })).toBeInTheDocument();
  });

  it('signs in from the desktop app\'s #key= fragment, and removes it from the address', async () => {
    window.history.replaceState(null, '', `/#key=${KEY}`);
    open();
    await screen.findByRole('button', { name: 'System' });
    expect(window.location.hash).toBe('');
  });

  it('ignores a malformed #key= fragment', async () => {
    window.history.replaceState(null, '', '/#key=not-a-key<script>');
    open();
    expect(await screen.findByLabelText(/admin api key/i)).toBeInTheDocument();
  });
});

describe('administering', () => {
  it('creates an API key and shows its secret once', async () => {
    const user = userEvent.setup();
    await signIn(user);
    await user.click(screen.getByRole('button', { name: 'API keys' }));
    await user.click(await screen.findByRole('button', { name: 'Create key' }));
    const dialog = await screen.findByRole('dialog');
    await user.type(within(dialog).getByLabelText(/name/i), 'reporting service');
    await user.click(within(dialog).getByRole('button', { name: 'Create key' }));
    const secret = await screen.findByText(/^pdb_live_/);
    expect(secret.textContent).toMatch(/^pdb_live_[A-Za-z0-9_-]{20,}$/);
    await user.click(screen.getByRole('button', { name: 'Done' }));
    expect(await screen.findByText('reporting service')).toBeInTheDocument();
    expect(screen.queryByText(/^pdb_live_/)).not.toBeInTheDocument();
  });

  it('adds a user, who can then sign in', async () => {
    const user = userEvent.setup();
    await signIn(user);
    await user.click(screen.getByRole('button', { name: 'Users' }));
    await user.click(await screen.findByRole('button', { name: 'Add user' }));
    const dialog = await screen.findByRole('dialog');
    await user.type(within(dialog).getByLabelText('Email'), 'ann@example.com');
    await user.type(within(dialog).getByLabelText(/^password/i), 'correct horse battery');
    await user.click(within(dialog).getByRole('button', { name: 'Add user' }));
    expect(await screen.findByText('ann@example.com')).toBeInTheDocument();
    // the account really exists on the server
    const login = await fetch(`${window.location.origin}/api/v1/auth/login`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ email: 'ann@example.com', password: 'correct horse battery' }) });
    expect(login.status).toBe(200);
  });

  it('takes a backup from the System page and lists it', async () => {
    const user = userEvent.setup();
    await signIn(user);
    await user.click(screen.getByRole('button', { name: 'System' }));
    await user.click(await screen.findByRole('button', { name: 'Back up now' }));
    await waitFor(() => expect(screen.getAllByText(/^backup-\d{8}-\d{6}$/).length).toBeGreaterThan(0), { timeout: 20000 });
  });

  it('signing out returns to the sign-in screen', async () => {
    const user = userEvent.setup();
    await signIn(user);
    await user.click(screen.getByRole('button', { name: /sign out/i }));
    expect(await screen.findByLabelText(/admin api key/i)).toBeInTheDocument();
  });
});

describe('files in buckets', () => {
  it('creates a bucket, uploads a file, filters, and deletes it', async () => {
    const user = userEvent.setup();
    await signIn(user);
    await user.click(await screen.findByRole('button', { name: 'New bucket' }));
    const dialog = await screen.findByRole('dialog');
    await user.type(within(dialog).getByLabelText(/^name/i), 'ui-docs');
    await user.click(within(dialog).getByRole('button', { name: 'Create bucket' }));
    expect(await screen.findByRole('heading', { name: /ui-docs/ })).toBeInTheDocument();
    expect(await screen.findByText(/Nothing here yet/)).toBeInTheDocument();

    // (Node's File, because the one jsdom provides is not a Blob to Node's fetch; a browser sends its own File straight from disk)
    const file = new NodeFile(['quarterly numbers'], 'q1.txt', { type: 'text/plain' }) as unknown as File;
    await user.upload(screen.getByLabelText('Choose files to upload'), file);
    expect(await screen.findByText('q1.txt')).toBeInTheDocument();
    expect(screen.getByText('text/plain')).toBeInTheDocument();
    // the bytes really arrived
    expect(new TextDecoder().decode(await db.bucket('ui-docs').download('q1.txt'))).toBe('quarterly numbers');

    await user.type(screen.getByLabelText('Name starts with'), 'nope/');
    await user.click(screen.getByRole('button', { name: 'Filter' }));
    expect(await screen.findByText('Nothing starts with that.')).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Clear' }));
    expect(await screen.findByText('q1.txt')).toBeInTheDocument();

    window.confirm = () => true;
    await user.click(screen.getByRole('button', { name: 'Delete q1.txt' }));
    expect(await screen.findByText(/Nothing here yet/)).toBeInTheDocument();
  });

  it('shows the versions of a file and restores an old one', async () => {
    db.setToken(KEY);
    await db.buckets.create('ui-versions', { versioning: true });
    const b = db.bucket('ui-versions');
    const one = await b.put('note.txt', 'first', { type: 'text/plain' });
    await b.put('note.txt', 'second', { type: 'text/plain' });
    const user = userEvent.setup();
    await signIn(user);
    await user.click(await screen.findByRole('button', { name: 'ui-versions' }));
    await user.click(await screen.findByRole('button', { name: 'Versions of note.txt' }));
    const dialog = await screen.findByRole('dialog');
    expect(within(dialog).getByText('newest')).toBeInTheDocument();
    const restore = within(dialog).getAllByRole('button', { name: 'Restore' });
    expect(restore).toHaveLength(1); // only the old version can be restored
    await user.click(restore[0]);
    await waitFor(async () => expect(new TextDecoder().decode(await b.download('note.txt'))).toBe('first'));
    expect((await b.versions('note.txt')).length).toBe(3);
    expect(one.id).toBeTruthy();
  });

  it('says plainly when the server refuses a file', async () => {
    db.setToken(KEY);
    await db.buckets.create('ui-checks');
    const user = userEvent.setup();
    await signIn(user);
    await user.click(await screen.findByRole('button', { name: 'ui-checks' }));
    const exe = new File([new Uint8Array([0x4d, 0x5a, 0x90, 0, 3, 0, 0, 0, 1, 2, 3, 4])], 'photo.png', { type: 'image/png' });
    await user.upload(await screen.findByLabelText('Choose files to upload'), exe);
    expect(await screen.findByText(/photo\.png/)).toBeInTheDocument(); // the toast names the file and the reason
    expect(await screen.findByText(/Nothing here yet/)).toBeInTheDocument();
  });
});

import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
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

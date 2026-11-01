// Starts a real PowerDB server (ephemeral data directory) for the whole test run and hands its admin key to the tests.
import { spawn } from 'node:child_process';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

export default async function setup() {
  const here = path.dirname(fileURLToPath(import.meta.url));
  const port = Number(process.env.DASH_TEST_PORT ?? 18090);
  const exe = process.platform === 'win32' ? '.exe' : '';
  const bin = process.env.POWERDB_BIN ?? path.resolve(here, '../../target/release', `powerdb${exe}`);
  const child = spawn(bin, ['serve', '--ephemeral', '--addr', `127.0.0.1:${port}`], { stdio: ['ignore', 'pipe', 'pipe'] });

  const key = await new Promise<string>((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error(`the server did not print an admin key (is ${bin} built? cargo build --release)`)), 30_000);
    let seen = '';
    const onData = (d: Buffer) => {
      seen += d.toString();
      const m = /pdb_live_[A-Za-z0-9_-]+/.exec(seen);
      if (m) { clearTimeout(timer); resolve(m[0]); }
    };
    child.stderr!.on('data', onData);
    child.stdout!.on('data', onData);
    child.on('error', reject);
    child.on('exit', (c) => reject(new Error(`the server exited early (${c})`)));
  });
  for (let i = 0; i < 50; i++) {
    try { if ((await fetch(`http://127.0.0.1:${port}/api/v1/healthz`)).ok) break; } catch { /* not up yet */ }
    await new Promise((r) => setTimeout(r, 100));
  }
  process.env.POWERDB_TEST_KEY = key; // workers inherit it
  return async () => { child.removeAllListeners('exit'); child.kill('SIGTERM'); };
}

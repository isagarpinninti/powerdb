// The SDK against a real `powerdb serve` process: nothing is mocked.
import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { randomBytes } from 'node:crypto';
import { PowerDB, PowerDBError, quote } from '../dist/index.js';

const BIN = join(dirname(fileURLToPath(import.meta.url)), '../../target/release/powerdb');
let proc, dir, url, admin;
const PORT = 20000 + Math.floor(Math.random() * 20000);

before(async () => {
  dir = mkdtempSync(join(tmpdir(), 'powerdb-sdk-'));
  url = `http://127.0.0.1:${PORT}`;
  proc = spawn(BIN, ['serve', '--data', join(dir, 'data'), '--addr', `127.0.0.1:${PORT}`, '--durability', 'never'], {
    env: { ...process.env, POWERDB_ADMIN_EMAIL: 'root@example.com', POWERDB_ADMIN_PASSWORD: 'correct horse battery' },
    stdio: 'ignore',
  });
  for (let i = 0; i < 100; i++) {
    try { if ((await fetch(url + '/api/v1/healthz')).ok) break; } catch {}
    await new Promise((r) => setTimeout(r, 50));
  }
  admin = new PowerDB(url);
  await admin.auth.login('root@example.com', 'correct horse battery');
});
after(() => { proc?.kill(); rmSync(dir, { recursive: true, force: true }); });

const png = (n) => Buffer.concat([Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]), randomBytes(n)]);

test('quote() keeps hostile text inside the string', () => {
  assert.equal(quote('paid'), '"paid"');
  assert.equal(quote('a"b'), '"a\\"b"');
  assert.equal(quote('a\\b'), '"a\\\\b"');
  assert.equal(quote('line\nbreak\ttab'), '"line\\nbreak\\ttab"');
  assert.equal(quote('bell\u0007gone'), '"bellgone"');
  assert.equal(quote(5), '5');
  assert.equal(quote(null), 'null');
  assert.equal(quote(true), 'true');
  assert.equal(quote(['a', 1]), '["a", 1]');
  assert.throws(() => quote(NaN), TypeError);
  assert.throws(() => quote({}), TypeError);
});

test('create with a file, read, filter, download, update, delete', async () => {
  await admin.admin.collections.create({
    name: 'invoices',
    fields: [
      { name: 'client', kind: 'text', required: true },
      { name: 'amount', kind: 'number' },
      { name: 'status', kind: 'text' },
      { name: 'document', kind: 'file', mime: ['image/*', 'application/pdf'], max: 5_000_000 },
    ],
    rules: { list: '', view: '', create: '', update: '', delete: '' },
  });
  const col = admin.collection('invoices');
  const bytes = png(700_000);
  const rec = await col.create({ client: 'Acme Corp', amount: 4500, status: 'paid', document: { file: bytes, name: 'invoice.png', type: 'image/png' } });
  assert.match(rec.id, /^[0-9a-f]{32}$/);
  assert.equal(rec.document.size, bytes.length);
  assert.equal(rec.document.mime, 'image/png');
  assert.equal(rec.document.name, 'invoice.png');

  const downloaded = await admin.download(rec.document);
  assert.deepEqual(Buffer.from(downloaded), bytes, 'the downloaded bytes are the uploaded bytes');

  await col.create({ client: 'Small Co', amount: 200, status: 'open' });
  const paid = await col.where('status == "paid" && amount >= 1000').sort('-created_at').limit(20).get();
  assert.equal(paid.items.length, 1);
  assert.equal(paid.items[0].client, 'Acme Corp');

  const updated = await col.update(rec.id, { status: 'archived' });
  assert.equal(updated.status, 'archived');
  assert.ok(updated.document.url, 'the file is still attached');
  const swapped = await col.update(rec.id, { document: { file: png(1000), name: 'new.png', type: 'image/png' } });
  assert.equal(swapped.document.name, 'new.png');
  const removed = await col.update(rec.id, {}, { removeFiles: ['document'] });
  assert.equal(removed.document, undefined);

  await col.delete(rec.id);
  await assert.rejects(col.get(rec.id), (e) => e instanceof PowerDBError && e.status === 404 && e.code === 'not_found');
});

test('values interpolated through the template tag cannot rewrite the filter', async () => {
  const col = admin.collection('invoices');
  for (const s of ['paid', 'open']) await col.create({ client: `c-${s}`, status: s });
  const hostile = 'x" || true || "';
  const hit = await col.where`status == ${hostile}`.get();
  assert.equal(hit.items.length, 0, 'the hostile text is a value that matches nothing');
  const real = await col.where`status == ${'paid'} && client ~ ${'c-'}`.get();
  assert.ok(real.items.length >= 1 && real.items.every((r) => r.status === 'paid'));
  // the unsafe way, to show what the tag protects against
  const unsafe = await col.where(`status == "${hostile}"`).get();
  assert.ok(unsafe.items.length > 0, 'string concatenation lets the input rewrite the filter');
});

test('all() pages through every match', async () => {
  await admin.admin.collections.create({ name: 'items', fields: [{ name: 'n', kind: 'number' }], rules: { list: '', create: '' } });
  const col = admin.collection('items');
  for (let i = 0; i < 25; i++) await col.create({ n: i });
  const everything = await col.query().sort('n').all(10);
  assert.deepEqual(everything.map((r) => r.n), Array.from({ length: 25 }, (_, i) => i));
  assert.equal((await col.query().first()).n, 24, 'newest first by default');
  const page = await col.limit(5).get();
  assert.equal(page.items.length, 5);
  assert.equal(page.has_more, true);
});

test('sessions, ownership rules and errors', async () => {
  await admin.admin.collections.create({
    name: 'notes',
    fields: [{ name: 'text', kind: 'text' }, { name: 'owner_id', kind: 'text' }],
    rules: { list: '@request.auth.id != ""', view: '@request.auth.id != ""', create: '@request.auth.id != "" && @request.body.owner_id == @request.auth.id', update: '@request.auth.id == owner_id', delete: '@request.auth.id == owner_id' },
  });
  await admin.admin.users.create({ email: 'ann@example.com', password: 'long enough pw' });
  await admin.admin.users.create({ email: 'ben@example.com', password: 'long enough pw' });
  const ann = new PowerDB(url), ben = new PowerDB(url);
  const a = (await ann.auth.login('ann@example.com', 'long enough pw')).user;
  await ben.auth.login('ben@example.com', 'long enough pw');
  const anon = new PowerDB(url);

  await assert.rejects(anon.collection('notes').list(), (e) => e.status === 401);
  const mine = await ann.collection('notes').create({ text: 'hello', owner_id: a.id });
  await assert.rejects(ann.collection('notes').create({ text: 'forged', owner_id: 'someone-else' }), (e) => e instanceof PowerDBError && e.status === 403);
  assert.equal((await ben.collection('notes').get(mine.id)).text, 'hello', 'ben can read it');
  await assert.rejects(ben.collection('notes').update(mine.id, { text: 'hacked' }), (e) => e.status === 403);
  await assert.rejects(ben.collection('notes').delete(mine.id), (e) => e.status === 403);
  assert.equal((await ann.collection('notes').update(mine.id, { text: 'edited' })).text, 'edited');

  await assert.rejects(new PowerDB(url).auth.login('ann@example.com', 'wrong password'), (e) => e.status === 401);
  await assert.rejects(ann.collection('notes').where('nope == 1').get(), (e) => e.status === 400 && /no field/.test(e.message));
  ann.auth.logout();
  await assert.rejects(ann.collection('notes').list(), (e) => e.status === 401, 'after logout the client is anonymous');
});

test('API keys carry their scope', async () => {
  const { key, meta } = await admin.admin.keys.create({ name: 'reader', access: 'read', collections: ['items'] });
  const reader = new PowerDB(url, key);
  assert.ok((await reader.collection('items').list()).items.length > 0);
  await assert.rejects(reader.collection('items').create({ n: 1 }), (e) => e.status === 403);
  await assert.rejects(reader.collection('notes').list(), (e) => e.status === 403);
  await admin.admin.keys.revoke(meta.id);
  await assert.rejects(reader.collection('items').list(), (e) => e.status === 401);
});

test('uploads that fail the content check are rejected with a clear error', async () => {
  const exe = Buffer.concat([Buffer.from('MZ\x90\x00'), randomBytes(5000)]);
  await assert.rejects(admin.collection('invoices').create({ client: 'x', document: { file: exe, name: 'photo.png', type: 'image/png' } }), (e) => e instanceof PowerDBError && e.status === 400 && /executable/.test(e.message));
});

test('OAuth helpers build the sign-in URL and read the result from the fragment', () => {
  const db = new PowerDB('http://localhost:1234/');
  assert.equal(db.auth.oauthUrl('github', 'https://app.example/done'), 'http://localhost:1234/api/v1/auth/oauth/github/start?redirect=https%3A%2F%2Fapp.example%2Fdone');
  assert.equal(db.auth.oauthUrl('google'), 'http://localhost:1234/api/v1/auth/oauth/google/start');
  assert.equal(db.auth.finishOAuth('#something=else'), null);
  assert.throws(() => db.auth.finishOAuth('#error=no_account'), (e) => e instanceof PowerDBError && /no_account/.test(e.message));
  assert.equal(db.auth.finishOAuth('#token=abc.def.ghi'), 'abc.def.ghi');
});

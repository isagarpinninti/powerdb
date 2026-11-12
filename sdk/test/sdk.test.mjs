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

// ---- buckets and objects ----

test('buckets: create, list, versioning, delete', async () => {
  const b = await admin.buckets.create('sdk-docs');
  assert.equal(b.name, 'sdk-docs');
  assert.equal(b.versioning, false);
  assert.ok((await admin.buckets.list()).some((x) => x.name === 'sdk-docs'));
  assert.equal((await admin.buckets.get('sdk-docs')).name, 'sdk-docs');
  await assert.rejects(admin.buckets.create('sdk-docs'), (e) => e instanceof PowerDBError && e.status === 409);
  await assert.rejects(admin.buckets.create('Bad Name'), (e) => e instanceof PowerDBError && e.status === 400);
  assert.equal((await admin.buckets.enableVersioning('sdk-docs')).versioning, true);
  await admin.buckets.delete('sdk-docs');
  await assert.rejects(admin.buckets.get('sdk-docs'), (e) => e.status === 404);
});

test('objects: put, head, download, range, list, delete', async () => {
  await admin.buckets.create('sdk-files');
  const files = admin.bucket('sdk-files');
  const body = randomBytes(2_500_000);
  const info = await files.put('reports/2026/q1.bin', body, { filename: 'Q1 report.bin', metadata: { customer: 'acme' } });
  assert.equal(info.size, body.length);
  assert.equal(info.key, 'reports/2026/q1.bin');
  assert.equal(info.metadata.customer, 'acme');

  const got = await files.download('reports/2026/q1.bin');
  assert.ok(Buffer.from(got).equals(body), 'the download is byte for byte the upload');
  const part = await files.download('reports/2026/q1.bin', { range: { start: 1000, end: 1999 } });
  assert.ok(Buffer.from(part).equals(body.subarray(1000, 2000)));

  const head = await files.head('reports/2026/q1.bin');
  assert.equal(head.size, body.length);
  assert.equal(head.checksum, info.checksum);
  assert.equal(head.metadata.customer, 'acme');

  // keys with spaces, unicode and slashes
  await files.put('weird name/日本 & more.txt', 'hello', { type: 'text/plain' });
  assert.equal(new TextDecoder().decode(await files.download('weird name/日本 & more.txt')), 'hello');

  const keys = [];
  for await (const o of files.each()) keys.push(o.key);
  assert.deepEqual(keys, [...keys].sort());
  assert.equal(keys.length, 2);
  assert.equal((await files.list({ prefix: 'reports/' })).items.length, 1);

  await files.delete('reports/2026/q1.bin');
  await assert.rejects(files.download('reports/2026/q1.bin'), (e) => e instanceof PowerDBError && e.status === 404);
});

test('objects: a stream is uploaded without being held in memory', async () => {
  await admin.buckets.create('sdk-stream');
  const files = admin.bucket('sdk-stream');
  const sent = [];
  const src = [randomBytes(300_000), randomBytes(300_000), randomBytes(100_000)];
  const s2 = new ReadableStream({
    pull(c) {
      const n = src.shift();
      if (n) { sent.push(n); c.enqueue(new Uint8Array(n)); } else c.close();
    },
  });
  const info = await files.put('streamed.bin', s2);
  assert.equal(info.size, 700_000);
  assert.ok(Buffer.from(await files.download('streamed.bin')).equals(Buffer.concat(sent)));
});

test('objects: versions, delete markers and restore', async () => {
  await admin.buckets.create('sdk-versions', { versioning: true });
  const v = admin.bucket('sdk-versions');
  const one = await v.put('doc.txt', 'one', { type: 'text/plain' });
  const two = await v.put('doc.txt', 'two', { type: 'text/plain' });
  assert.equal(one.id, two.id);
  assert.deepEqual((await v.versions('doc.txt')).map((x) => x.version_id), [two.version_id, one.version_id]);
  assert.equal(new TextDecoder().decode(await v.download('doc.txt', { version: one.version_id })), 'one');
  await v.delete('doc.txt');
  await assert.rejects(v.download('doc.txt'), (e) => e.status === 404);
  assert.equal((await v.versions('doc.txt')).length, 3);
  await v.restore('doc.txt', one.version_id);
  assert.equal(new TextDecoder().decode(await v.download('doc.txt')), 'one');
});

test('objects: the server refuses a program called an image', async () => {
  await admin.buckets.create('sdk-checks');
  const exe = Buffer.concat([Buffer.from('MZ\x90\x00\x03\x00\x00\x00'), randomBytes(2000)]);
  await assert.rejects(admin.bucket('sdk-checks').put('photo.png', exe, { type: 'image/png' }), (e) => e instanceof PowerDBError && e.status === 400);
  await assert.rejects(admin.bucket('sdk-checks').download('photo.png'), (e) => e.status === 404, 'nothing was stored');
});

test('objects: an ordinary user and an anonymous caller are refused', async () => {
  const anon = new PowerDB(url);
  await assert.rejects(anon.buckets.list(), (e) => e instanceof PowerDBError && e.status === 401);
  const r = await fetch(url + '/api/v1/users', {
    method: 'POST',
    headers: { 'content-type': 'application/json', authorization: `Bearer ${(await admin.auth.login('root@example.com', 'correct horse battery')).token}` },
    body: JSON.stringify({ email: 'plain@example.com', password: 'another long passphrase' }),
  });
  assert.equal(r.status, 201);
  const user = new PowerDB(url);
  await user.auth.login('plain@example.com', 'another long passphrase');
  await assert.rejects(user.buckets.list(), (e) => e instanceof PowerDBError && e.status === 403);
});

test('typed tables: create, insert with generated ids and a file, page, find, alter, drop', async () => {
  const t = await admin.tables.create({
    name: 'sdk_people',
    columns: [
      { name: 'id', type: 'uuid', generate: 'uuid' },
      { name: 'name', type: 'text', nullable: false },
      { name: 'age', type: 'int32' },
      { name: 'big', type: 'int64' },
      { name: 'photo', type: 'file' },
    ],
    primary_key: ['id'],
    constraints: [{ name: 'adult', kind: 'check', expr: 'age == null || age >= 18' }],
    indexes: [{ name: 'by_name', columns: ['name'] }],
  });
  assert.deepEqual(t.primary_key, ['id']);
  const people = admin.table('sdk_people');
  const a = await people.insert({ name: 'Asha', age: 30, big: '9007199254740993' });
  assert.match(a.id, /^[0-9a-f-]{36}$/);
  assert.equal(a.big, '9007199254740993', '64-bit integers keep every digit');
  const body = png(2000);
  const b = await people.insert({ name: 'Ben' }, { photo: new File([body], 'me.png', { type: 'image/png' }) });
  assert.equal(b.photo.size, body.length);
  assert.deepEqual(Buffer.from(await people.download(b.photo)).subarray(0, 8), body.subarray(0, 8));
  await assert.rejects(people.insert({ name: 'Kid', age: 3 }), (e) => e instanceof PowerDBError && e.status === 400);
  assert.equal((await people.get(a.id)).name, 'Asha');
  assert.equal((await people.update(a.id, { age: null })).age, null);
  assert.equal((await people.find('by_name', ['Ben'])).items.length, 1);
  const all = [];
  for await (const r of people.rows(1)) all.push(r.name);
  assert.equal(all.length, 2);
  await admin.tables.alter('sdk_people', { op: 'add_column', name: 'nick', type: 'text' });
  assert.equal((await admin.tables.get('sdk_people')).table.columns.length, 6);
  await people.delete(a.id);
  await assert.rejects(people.get(a.id), (e) => e.status === 404);
  assert.equal(await admin.tables.drop('sdk_people'), 1);
});

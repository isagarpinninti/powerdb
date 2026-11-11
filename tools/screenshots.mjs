// Takes the screenshots in docs/screenshots with headless Chrome (DevTools protocol; needs Node 22+ for WebSocket).
//
//   node tools/screenshots.mjs --dashboard http://127.0.0.1:8300 --key pdb_live_... --ui http://127.0.0.1:1420 --out docs/screenshots
//
// The dashboard shots are the real dashboard on a server filled by tools/demo-data.py. The app shots are the app's interface
// running in a browser with sample status data (`npm run dev` in desktop/), with the real dashboard shown in its Data section.
import http from 'node:http';
import { spawn } from 'node:child_process';
import { mkdtempSync, mkdirSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';

const arg = (n, d) => { const i = process.argv.indexOf(`--${n}`); return i > 0 ? process.argv[i + 1] : d; };
const CHROME = arg('chrome', '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome');
const DASH = arg('dashboard', 'http://127.0.0.1:8300');
const KEY = arg('key');
const UI = arg('ui', 'http://127.0.0.1:1420');
const OUT = arg('out', 'docs/screenshots');
const only = arg('only');
if (!KEY) { console.error('give the demo server\'s admin key with --key'); process.exit(1); }
mkdirSync(OUT, { recursive: true });

// The server only lets the real desktop app frame its dashboard (CSP frame-ancestors). To show the dashboard inside the app's
// interface in an ordinary browser, this pass-through proxy forwards everything and leaves that one restriction out.
const PROXY = 8301;
const proxy = http.createServer((req, res) => {
  const target = new URL(DASH);
  const up = http.request({ host: target.hostname, port: target.port, path: req.url, method: req.method, headers: { ...req.headers, host: target.host } }, (r) => {
    const h = { ...r.headers };
    if (h['content-security-policy']) h['content-security-policy'] = String(h['content-security-policy']).replace(/frame-ancestors[^;]*;?/, '');
    delete h['x-frame-options'];
    res.writeHead(r.statusCode, h);
    r.pipe(res);
  });
  up.on('error', () => { res.writeHead(502); res.end(); });
  req.pipe(up);
}).listen(PROXY, '127.0.0.1');

const port = 9333;
const chrome = spawn(CHROME, ['--headless=new', `--remote-debugging-port=${port}`, `--user-data-dir=${mkdtempSync(path.join(tmpdir(), 'pdb-shots-'))}`, '--hide-scrollbars', '--no-first-run', '--no-default-browser-check', '--force-color-profile=srgb', '--disable-web-security', '--disable-features=IsolateOrigins,site-per-process', 'about:blank'], { stdio: 'ignore' });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

let ws, seq = 0;
const pending = new Map();
async function connect() {
  for (let i = 0; i < 60; i++) {
    try {
      const t = await (await fetch(`http://127.0.0.1:${port}/json/new?about:blank`, { method: 'PUT' })).json();
      ws = new WebSocket(t.webSocketDebuggerUrl);
      await new Promise((res, rej) => { ws.onopen = res; ws.onerror = rej; });
      ws.onmessage = (m) => { const d = JSON.parse(m.data); if (d.id && pending.has(d.id)) { pending.get(d.id)(d); pending.delete(d.id); } };
      return;
    } catch { await sleep(250); }
  }
  throw new Error('could not reach Chrome');
}
const cdp = (method, params = {}) => new Promise((res, rej) => { const id = ++seq; pending.set(id, (d) => (d.error ? rej(new Error(`${method}: ${d.error.message}`)) : res(d.result))); ws.send(JSON.stringify({ id, method, params })); });
const evaluate = async (expression) => (await cdp('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true })).result.value;
async function waitFor(expression, what, ms = 20000) {
  const end = Date.now() + ms;
  while (Date.now() < end) { if (await evaluate(`!!(${expression})`)) return; await sleep(150); }
  throw new Error(`timed out waiting for ${what}`);
}
const clickText = (text, sel = 'button,a,[role=tab]') => evaluate(`(() => { const e=[...document.querySelectorAll(${JSON.stringify(sel)})].find(x => x.textContent.trim().startsWith(${JSON.stringify(text)})); if(!e) return false; e.click(); return true; })()`);
async function goto(url) { await cdp('Page.navigate', { url }); await sleep(600); }
async function shot(name, { width = 1440, height = 900 } = {}) {
  await sleep(500);
  const { data } = await cdp('Page.captureScreenshot', { format: 'webp', quality: 92 });
  writeFileSync(path.join(OUT, `${name}.webp`), Buffer.from(data, 'base64'));
  console.log('wrote', name);
}

const dashboard = [
  ['dashboard-home', async () => { await goto(`${DASH}/#key=${KEY}`); await waitFor(`document.body.innerText.includes('invoices')`, 'the collection list'); }],
  ['dashboard-invoices', async () => { await clickText('invoices'); await waitFor(`document.querySelectorAll('[role=row]').length > 8`, 'invoice rows'); }],
  ['dashboard-record', async () => { await evaluate(`document.querySelectorAll('[role=row]')[2].click()`); await waitFor(`document.querySelector('form[aria-label]')`, 'the record panel'); }],
  ['dashboard-products', async () => { await evaluate(`document.querySelector('form[aria-label] button[aria-label=Close]')?.click()`); await clickText('products'); await waitFor(`document.querySelectorAll('img').length > 3`, 'thumbnails'); await sleep(1200); }],
  ['dashboard-rules', async () => { await clickText('invoices'); await waitFor(`document.querySelector('[role=tab]')`, 'tabs'); await clickText('Access rules', '[role=tab]'); await waitFor(`document.body.innerText.includes('Who can')||document.querySelector('textarea,input[type=text]')`, 'the rules editor'); }],
  ['dashboard-code', async () => { await clickText('Code', '[role=tab]'); await waitFor(`document.querySelector('pre')`, 'code samples'); }],
  ['dashboard-keys', async () => { await clickText('API keys'); await waitFor(`document.body.innerText.includes('reporting service')`, 'the key list'); }],
  ['dashboard-system', async () => { await clickText('System'); await waitFor(`document.body.innerText.includes('Reclaim')`, 'the system page'); }],
  ['dashboard-files', async () => { await clickText('documents'); await waitFor(`document.body.innerText.includes('msa-meridian.pdf')`, 'the file list'); }],
  ['dashboard-versions', async () => { await evaluate(`document.querySelector('button[aria-label="Versions of legal/msa-meridian.pdf"]').click()`); await waitFor(`document.body.innerText.includes('newest')`, 'the version list'); }],
];
const qs = (o) => '?' + new URLSearchParams(o).toString();
const dashUrl = `http://127.0.0.1:${PROXY}/#key=${KEY}`;
const app = [
  ['app-setup', async () => { await goto(UI + qs({ scenario: 'setup' })); await waitFor(`document.body.innerText.includes('Set up PowerDB')`, 'the welcome step'); }],
  ['app-overview', async () => { await goto(UI + qs({ scenario: 'running', view: 'server', port: 8300 })); await waitFor(`/health/i.test(document.body.innerText)`, 'the overview'); }],
  ['app-data', async () => { await goto(UI + qs({ scenario: 'running', view: 'data', port: PROXY, dash: dashUrl })); await waitFor(`document.querySelector('iframe')`, 'the frame'); await sleep(3500); await evaluate(`window.dispatchEvent(new MessageEvent('message',{origin:'http://127.0.0.1:${PROXY}',data:{powerdb:'dashboard',state:'signed-in'}}))`); await sleep(1500); }],
  ['app-backups', async () => { await goto(UI + qs({ scenario: 'running', view: 'backups', port: 8300 })); await waitFor(`document.body.innerText.includes('Back up now')`, 'backups'); await sleep(600); }],
  ['app-logs', async () => { await goto(UI + qs({ scenario: 'running', view: 'logs', port: 8300 })); await waitFor(`document.body.innerText.includes('Filter lines')||document.querySelector('[role=log]')`, 'logs'); }],
  ['app-settings', async () => { await goto(UI + qs({ scenario: 'running', view: 'settings', port: 8300 })); await waitFor(`document.body.innerText.includes('Administrator key')`, 'settings'); }],
  ['app-palette', async () => { await goto(UI + qs({ scenario: 'running', view: 'server', port: 8300 })); await waitFor(`/health/i.test(document.body.innerText)`, 'the overview'); await evaluate(`window.dispatchEvent(new KeyboardEvent('keydown',{key:'k',metaKey:true,bubbles:true}))`); await waitFor(`document.querySelector('[role=dialog]')`, 'the palette'); }],
];

try {
  await connect();
  await cdp('Page.enable');
  await cdp('Page.setBypassCSP', { enabled: true }); // the app shows the dashboard in a frame, which the server only allows for the real app's own origin
  await cdp('Emulation.setDeviceMetricsOverride', { width: 1440, height: 900, deviceScaleFactor: 2, mobile: false });
  await cdp('Emulation.setEmulatedMedia', { features: [{ name: 'prefers-color-scheme', value: 'dark' }] });
  for (const [name, run] of [...dashboard, ...app]) {
    if (only && !name.includes(only)) continue;
    try { await run(); await shot(name); } catch (e) { console.error(`FAILED ${name}: ${e.message}`); }
  }
} finally {
  chrome.kill();
  proxy.close();
}

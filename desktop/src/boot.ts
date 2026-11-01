// Imported first. If the interface fails in any way, say so on screen and in the app's log, instead of leaving a blank window.
import { invoke } from './api';

export function logUi(level: 'info' | 'error', msg: string): void {
  void invoke('frontend_log', { level, msg }).catch(() => {});
}

export function showFatal(msg: string): void {
  const root = document.getElementById('root');
  if (!root || root.childElementCount > 0) return;
  root.innerHTML = '';
  const box = document.createElement('div');
  box.setAttribute('role', 'alert');
  box.style.cssText = 'padding:32px;font:14px/1.5 system-ui,sans-serif;max-width:640px;margin:10vh auto;color:#fafafa;background:#0a0a0b;border:1px solid #27272d;border-radius:12px';
  const h = document.createElement('div');
  h.textContent = 'PowerDB Manager could not start its interface';
  h.style.cssText = 'font-weight:700;font-size:18px;margin-bottom:8px';
  const p = document.createElement('pre');
  p.textContent = msg;
  p.style.cssText = 'white-space:pre-wrap;color:#f87171;margin:0 0 12px';
  const n = document.createElement('div');
  n.textContent = 'The details are also in frontend.log in the app\'s settings folder. Quit and reopen PowerDB; if it keeps happening, please report it with that file.';
  n.style.color = '#a1a1aa';
  box.append(h, p, n);
  root.append(box);
}

logUi('info', `boot ${location.href}`);
window.addEventListener('error', (e) => { logUi('error', `${e.message} @ ${e.filename}:${e.lineno}`); showFatal(e.message); });
window.addEventListener('unhandledrejection', (e) => { const m = e.reason instanceof Error ? e.reason.message : String(e.reason); logUi('error', `unhandled: ${m}`); showFatal(m); });

// What did it actually draw? (a blank window is otherwise impossible to tell apart from a working one)
window.setTimeout(() => {
  const root = document.getElementById('root');
  const cs = getComputedStyle(document.body);
  logUi('info', `dom: size=${innerWidth}x${innerHeight} visible=${document.visibilityState} rootChildren=${root?.childElementCount} text=${JSON.stringify((document.body.innerText || '').slice(0, 100))} bg=${cs.backgroundColor} color=${cs.color} font=${cs.fontFamily.slice(0, 40)} theme=${document.documentElement.dataset.theme} sheets=${document.styleSheets.length}`);
}, 2500);

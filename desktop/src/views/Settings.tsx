import { useState } from 'react';
import { revealItemInDir } from '@tauri-apps/plugin-opener';
import { Button, Card, CopyButton, Input, Modal, useToast } from '@dash/ui';
import { applyTheme, readTheme, type Theme } from '@dash/theme';
import { api, errorText } from '../api';
import { useApp } from '../app';
import { ErrorLine, Page, Row, Section, Toggle, fileManager } from '../common';

export function Settings() {
  const { snap, refresh } = useApp();
  const toast = useToast();
  const [port, setPort] = useState(String(snap.config.port));
  const [auto, setAuto] = useState(snap.config.auto_start_server);
  const [login, setLogin] = useState(snap.start_at_login);
  const [theme, setTheme] = useState<Theme>(readTheme());
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [key, setKey] = useState<string | null>(null);
  const dirty = Number(port) !== snap.config.port || auto !== snap.config.auto_start_server || login !== snap.start_at_login;

  const save = async () => {
    setBusy(true); setError(null);
    try {
      const p = Number(port);
      if (!Number.isInteger(p) || p < 1024 || p > 65535) throw new Error('The port must be a number from 1024 to 65535.');
      await api.saveSettings(p, auto, login);
      await refresh();
      toast('Settings saved');
    } catch (e) { setError(errorText(e)); } finally { setBusy(false); }
  };

  return (
    <Page title="Settings" actions={<Button variant="primary" disabled={!dirty || busy} onClick={save}>{busy ? 'Saving…' : 'Save changes'}</Button>}>
      <ErrorLine error={error} />
      <Section title="Server">
        <Card className="divide-y divide-line">
          <Row label="Port" hint="The server listens on this computer only (127.0.0.1). Changing it restarts a running server."><Input value={port} onChange={(e) => setPort(e.target.value)} className="w-28" inputMode="numeric" aria-label="Port" /></Row>
          <Row label="Start the server when PowerDB opens" hint="Otherwise use Start Server on the Server page or in the tray menu."><Toggle checked={auto} onChange={setAuto} label="Start the server when PowerDB opens" /></Row>
          <Row label="Open PowerDB when I sign in" hint="Starts hidden in the menu bar or system tray."><Toggle checked={login} onChange={setLogin} label="Open PowerDB at sign-in" /></Row>
        </Card>
      </Section>
      <Section title="Appearance">
        <Card><Row label="Theme" hint="Matches your system by default."><div className="flex gap-1">{(['dark', 'light'] as Theme[]).map((t) => <Button key={t} small variant={theme === t ? 'primary' : 'outline'} onClick={() => { applyTheme(t); setTheme(t); }}>{t === 'dark' ? 'Dark' : 'Light'}</Button>)}</div></Row></Card>
      </Section>
      <Section title="Data and security">
        <Card className="divide-y divide-line">
          <Row label="Data folder" hint={<span className="font-mono">{snap.config.data_dir}</span>}><Button small onClick={() => revealItemInDir(snap.config.data_dir).catch(() => {})}>Show in {fileManager(snap.platform)}</Button></Row>
          <Row label="Administrator key" hint="Stored in your system's credential store. It signs this app in to the dashboard."><Button small onClick={async () => { try { setKey(await api.revealKey()); } catch (e) { setError(errorText(e)); } }}>Reveal…</Button></Row>
        </Card>
      </Section>
      {key !== undefined && key !== null && (
        <Modal title="Administrator key" onClose={() => setKey(null)}>
          <div className="space-y-3 text-sm">
            <p className="text-muted">Anyone with this key can read and change all data. Keep it private.</p>
            <code className="block break-all rounded-md bg-raised p-3">{key}</code>
            <div className="flex justify-end gap-2"><CopyButton text={key} /><Button onClick={() => setKey(null)}>Close</Button></div>
          </div>
        </Modal>
      )}
    </Page>
  );
}

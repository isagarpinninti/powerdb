import { openUrl } from '@tauri-apps/plugin-opener';
import { Button, Card } from '@dash/ui';
import { Logo } from '@dash/Logo';
import { useApp } from '../app';
import { Page } from '../common';

export function About() {
  const { snap } = useApp();
  const rows: [string, string][] = [['Manager', snap.app_version], ['Server', snap.server_version.replace('powerdb ', '')], ['Platform', snap.platform], ['License', 'MIT']];
  return (
    <Page title="About">
      <Card className="flex items-center gap-5 p-6">
        <Logo size={56} />
        <div><div className="text-xl font-semibold">PowerDB</div><div className="text-sm text-muted">One database for records and the files that belong to them.</div></div>
      </Card>
      <Card className="divide-y divide-line">{rows.map(([k, v]) => <div key={k} className="flex justify-between px-5 py-3 text-sm"><span className="text-muted">{k}</span><span className="font-mono">{v}</span></div>)}</Card>
      <div className="flex gap-2">
        <Button onClick={() => openUrl('https://github.com/isagarpinninti/powerdb').catch(() => {})}>Project on GitHub</Button>
        <Button onClick={() => openUrl('https://github.com/isagarpinninti/powerdb/releases').catch(() => {})}>Releases</Button>
      </div>
    </Page>
  );
}

// Copies the built server into src-tauri/binaries under the name Tauri expects (powerdb-<target triple>[.exe]).
// Usage: npm run sidecar [-- path/to/powerdb]   (default: ../target/release/powerdb[.exe])
import { execSync } from 'node:child_process';
import { copyFileSync, mkdirSync, existsSync } from 'node:fs';
import path from 'node:path';

const triple = execSync('rustc -vV').toString().match(/host: (\S+)/)[1];
const exe = process.platform === 'win32' ? '.exe' : '';
const from = process.argv[2] ?? path.resolve('..', 'target', 'release', `powerdb${exe}`);
if (!existsSync(from)) {
  console.error(`server binary not found: ${from}\nBuild it first: cargo build --release   (in the repository root)`);
  process.exit(1);
}
const dir = path.resolve('src-tauri', 'binaries');
mkdirSync(dir, { recursive: true });
copyFileSync(from, path.join(dir, `powerdb-${triple}${exe}`));
console.log(`sidecar: ${from} -> binaries/powerdb-${triple}${exe}`);

# Installing PowerDB

PowerDB is one binary (`powerdb`, about 5 MB, dashboard embedded). Pick one.

> **Verification status.** The test suite passes in CI on Linux (x64, arm64), macOS (arm64, Intel) and Windows.
> The install scripts, the Dockerfile, the compose file, the service files and the release workflow **have not been run**:
> no release exists yet, so treat each as unverified until you have run it.

## Desktop app (macOS, Windows)
**PowerDB** (the desktop app) installs and runs a server on your own computer, with a window and a tray icon instead of a terminal:
set up a database, start and stop it, browse your data, watch the log, and take and restore backups.

- **macOS:** open `PowerDB_<version>_aarch64.dmg` (Apple silicon) or `..._x64.dmg` (Intel) and drag **PowerDB** to Applications.
- **Windows:** run `PowerDB_<version>_x64-setup.exe`. It installs for the current user (no administrator rights) and adds a Start menu entry.

On first start a short wizard creates a database in a folder you choose, picks a port and shows the administrator key once; the key is also
kept in your system's credential store (macOS Keychain, Windows Credential Manager), which is how the app signs the dashboard in for you.
Closing the window leaves the server running in the menu bar or system tray; **Quit** stops it cleanly first. If the app ever crashes, the
server notices and shuts itself down, so a database is never left running unattended.

> **These installers are not signed by Apple or Microsoft.** (The macOS app carries a stable self-signed signature, so the Keychain asks once and remembers: choose **Always Allow**.) Signing needs a paid Apple Developer ID (plus notarization) and a Windows code-signing
> certificate, which this project does not have yet. Expect a warning the first time:
> on macOS, right-click the app and choose **Open** (or allow it under System Settings, Privacy & Security); on Windows, SmartScreen says
> "Windows protected your PC": choose **More info**, then **Run anyway**. You can check a download is the file this project built:
> compare its SHA-256 with `SHA256SUMS` on the release page, or run `gh attestation verify <file> --repo isagarpinninti/powerdb`.
> The app makes no network connections of its own and does not update itself; download a newer installer to upgrade.

The app uses the same `powerdb` server as everything else on this page, so a database made by the app can be served by `powerdb serve`
and the other way round. Use the app for a personal or development database; use a service (below) or Docker for a server others depend on.

## From a release (macOS, Linux)
```bash
POWERDB_REPO=owner/powerdb ./scripts/install.sh          # installs to ~/.local/bin, checks SHA256
```
## From a release (Windows, PowerShell)
```powershell
.\scripts\install.ps1 -Repo owner/powerdb
```
## From source (any platform with Rust ≥ 1.89)
```bash
cargo build --release        # → target/release/powerdb
```
The built dashboard is committed in `engine/ui`; Node is only needed to change the dashboard.
## Docker
```bash
docker build -t powerdb .
docker run -d -p 127.0.0.1:8090:8090 -v powerdb-data:/data powerdb
```
With automatic HTTPS: `POWERDB_DOMAIN=db.example.com docker compose up -d`.

## First run
```bash
powerdb init ./mydata      # creates the data dir, prints the first admin API key once
powerdb serve --data ./mydata
```
Open http://127.0.0.1:8090. `powerdb status` checks a running server.

## Run as a service (starts at boot)
PowerDB installs itself as an operating-system service; no unit files to copy.
```bash
sudo powerdb service install            # Linux: systemd unit; macOS: launch daemon (add --user for a per-user launch agent)
sudo powerdb service status | stop | start | uninstall
```
```powershell
powerdb service install                 # Windows: a Windows Service running as LocalService (run PowerShell as Administrator)
powerdb service status | stop | start | uninstall
```
Options: `--data DIR`, `--addr HOST:PORT`, `--run-as USER` (Linux and macOS), `--print` to show what would be installed without changing anything.
Default data: `/var/lib/powerdb` (Linux), `/usr/local/var/powerdb` (macOS daemon), `%ProgramData%\PowerDB` (Windows). Uninstalling never touches data.
The service runs the binary where it was installed from, so install `powerdb` somewhere stable (for example `/usr/local/bin`) first.
Logs: `journalctl -u powerdb` (Linux), `<data>/powerdb.log` (macOS, Windows).

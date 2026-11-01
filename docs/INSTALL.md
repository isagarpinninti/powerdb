# Installing PowerDB

PowerDB is one binary (`powerdb`, about 5 MB, dashboard embedded). Pick one.

> **Verification status.** The test suite passes in CI on Linux (x64, arm64), macOS (arm64, Intel) and Windows.
> The install scripts, the Dockerfile, the compose file, the service files and the release workflow **have not been run**:
> no release exists yet, so treat each as unverified until you have run it.

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

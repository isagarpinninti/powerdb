# Installing PowerDB

PowerDB is one binary (`powerdb`, about 5 MB, dashboard embedded). Pick one.

> **Verification status.** The code is built and tested on macOS arm64 only. The Linux, Windows and
> macOS-Intel builds, `scripts/install.*`, the Dockerfile and the service files are written but
> **have not been run**; the CI workflow (`.github/workflows/ci.yml`) exists to do that.
> Treat each as unverified until its CI job is green.

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

## Run at boot
| Platform | File |
|---|---|
| Linux (systemd) | `deploy/powerdb.service` → `/etc/systemd/system/`, create user `powerdb`, `systemctl enable --now powerdb` |
| macOS (launchd) | `deploy/com.powerdb.server.plist` → `~/Library/LaunchAgents/`, edit the data path, `launchctl load` |
| Windows | `deploy/windows-service.ps1` (scheduled task at logon; not a true Windows Service) |

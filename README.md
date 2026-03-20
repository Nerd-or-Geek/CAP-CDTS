# CAP-CDTS — Rust backend (JSON storage)

This repository contains the **2.0 UI prototypes** in `2.0/` and a small Rust backend that:

- Serves the UI from `2.0/`
- Exposes a JSON API under `/api/*`
- Persists `users` and `reports` to a single JSON file on disk (`data/store.json`)
- Publishes a lightweight live snapshot over WebSocket at `/ws`

## Run

- `cargo run`
- Open: http://localhost:8080/

The server binds to `0.0.0.0:8080` by default.

You can also build a staged release binary via:

- `make` (same as `make build`)

That produces `bin/cap-cdts-backend.new` which is used by the in-app Update flow.

## Storage

Data is stored in:

- `data/store.json`

If the file does not exist, it will be created on first startup.

An empty reference file is included at:

- `data/store.example.json`

## API (minimal)

- `GET /api/status`
- `GET /api/users`
- `POST /api/users`
- `GET /api/reports`
- `POST /api/reports`
- `GET /api/reports/{num}`
- `DELETE /api/reports/{num}`

GPIO configuration:

- `GET /api/gpio/config`
- `POST /api/gpio/config`

Update (admin token required):

- `GET /api/update/status`
- `POST /api/update/start`

## WebSocket

- `GET /ws` streams a JSON snapshot when data changes.

## GPIO screen

- http://localhost:8080/gpio.html

## Update screen

- http://localhost:8080/update.html

This screen triggers a background update that runs:

- `git pull --ff-only`
- `make build`

On Linux, after a successful build it swaps `bin/cap-cdts-backend.new` into place and (optionally) exits so a supervisor can restart into the new build.

### Security model

The update endpoints execute commands on the host and **must be protected**.

- Set `CAP_CDTS_ADMIN_TOKEN` to enable `/api/update/*`
- The UI sends it in an `X-Admin-Token` header
- If `CAP_CDTS_ADMIN_TOKEN` is not set, update endpoints return `503`

Do not expose the update screen to untrusted networks.

## Environment variables

These can be set in your shell, or in a `.env` file when using systemd’s `EnvironmentFile=`.

- `CAP_CDTS_BIND_ADDR` (default `0.0.0.0:8080`)
- `CAP_CDTS_STORE_PATH` (default `data/store.json`)

Updater:

- `CAP_CDTS_ADMIN_TOKEN` (**required** to enable update endpoints)
- `CAP_CDTS_UPDATE_ENABLED` (default `1`)
- `CAP_CDTS_UPDATE_AUTO_RESTART` (default `1`)
- `CAP_CDTS_REPO_DIR` (default current working directory)
- `CAP_CDTS_UPDATE_NEW_BIN` (default `bin/cap-cdts-backend.new{EXE_SUFFIX}`)
- `CAP_CDTS_UPDATE_LIVE_BIN` (optional; defaults to the current executable path)
- `CAP_CDTS_UPDATE_STATE_PATH` (default `data/update_state.json`)
- `CAP_CDTS_UPDATE_LOG_LINES` (default `400`)

See `.env.example` for a starting point.

## Raspberry Pi deployment (systemd)

These steps assume Raspberry Pi OS (Debian) and a device that will build locally (slower, but simplest). If your repo is private, ensure `git pull` can run non-interactively (SSH deploy key recommended).

### 1) OS prep

1. Update packages:
	- `sudo apt update && sudo apt -y upgrade`
2. Install required tools:
	- `sudo apt install -y git make build-essential curl`

Optional (recommended for later RFID work): enable SPI via `raspi-config`.

### 2) Install Rust (via rustup)

1. Install rustup:
	- `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
2. Load cargo env (or log out/in):
	- `source $HOME/.cargo/env`

### 3) Create an app user

Run as a dedicated user (example: `capcdts`):

- `sudo useradd -r -m -s /usr/sbin/nologin capcdts`

### 4) Clone the repo

Example location:

- `/opt/cap-cdts/app`

Commands:

1. `sudo mkdir -p /opt/cap-cdts`
2. `sudo chown -R capcdts:capcdts /opt/cap-cdts`
3. As the app user:
	- `sudo -u capcdts -H bash`
	- `cd /opt/cap-cdts`
	- `git clone <YOUR_GITHUB_REPO_URL> app`

### 5) Configure environment

1. Copy the example env file:
	- `cd /opt/cap-cdts/app`
	- `cp .env.example .env`
2. Edit `.env` and set a strong token:
	- `CAP_CDTS_ADMIN_TOKEN=<long-random-string>`

### 6) Build and install the first binary

As `capcdts` in `/opt/cap-cdts/app`:

1. `make build`
2. `make promote`

You should now have:

- `bin/cap-cdts-backend` (live)

### 7) Install and start the systemd service

1. Copy the unit file:
	- `sudo cp /opt/cap-cdts/app/deploy/cap-cdts.service /etc/systemd/system/cap-cdts.service`
2. Edit it if your paths/user differ.
3. Reload + enable + start:
	- `sudo systemctl daemon-reload`
	- `sudo systemctl enable cap-cdts.service`
	- `sudo systemctl start cap-cdts.service`
4. Check status/logs:
	- `sudo systemctl status cap-cdts.service`
	- `journalctl -u cap-cdts.service -f`

### 8) Use the Update button

1. Open `http://<pi-hostname-or-ip>:8080/update.html`
2. Paste the same value as `CAP_CDTS_ADMIN_TOKEN`
3. Click **Run update**

What happens:

1. Backend runs `git pull --ff-only`
2. Backend runs `make build` (produces `bin/cap-cdts-backend.new`)
3. Backend swaps the staged binary into place and exits
4. systemd restarts the service into the new build

If the repo has uncommitted changes or Git cannot authenticate, the update will fail and the error will be shown in the Update screen log.


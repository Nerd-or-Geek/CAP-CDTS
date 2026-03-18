# RFID Cyberdeck Rust

A beautiful cyberpunk RFID reader/writer for Raspberry Pi Zero + RC522, with a pure Rust backend (Axum), neon HTML frontend, and one-click self-updater from GitHub releases.

![Cyberpunk](https://img.shields.io/badge/theme-cyberpunk-blueviolet)
![Rust](https://img.shields.io/badge/language-Rust-orange)
![License](https://img.shields.io/badge/license-MIT-green)

---

## Features

✨ **Single Static Binary** — No external files except database  
🚀 **Self-Updating** — One-click update from GitHub releases via `/api/update`  
💜 **Cyberpunk UI** — Neon cyan/purple, dark, fully responsive  
📡 **REST API** — Read/write RFID tags, manage card library  
⚡ **Lightweight** — Optimized for Raspberry Pi Zero (armv6l)  
🔧 **Embedded HTML** — Frontend compiled into binary using `include_str!`  
📦 **Zero Dependencies for Frontend** — Tailwind CDN, no build step  

---

## Quick Start

### Prerequisites

- Rust 1.70+ (install from [rustup.rs](https://rustup.rs))
- Cargo

### Build & Run

```bash
# Clone the repo
git clone https://github.com/Nerd-or-Geek/CAP-CDTS
cd CAP-CDTS

# Build (on any platform)
cargo build --release

# Run
cargo run --release

# Open browser to http://localhost:8080
```

---

## Raspberry Pi Zero Setup

### 1. Enable SPI

```bash
sudo raspi-config
# Interfacing Options → SPI → Enable
```

### 2. Wire RC522 Breakout

| RC522 | Pi Zero |
|-------|---------|
| SDA   | GPIO8 (CE0) |
| SCK   | GPIO11 (SCLK) |
| MOSI  | GPIO10 (MOSI) |
| MISO  | GPIO9 (MISO) |
| GND   | GND |
| 3.3V  | 3.3V |
| RST   | GPIO25 |

[Detailed wiring diagram](https://pimylifeup.com/raspberry-pi-rfid-rc522/)

### 3. Build on Pi Zero

```bash
# Install SQLite dev libraries
sudo apt update
sudo apt install -y libsqlite3-dev pkg-config

# Clone and build
git clone https://github.com/Nerd-or-Geek/CAP-CDTS
cd CAP-CDTS

# Build release (Pi Zero can take a long time the first build)
cargo build --release

# Run with elevated privileges (for GPIO)
sudo ./target/release/rfid-cyberdeck-rust
```

Access at `http://<pi-ip>:8080`

### 4. Run as Systemd Service (Optional)

[Create a systemd service file](#optional-systemd-service)

---

## API Endpoints

### GET `/`

Serves the embedded HTML frontend.

### GET `/api/status`

Returns app version and repo info:

```json
{
  "version": "0.1.0",
  "repo": "Nerd-or-Geek/CAP-CDTS",
  "status": "ok"
}
```

### GET `/api/read`

Reads the current RFID tag:

```json
{
  "uid": "DEADBEEF",
  "text": "Hello, Cyberdeck!",
  "success": true
}
```

### POST `/api/write`

Writes data to an RFID tag:

**Request:**
```json
{
  "text": "New text"
}
```

**Response:**
```json
{
  "uid": "DEADBEEF",
  "text": "New text",
  "success": true
}
```

### GET `/api/cards`

Returns all saved cards from SQLite:

```json
[
  {
    "uid": "DEADBEEF",
    "label": "My Card",
    "text": "Hello, Cyberdeck!"
  }
]
```

### POST `/api/label`

Save a custom label for a card:

**Request:**
```json
{
  "uid": "DEADBEEF",
  "label": "Access Card"
}
```

### POST `/api/update`

Triggers self-update from GitHub releases (returns 202 ACCEPTED immediately):

```bash
curl -X POST http://localhost:8080/api/update
```

---

## GitHub Release & Auto-Build

1. **Create GitHub repository**

   ```bash
   git init
   git add .
   git commit -m "Initial commit"
   git branch -M main
  git remote add origin https://github.com/Nerd-or-Geek/CAP-CDTS.git
   git push -u origin main
   ```

2. **Create GitHub Actions Workflow** (already in `.github/workflows/release.yml`)

3. **Push a release tag**

   ```bash
   git tag v0.1.0
   git push origin v0.1.0
   ```

   The workflow will automatically:
   - Build for `arm-unknown-linux-gnueabihf` (Pi Zero)
   - Create a GitHub Release
   - Upload the binary

4. **Click "Check Update" in the web UI**

   The app will detect the new release and download/install on click.

---

## How the Self-Updater Works

On Linux (Raspberry Pi), the updater prefers a **prebuilt GitHub Release binary** to avoid long compiles on-device:

1. Downloads `https://github.com/<owner>/<repo>/releases/latest/download/rfid-cyberdeck-rust` (via `curl` or `wget`)
2. Atomically swaps the on-disk binary
3. Restarts the app

If no Release asset exists, it falls back to **cloning the repo + building** in the background, then swaps/restarts.

Either way, the currently running version keeps serving requests until the new binary is ready.

Environment variable override:

```bash
export RFID_CYBERDECK_REPO=myorg/myrepo
cargo run --release
```

---

## Project Structure

```
rfid-cyberdeck-rust/
├── Cargo.toml                  # Dependencies & metadata
├── src/
│   └── main.rs                 # Full Axum server + API handlers
├── static/
│   └── index.html              # Embedded HTML frontend
├── .github/
│   └── workflows/
│       └── release.yml         # GitHub Actions for auto-build
├── README.md                   # This file
├── LICENSE                     # MIT
└── .gitignore
```

---

## Configuration

### Repository Owner/Name

By default, the updater looks for `Nerd-or-Geek/CAP-CDTS`. Change it:

1. **Via environment variable:**
   ```bash
   export RFID_CYBERDECK_REPO=myorg/my-repo
   cargo run --release
   ```

2. **Via Cargo.toml:**
   Edit the default in `src/main.rs`:
   ```rust
   let repo = option_env!("RFID_CYBERDECK_REPO")
       .unwrap_or("Nerd-or-Geek/CAP-CDTS")
   ```

---

## Optional: Systemd Service

Create `/etc/systemd/system/rfid-cyberdeck.service`:

```ini
[Unit]
Description=RFID Cyberdeck
After=network.target

[Service]
Type=simple
User=pi
WorkingDirectory=/home/pi/CAP-CDTS
ExecStart=/home/pi/CAP-CDTS/target/release/rfid-cyberdeck-rust
Restart=on-failure
RestartSec=10

[Install]
WantedBy=multi-user.target
```

Enable & start:

```bash
sudo systemctl daemon-reload
sudo systemctl enable rfid-cyberdeck
sudo systemctl start rfid-cyberdeck

# Check logs
sudo journalctl -u rfid-cyberdeck -f
```

---

## Troubleshooting

### GPIO "channel already in use"

- Stop any other GPIO-using processes
- Restart: `sudo systemctl restart rfid-cyberdeck`

### Build fails on Windows

The project is cross-platform but targets ARM. For Pi deployment use:

```bash
cargo install cross
cross build --release --target=arm-unknown-linux-gnueabihf
```

### Database lock errors

The app creates `cards.db` in the current working directory. Ensure proper permissions:

```bash
sudo chown pi:pi cards.db
```

### Updater fails

- Ensure binary is uploaded to **GitHub Releases** (not just tags)
- Name must be exactly `rfid-cyberdeck-rust` (no `.exe` or extensions)
- Verify your repo is **public** for GitHub API access

---

## Development

### Replace Simulated RC522 with Real Hardware

Edit `src/main.rs`, replace the `RfidReader::read()` and `RfidReader::write()` methods:

```rust
// Add to Cargo.toml
// rppal = "0.15"  # for GPIO on Pi
// rc522 = "0.1"   # or similar MFRC522 crate

use rppal::spi::Spi;
use rc522::Rc522;

impl RfidReader {
    fn new() -> Self {
        let spi = Spi::new(rppal::spi::Bus::Spi0, rppal::spi::SlaveSelect::Ss0, 1_000_000, rppal::spi::Mode::Mode0).unwrap();
        let reader = Rc522::new(spi).unwrap();
        // ...
    }

    fn read(&mut self) -> Result<(String, String), String> {
        // Use reader.read_uid() etc.
    }
}
```

### Cross-Compile on Windows

```bash
cargo install cross
cross build --release --target=arm-unknown-linux-gnueabihf
```

---

## License

MIT License — See [LICENSE](LICENSE)

---

## Credits

- **Axum** — Lightweight async web framework
- **Tokio** — Async runtime
- **rusqlite** — SQLite bindings
- **Built-in updater** — GitHub Release binary download (with source-build fallback)
- **Tailwind CSS** — Styling (CDN)
- **Font Awesome** — Icons (CDN)

---

## Contributing

Contributions welcome! Fork, create a feature branch, and submit a PR.

---

## Contact

For issues, suggestions, or questions:  
Open an issue on [GitHub](https://github.com/Nerd-or-Geek/CAP-CDTS/issues)

---

Happy hacking! 🎩⚡

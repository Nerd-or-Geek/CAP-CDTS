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

## WebSocket

- `GET /ws` streams a JSON snapshot when data changes.

## GPIO screen

- http://localhost:8080/gpio.html


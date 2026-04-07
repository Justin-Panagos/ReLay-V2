# ReLay

> Downloads, redefined.

A cross-platform desktop download manager built with Rust and Tauri. ReLay combines fast parallel chunked downloads, full torrent support, and a 7-layer virus scanner (ReLay Shield) backed by a decentralised threat intelligence network on the Internet Computer Protocol (ICP).

---

## Features

### Core
- HTTP/HTTPS downloads with parallel chunking (4 chunks free / 16 Pro)
- HTTP/2 multiplexing via reqwest
- Pause, resume, and cancel at any point
- Magnet link and .torrent file support (DHT + PEX peer discovery)
- UPnP port forwarding for inbound peer connections (port 6881)
- Drag-and-drop URLs and .torrent files

### ReLay Shield
- Layer 1 — Hash reputation (VirusTotal)
- Layer 2 — YARA rules scan
- Layer 3 — Entropy analysis (detects packed/encrypted payloads)
- Layer 4 — File type mismatch detection
- Layer 5 — URL/IP reputation
- Layer 6 — Static binary heuristics
- Layer 7 — Sandbox behavioural analysis (Pro, opt-in)

### Decentralised Threat Network (ICP)
- Signature database synced every 12 hours from ICP Pattern Canister
- Anonymous zero-day submission — help protect other ReLay users
- DAO voting for community threat review (Pro, opt-in)
- Public Threat Intelligence API for developers and researchers

### Pro Tier ($5/month)
- Unlimited simultaneous HTTP downloads
- 16 parallel chunks per download
- No speed cap
- 200 torrent peers
- Browser extension (Chrome + Firefox)
- Sandbox scanning
- Download history: 100 entries (vs 3 free)
- ICP Shield DAO voting rights

---

## Tech Stack

| Layer | Technology |
|---|---|
| Desktop framework | Tauri v1 |
| Backend language | Rust |
| Async runtime | Tokio |
| HTTP client | Reqwest + HTTP/2 |
| Torrent engine | librqbit |
| Local database | SQLite (rusqlite) |
| Virus scanning | yara-rs, goblin, infer, memmap2, sha2 |
| ICP canisters | ic-cdk, ic-agent, candid |
| Payments | Stripe + Cloudflare Workers |
| Frontend | HTML + CSS + Vanilla JS |
| Browser extension | Chrome/Firefox Manifest V3 |
| Build tool | Vite |

---

## Building From Source

### Prerequisites

- Rust (stable, via rustup): `rustup update stable`
- Node.js 20+
- Tauri CLI: `cargo install tauri-cli`

**macOS:** Xcode command line tools (`xcode-select --install`)

**Linux (Debian/Ubuntu):**
```bash
sudo apt install libwebkit2gtk-4.0-dev libssl-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev
```

**Windows:** Microsoft C++ Build Tools (Visual Studio installer)

### Run in Development

```bash
npm install
npm run tauri dev
```

### Build for Release

```bash
npm run tauri build
```

Outputs to `src-tauri/target/release/bundle/`.

---

## Project Structure

```
relay/
├── src-tauri/          # Rust backend
│   └── src/
│       ├── main.rs     # Entry point + Tauri command registration
│       ├── commands/   # Tauri IPC bridge (thin command wrappers)
│       ├── db/         # SQLite layer + migrations
│       ├── download/   # HTTP chunked download engine
│       ├── torrent/    # Torrent engine (librqbit wrapper)
│       ├── shield/     # ReLay Shield scan pipeline
│       ├── icp/        # ICP agent + canister calls
│       └── pro/        # Licence checking + feature gating
├── src/                # Frontend (vanilla HTML/CSS/JS)
│   ├── index.html
│   ├── styles/
│   └── scripts/
├── icp/                # ICP canister source (Rust)
│   ├── pattern/        # Threat signature database
│   ├── governance/     # DAO voting + reputation
│   ├── identity/       # Pro licences + API keys
│   └── update/         # App version tracking
└── extension/          # Browser extension (Chrome + Firefox)
```

---

## Torrent Networking Notes

ReLay uses UPnP to automatically open an inbound port (6881) on your router, which allows peers to connect to you directly and significantly improves download speeds.

**If speeds are still slow, check the following:**

- **Strict NAT or corporate firewall** — UPnP won't work. Manually forward TCP/UDP port 6881 on your router to your machine.
- **macOS firewall** — Allow ReLay through `System Settings → Privacy & Security → Firewall`. Without this, macOS blocks inbound peer connections even if the router is open.
- **Router with UPnP disabled** — Enable UPnP in your router admin panel (usually under Advanced → NAT or WAN settings).
- **Low-seeder torrent** — If the torrent itself has very few seeders, speeds will be limited regardless of network configuration.

---

## Platforms

| Platform | Installer |
|---|---|
| Windows | `ReLay_x64-setup.exe` |
| macOS | `ReLay_x64.dmg` |
| Linux | `ReLay_amd64.AppImage`, `ReLay_amd64.deb` |

---

## Licence

MIT

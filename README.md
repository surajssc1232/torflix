# torflix

A terminal UI for streaming movies from torrents on Linux. Built with
[ratatui](https://ratatui.rs), powered by the [rqbit](https://github.com/ikatson/rqbit)
torrent engine, played through mpv.

```
 ▶ torflix  stream torrents in your terminal  engine: online
┌ torrents ──────────────────────────────────────────────────────────┐
│▶ Big.Buck.Bunny.2008.1080p  ████████░░░░░░░░░░░░  41.3%  3.2 MiB/s │
│  Sintel.2010.4K             ████████████████████ 100.0%  done      │
└─────────────────────────────────────────────────────────────────────┘
 a add  Enter files  Space pause  d remove  D remove+files  q quit
```

## How it works

torflix doesn't implement BitTorrent itself. On startup it looks for a local
rqbit engine on `127.0.0.1:3030`; if none is running it launches one
(`rqbit server start <download-dir>`). rqbit downloads pieces and exposes every
file over a local HTTP endpoint with Range support, so playback can start
after a few seconds of buffering — rqbit prioritizes the pieces the player asks
for. Pressing Enter on a file simply spawns:

```
mpv http://127.0.0.1:3030/torrents/<id>/stream/<file>
```

You can seek freely; mpv issues Range requests and rqbit fetches those pieces.

## Requirements

- `rqbit` in PATH (torrent engine)
- `mpv` in PATH (player) — or any player, see `TORFLIX_PLAYER`
- Rust 1.75+ to build

On NixOS, both are in nixpkgs:

```nix
environment.systemPackages = with pkgs; [ rqbit mpv ];
```

or ad-hoc: `nix-shell -p rqbit mpv cargo`

Elsewhere: `mpv` from your package manager, `rqbit` as a static binary from
its GitHub releases page.

## Build & run

```sh
cargo build --release
./target/release/torflix

# or add something immediately:
./target/release/torflix "magnet:?xt=urn:btih:..."
./target/release/torflix ~/Downloads/movie.torrent
```

## Built-in search (qBittorrent-style)

Press `s`, type a query, Enter. Results from all your indexers appear sorted
by seeders; Enter on a result adds it and starts downloading. Like
qBittorrent's search plugins, torflix doesn't scrape torrent sites itself —
it queries a local **Prowlarr** or **Jackett** instance, and you configure
your indexers there once.

On NixOS the whole thing is two lines:

```nix
services.prowlarr.enable = true;   # web UI on http://127.0.0.1:9696
```

Open the Prowlarr web UI, add your indexers, copy the API key from
Settings → General, then:

```sh
export TORFLIX_PROWLARR_URL=http://127.0.0.1:9696
export TORFLIX_PROWLARR_APIKEY=<your key>
```

Jackett works the same way (`services.jackett.enable = true;`, port 9117):

```sh
export TORFLIX_JACKETT_URL=http://127.0.0.1:9117
export TORFLIX_JACKETT_APIKEY=<your key>
```

If both are set, Prowlarr wins. Only plain http backends are supported
(they're on localhost anyway).

## Keys

| Key            | Action                                          |
|----------------|-------------------------------------------------|
| `s` or `/`     | search torrents (via Prowlarr/Jackett)          |
| `a`            | add torrent (paste magnet link, URL, or path)   |
| `j`/`k`, arrows| move selection                                  |
| `Enter` / `l`  | open file list of selected torrent              |
| `Enter` (files)| play file in mpv                                |
| `p` (files)    | play whole torrent as an mpv playlist           |
| `Enter` (search)| add result & start downloading                 |
| `Space`        | pause / resume torrent                          |
| `d`            | remove torrent, keep downloaded files           |
| `D`            | remove torrent AND delete files                 |
| `Esc` / `h`    | back                                            |
| `q`            | quit (engine keeps running, downloads continue) |
| `Q`            | quit and stop the engine torflix started        |

The file list pre-selects the largest video file — almost always the movie.

## Configuration (env vars)

| Variable               | Default                        | Meaning                       |
|------------------------|--------------------------------|-------------------------------|
| `TORFLIX_DOWNLOAD_DIR` | `~/Videos/torflix`             | where rqbit stores downloads  |
| `TORFLIX_RQBIT_URL`    | `http://127.0.0.1:3030`        | engine API address            |
| `TORFLIX_PLAYER`       | `mpv`                          | player command, e.g. `vlc` or `mpv --fullscreen` |
| `TORFLIX_PROWLARR_URL` + `TORFLIX_PROWLARR_APIKEY` | — | enable search via Prowlarr |
| `TORFLIX_JACKETT_URL` + `TORFLIX_JACKETT_APIKEY`   | — | enable search via Jackett  |

## Notes

- Only download and share content you have the rights to — plenty of great
  material is distributed legitimately over BitTorrent (Blender open movies,
  Linux ISOs, Internet Archive, public-domain films).
- Streaming works best on well-seeded torrents; with few peers, give it a
  minute of buffer before playing.
- `cargo test` includes a live integration test that runs automatically when
  a local rqbit engine is up, and skips otherwise.

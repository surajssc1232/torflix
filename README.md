# torflix

Stream movies and shows in your terminal — search torrents, pick a file, and start watching in seconds.

Powered by [rqbit](https://github.com/ikatson/rqbit) (embedded BitTorrent engine) + mpv/vlc. No separate daemon required.

```
cargo install torflix
```

## Demo

```
 ▶ torflix  stream torrents in your terminal           engine: online
┌──────────────────────────────────────────────────────────────────────┐
│                                                                      │
│   ████████╗ ██████╗ ██████╗ ███████╗██╗     ██╗██╗  ██╗            │
│      ██╔══╝██╔═══██╗██╔══██╗██╔════╝██║     ██║╚██╗██╔╝            │
│      ██║   ██║   ██║██████╔╝█████╗  ██║     ██║ ╚███╔╝             │
│      ██║   ██║   ██║██╔══██╗██╔══╝  ██║     ██║ ██╔██╗             │
│      ██║   ╚██████╔╝██║  ██║██║     ███████╗██║██╔╝ ██╗            │
│      ╚═╝    ╚═════╝ ╚═╝  ╚═╝╚═╝     ╚══════╝╚═╝╚═╝  ╚═╝           │
│                                                                      │
│    › breaking bad s03______                                          │
│                                                                      │
│      Tab: downloads   q: quit (from downloads)                      │
└──────────────────────────────────────────────────────────────────────┘
 Enter: search   Tab: downloads   Esc: clear   ?: help
```

```
 ▶ torflix  stream torrents in your terminal           engine: online
┌ results — 'breaking bad s03' (42) ───────────────────────────────────┐
│ title                          size       seed  leech  imdb  indexer │
│▶ Breaking Bad S03 Complete ...  12.4 GiB   312    28         1337x   │
│  Breaking Bad S03E01 ...         450 MiB   198    12         apibay  │
│  Breaking Bad S03 BluRay ...    8.2 GiB    95     4          Knaben  │
└──────────────────────────────────────────────────────────────────────┘
┌ filter — /: edit   Esc: clear ───────────────────────────────────────┐
│  › bluray                                                            │
└──────────────────────────────────────────────────────────────────────┘
 /: filter   f: files   Enter: stream   d: download   o: sort   s: new search
```

## How it works

torflix embeds [rqbit](https://github.com/ikatson/rqbit) directly — no daemon to install or manage. On startup it spins up a local BitTorrent engine that streams pieces on demand and exposes every file over HTTP with Range support. mpv or vlc connect to that URL and start playing within a few seconds of buffering. When the player exits, temp files are wiped automatically.

## Requirements

| Tool | Role | Required? |
|------|------|-----------|
| **mpv** | Media player | Recommended |
| **vlc** | Media player | Alternative to mpv |
| **Prowlarr** or **Jackett** | More torrent indexers | Optional — built-in search works without them |

Without a player, torrents are downloaded permanently to disk instead.

On **Windows**, VLC and mpv are often not in `PATH`. torflix checks the default install locations automatically (`%PROGRAMFILES%\VideoLAN\VLC\vlc.exe` etc). If detection still fails, set `TORFLIX_PLAYER` to the full path:

```powershell
$env:TORFLIX_PLAYER = "C:\Program Files\VideoLAN\VLC\vlc.exe"
```

## Quick start

```bash
torflix
```

Type your search query and press **Enter**. Results stream in as each indexer responds. Press **Enter** on a result to stream it, **d** to download permanently, or **f** to preview the file list first.

Pass a magnet link, `.torrent` path, or torrent URL directly:

```bash
torflix "magnet:?xt=urn:btih:..."
torflix ~/Downloads/movie.torrent
```

## Key bindings

### Home screen

| Key | Action |
|-----|--------|
| type | build search query |
| `Enter` | search |
| `Esc` | clear query |
| `Ctrl+U` | clear query |
| `Tab` | go to downloads view |
| `?` | toggle help |

### Search results

| Key | Action |
|-----|--------|
| `j` / `k` / `↑` / `↓` | navigate |
| `Enter` / `l` | stream selected result (temp dir, auto-deleted after playback) |
| `d` | download permanently to `~/Downloads/torflix` |
| `f` | preview file list before committing |
| `/` | open filter bar — type to narrow results by title substring |
| `o` | cycle sort: seeders → name → size |
| `s` | new search (back to home) |
| `Esc` / `h` | back to home |

### Filter bar (press `/` in search results)

| Key | Action |
|-----|--------|
| type | narrow results live |
| `Backspace` / `Ctrl+U` | edit / clear filter |
| `Enter` or `/` | close bar, keep filter applied |
| `Esc` | clear filter and close bar |

### File preview panel (press `f` in search results)

Fetches the torrent's file list from peers before you commit to streaming.

| Key | Action |
|-----|--------|
| `j` / `k` | navigate files |
| `Enter` / `l` | stream selected file |
| `d` | download whole torrent to disk |
| `o` | sort files: name → size |
| `f` / `Esc` | close preview |

### Downloads view (press `Tab` from home)

| Key | Action |
|-----|--------|
| `a` | add magnet link, URL, or `.torrent` path |
| `j` / `k` | navigate |
| `Enter` / `l` | open file list |
| `Space` | pause / resume |
| `d` | remove torrent (keep files) |
| `D` | remove torrent and delete files |
| `s` / `Esc` | back to home / search |
| `q` | quit |
| `Q` | quit and stop the rqbit engine |

### Files view

| Key | Action |
|-----|--------|
| `j` / `k` | navigate |
| `Enter` / `l` | stream selected file |
| `p` | play all files as playlist |
| `Esc` / `h` | back |

## Search backends

torflix picks the first available backend automatically:

1. **Prowlarr** — if `TORFLIX_PROWLARR_URL` is set (aggregates dozens of indexers)
2. **Jackett** — if `TORFLIX_JACKETT_URL` is set
3. **Built-in scraper** (default) — searches Knaben, apibay, TorrentsCSV, Nyaa, TorrentGalaxy, and 1337x in parallel. No config required.

All 6 built-in sources run concurrently and results appear as each one finishes.

## Prowlarr setup (optional but recommended)

Prowlarr aggregates dozens of indexers. Once running, torflix uses it automatically.

```bash
# Install (Linux — follow https://wiki.servarr.com/prowlarr/installation for other platforms)
bash <(curl -fsSL https://raw.githubusercontent.com/Servarr/Wiki/master/servarr/servarr-install-script.sh)
```

1. Open `http://127.0.0.1:9696` → **Indexers → Add Indexer**, add what you want
2. Go to **Settings → General**, copy your **API Key**
3. Set env vars:

```bash
# bash/zsh — add to ~/.bashrc or ~/.zshrc
export TORFLIX_PROWLARR_URL="http://127.0.0.1:9696"
export TORFLIX_PROWLARR_APIKEY="your_api_key_here"
```

```fish
# fish — add to ~/.config/fish/config.fish
set -gx TORFLIX_PROWLARR_URL "http://127.0.0.1:9696"
set -gx TORFLIX_PROWLARR_APIKEY "your_api_key_here"
```

## Environment variables

| Variable | Default | Description |
|----------|---------|-------------|
| `TORFLIX_PROWLARR_URL` | — | Prowlarr base URL |
| `TORFLIX_PROWLARR_APIKEY` | — | Prowlarr API key |
| `TORFLIX_JACKETT_URL` | — | Jackett base URL |
| `TORFLIX_JACKETT_APIKEY` | — | Jackett API key |
| `TORFLIX_OMDB_KEY` | — | [OMDb API key](https://www.omdbapi.com/apikey.aspx) for IMDb + RT ratings |
| `TORFLIX_PLAYER` | auto | Player override (e.g. `mpv --fullscreen`) |
| `TORFLIX_DOWNLOAD_DIR` | `~/Downloads/torflix` | Permanent download directory |
| `TORFLIX_RQBIT_URL` | `http://127.0.0.1:3030` | rqbit API URL (if running externally) |

## Ratings (optional)

Set `TORFLIX_OMDB_KEY` to show IMDb and Rotten Tomatoes scores for your search queries.

1. Go to https://www.omdbapi.com/apikey.aspx — choose the **Free** tier
2. Activate the key from your email
3. Set `TORFLIX_OMDB_KEY=your_key_here` in your shell config

## Streaming vs downloading

**With a player (mpv or vlc):**
- Files land in a temp dir under `/tmp/torflix-*`
- Playback starts after a few seconds of buffering
- Temp files are deleted when the player exits

**Without a player:**
- Torrent downloads permanently to `TORFLIX_DOWNLOAD_DIR`
- Track progress in the downloads view (`Tab`)

## Notes

- Only stream content you have the rights to — Blender open movies, Linux ISOs, public-domain films, and legitimately distributed content all work great.
- Well-seeded torrents buffer in a few seconds. Poorly-seeded ones may take longer.
- If your ISP blocks torrent sites (common in India, UK, and others), use a VPN or set up Prowlarr with private indexers.

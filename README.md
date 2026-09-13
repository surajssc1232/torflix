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

Override the download directory for this session with `-d`:

```bash
torflix -d ~/Videos/torrents
torflix -d /mnt/nas/movies "magnet:?xt=urn:btih:..."
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
| `Ctrl+P` | switch what the box searches: torrents ↔ catalog |
| `/` | slash commands (`/browse`, `/favorites`, `/tv`, …) |
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
| `Tab` / `H` | go to history |
| `q` | quit (asks whether to keep downloads going in the background) |
| `Q` | quit and stop background downloads |

### Files view

| Key | Action |
|-----|--------|
| `j` / `k` | navigate |
| `Enter` / `l` | stream selected file |
| `p` | play all files as playlist |
| `Esc` / `h` | back |

### History view (press `Tab` from downloads)

Everything you stream or download is remembered, newest first.

| Key | Action |
|-----|--------|
| `j` / `k` | navigate |
| `Enter` / `l` | stream it again |
| `d` | download permanently |
| `x` | remove from history |
| `Tab` / `Esc` | back to home |

History is stored in `~/.local/share/torflix/history.json` (`%LOCALAPPDATA%\torflix` on Windows).

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
| `TORFLIX_DOWNLOAD_DIR` | `~/Downloads/torflix` | Permanent download directory (also settable per-session with `-d <path>`) |
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

When streaming from a multi-file torrent (a season pack, say), torflix fetches only the episode you're watching, so the rest of the pack doesn't compete with it for peers and bandwidth.

## Catalog mode, subtitles & resume

Press `Ctrl+P` on the home screen to search the **catalog** instead of torrent indexers. It uses [Stremio addons](https://github.com/Stremio/stremio-addon-sdk): titles, posters and ratings come from Cinemeta, and a series opens with its seasons and episodes.

| Key (details view) | Action |
|-----|--------|
| `Enter` | pick a season → episode → play a stream |
| `Tab` / `←` `→` | move between the seasons, episodes and streams panes |
| `f` | star / unstar |
| `d` | download the selected stream |
| `t` | search the torrent indexers for this title instead |

**Streams** come from stream addons you install yourself: `/addons`, then `a` and paste the addon's manifest URL. Torrent streams play through the built-in engine, fetching only the file you picked; direct HTTP streams go straight to the player with any headers the addon requires. torflix doesn't ship with a stream addon.

**Subtitles** are fetched from OpenSubtitles (installed by default, plus any subtitle addons you add) in your chosen language, while the video is still loading. A subtitle is only in sync with the release it was timed to, so torflix ranks them: same release group first, then the same kind of source (BluRay rips together, WEB-DL and WEBRip together), then the 23.976 fps that BluRay and WEB releases use. Subtitles made for cam/telesync copies go last. mpv gets the best few, with the top one selected:

| Key in mpv | |
|-----|--|
| `j` | next subtitle, if the current one is out of sync |
| `z` / `x` | shift subtitle timing earlier / later |

Sometimes no properly timed subtitle exists — early on, OpenSubtitles often only has cam-copy subtitles for a movie — and shifting with `z`/`x` is the only fix. VLC gets just the best match. Change the language or turn subtitles off in `/settings`.

**Resume**: when you watch in mpv, torflix remembers where you stopped and picks up from there next time — for catalog streams, torrent search results and files in the downloads view alike. VLC resumes from positions recorded in mpv but can't record them itself.

### Commands

Type these in the home search box:

| Command | |
|---------|--|
| `/browse` | popular and top-rated movies & series (`b` cycles lists) |
| `/favorites` | titles you've starred |
| `/history` | everything you've watched or downloaded |
| `/downloads` | torrents in progress |
| `/tv` | live TV from M3U playlists — `a` add a URL or file, `/` filter, `m` manage |
| `/addons` | install, enable or remove Stremio addons |
| `/settings` | player, subtitle language, download folder, default search |
| `/help`, `/quit` | |

Settings live in `~/.config/torflix/` (`config.json`, `addons.json`, `tv.json`); favorites, history and resume positions in `~/.local/share/torflix/`. Environment variables such as `TORFLIX_PLAYER` still take precedence.

## Background downloads

Downloads survive quitting. Press `q` with downloads still running and torflix asks:

- **`y`** — keep downloading in the background. A headless engine takes over, and stops itself once everything has finished.
- **`n`** — quit now. Unfinished downloads pause and resume where they left off the next time you open torflix.

Open torflix again while background downloads are running and it connects to them, so you can keep watching progress. To stop them:

```bash
torflix --stop
```

or press `Q` in the downloads view.

## Notes

- Only stream content you have the rights to — Blender open movies, Linux ISOs, public-domain films, and legitimately distributed content all work great.
- Well-seeded torrents buffer in a few seconds. Poorly-seeded ones may take longer.
- If your ISP blocks torrent sites (common in India, UK, and others), use a VPN or set up Prowlarr with private indexers.

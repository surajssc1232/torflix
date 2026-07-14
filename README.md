# torflix

Stream movies in your terminal from magnet links and torrents — powered by [rqbit](https://github.com/ikatson/rqbit) + mpv/vlc.

Browse popular movies from Letterboxd, search via built-in YTS (no account or setup required), paste any magnet link, and start watching in seconds. Nothing is kept on disk after playback ends.

```
 ▶ torflix  stream torrents in your terminal  engine: online
┌ Letterboxd — Popular this week ──────────────────────────────────────┐
│▶   1. Sinners (2025)  ★ 4.1                                          │
│    2. Interstellar (2014)  ★ 4.5                                     │
│    3. Parasite (2019)  ★ 4.5                                         │
│    4. Whiplash (2014)  ★ 4.4                                         │
└──────────────────────────────────────────────────────────────────────┘
 IMDb: 7.8/10   RT: 89%
 s search  Enter find torrent  Tab/→ next list  r refresh  j/k  Esc back
```

## How it works

torflix auto-starts a local [rqbit](https://github.com/ikatson/rqbit) engine on startup. rqbit downloads torrent pieces and exposes every file over a local HTTP endpoint with Range support — so mpv or vlc can start playing within a few seconds of buffering without downloading the whole file. When you quit the player, all temp files are wiped automatically.

## Requirements

| Tool | Role | Required? |
|------|------|-----------|
| **rqbit** | BitTorrent engine (auto-started) | Yes — [download here](https://github.com/ikatson/rqbit/releases) |
| **mpv** | Media player | Recommended |
| **vlc** | Media player | Alternative to mpv |

If neither mpv nor vlc is found, the torrent is downloaded permanently to disk instead of streamed.

## Installation

```bash
git clone <repo>
cd torflix
cargo build --release
cp target/release/torflix ~/.local/bin/
```

Or run directly:

```bash
cargo run --release
```

## Quick start

```bash
torflix
```

torflix opens on the **Popular this week** list. Press `Enter` on any film to find a torrent and stream it, or press `s` to type a search query.

You can also pass a magnet link, `.torrent` path, or torrent URL directly:

```bash
torflix "magnet:?xt=urn:btih:..."
torflix ~/Downloads/movie.torrent
```

## Key bindings

### Browse view (default on startup)

| Key | Action |
|-----|--------|
| `j` / `k` | Move down / up |
| `Enter` / `l` | Search for selected film and stream it |
| `s` or `/` | Type a custom search query |
| `Tab` / `→` | Next list: this week → this month → all-time → top rated |
| `Shift+Tab` / `←` | Previous list |
| `r` | Refresh current list |
| `Esc` / `h` | Go to torrents view |
| `q` | Quit |

### Torrents view

| Key | Action |
|-----|--------|
| `a` | Add magnet link, URL, or `.torrent` path |
| `s` or `/` | Search movies |
| `b` | Back to browse |
| `Enter` / `l` | Open file list |
| `Space` | Pause / resume torrent |
| `d` | Remove torrent (keep files on disk) |
| `D` | Remove torrent and delete all files |
| `q` | Quit (downloads/streams keep running) |
| `Q` | Quit and stop the rqbit engine |

### Files view

| Key | Action |
|-----|--------|
| `Enter` / `l` | Stream selected file |
| `p` | Play all files as a playlist |
| `Esc` / `h` | Back |

### Search results

| Key | Action |
|-----|--------|
| `Enter` / `l` | Stream selected result |
| `s` or `/` | New search |
| `Esc` / `h` | Back to where you came from |

## Environment variables

| Variable | Default | Description |
|----------|---------|-------------|
| `TORFLIX_PLAYER` | auto | Override player (e.g. `mpv --fullscreen`) |
| `TORFLIX_DOWNLOAD_DIR` | `~/Videos/torflix` | Download directory (used when no player found) |
| `TORFLIX_RQBIT_URL` | `http://127.0.0.1:3030` | rqbit API URL |
| `TORFLIX_OMDB_KEY` | — | [Free OMDb key](https://www.omdbapi.com/apikey.aspx) for IMDb + RT ratings |
| `TORFLIX_PROWLARR_URL` | — | Prowlarr base URL for extended search |
| `TORFLIX_PROWLARR_APIKEY` | — | Prowlarr API key |
| `TORFLIX_JACKETT_URL` | — | Jackett base URL for extended search |
| `TORFLIX_JACKETT_APIKEY` | — | Jackett API key |

## Search backends

torflix picks the first available backend automatically:

1. **Prowlarr** — if `TORFLIX_PROWLARR_URL` is set (all your configured indexers)
2. **Jackett** — if `TORFLIX_JACKETT_URL` is set
3. **YTS** — built-in, no setup needed, movies only, includes IMDb ratings

## Ratings

- **Letterboxd** star ratings (★) are always shown in the browse list
- **IMDb + Rotten Tomatoes** appear in the status bar when browsing or searching — set `TORFLIX_OMDB_KEY` to enable (free API key from [omdbapi.com](https://www.omdbapi.com/apikey.aspx))
- **YTS IMDb ratings** appear in the `imdb` column of search results with no key needed

## Streaming vs downloading

When a player (mpv or vlc) is available:
- Files go to a temp dir under `/tmp/torflix-*`
- Playback starts after a few seconds of buffering
- All temp files are deleted automatically when the player exits

When no player is found:
- Torrent is added to rqbit and downloads permanently to `TORFLIX_DOWNLOAD_DIR`
- The torrent appears in the torrents list and you can track progress there

## Notes

- Only stream and share content you have the rights to — Blender open movies, Linux ISOs, Internet Archive, public-domain films, and legitimately distributed content all work great here.
- Well-seeded torrents start instantly. Poorly-seeded ones may take a minute to buffer.

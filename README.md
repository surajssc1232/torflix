# torflix

Stream movies in your terminal from magnet links and torrents — powered by [rqbit](https://github.com/ikatson/rqbit) + mpv/vlc.

Browse popular movies from Letterboxd, search torrents out of the box (no Prowlarr required), paste any magnet link, and start watching in seconds. Nothing is kept on disk after playback ends.

`cargo install torflix` — that's the whole setup.

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

torflix embeds [rqbit](https://github.com/ikatson/rqbit) directly — no separate install needed. On startup it spins up a local BitTorrent engine that downloads torrent pieces and exposes every file over a local HTTP endpoint with Range support, so mpv or vlc can start playing within a few seconds of buffering without downloading the whole file. When you quit the player, all temp files are wiped automatically.

## Requirements

| Tool | Role | Required? |
|------|------|-----------|
| **mpv** | Media player | Recommended |
| **vlc** | Media player | Alternative to mpv |
| **Prowlarr** or **Jackett** | More torrent sources | Optional — built-in search works without them |

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

## Search backends

torflix picks the first available backend automatically:

1. **Prowlarr** — if `TORFLIX_PROWLARR_URL` is set (aggregates dozens of indexers)
2. **Jackett** — if `TORFLIX_JACKETT_URL` is set
3. **Built-in scraper** (default) — uses apibay.org (Pirate Bay JSON API) with TorrentGalaxy as fallback. No config required.

The built-in scraper is enough for most movies and shows. If your ISP blocks torrent sites at the DNS/network level (common in India, UK, and many other countries), you'll see "network may be blocking torrent sites" and should either use a VPN or set up Prowlarr.

## Optional: Prowlarr for more sources

Prowlarr is a free, self-hosted app that connects to dozens of torrent indexers at once. Once running, torflix uses it automatically instead of the built-in scraper.

### 1. Install Prowlarr

Follow the official guide for your OS: https://wiki.servarr.com/prowlarr/installation

On Linux the quickest way is via the install script:
```bash
bash <(curl -fsSL https://raw.githubusercontent.com/Servarr/Wiki/master/servarr/servarr-install-script.sh)
```
Pick **Prowlarr** when prompted. It will run as a service on `http://127.0.0.1:9696`.

### 2. Open Prowlarr and add indexers

1. Go to `http://127.0.0.1:9696` in your browser
2. Click **Indexers → Add Indexer**
3. Search for and add the indexers you want (e.g. "1337x", "RARBG", "YTS", "EZTV")
4. Click **Test** on each one to confirm they work

### 3. Get your API key

1. In Prowlarr, go to **Settings → General**
2. Copy the value under **API Key** (looks like `a1b2c3d4e5f6...`)

### 4. Set the environment variables

**bash / zsh** — add to your `~/.bashrc` or `~/.zshrc`:
```bash
export TORFLIX_PROWLARR_URL="http://127.0.0.1:9696"
export TORFLIX_PROWLARR_APIKEY="your_api_key_here"
```
Then reload: `source ~/.bashrc`

**fish** — add to your `~/.config/fish/config.fish`:
```fish
set -gx TORFLIX_PROWLARR_URL "http://127.0.0.1:9696"
set -gx TORFLIX_PROWLARR_APIKEY "your_api_key_here"
```
Then reload: `source ~/.config/fish/config.fish`

### 5. Run torflix and search

```bash
torflix
```
Press `s`, type a movie name, hit Enter. Results come from all your configured indexers.

---

## Alternative: Jackett

Jackett is similar to Prowlarr. Use it if you already have it set up.

### Get your Jackett API key

1. Open `http://127.0.0.1:9117` in your browser
2. The **API Key** is shown at the top of the page
3. Add indexers via **Add indexer**

### Set the variables

**bash / zsh:**
```bash
export TORFLIX_JACKETT_URL="http://127.0.0.1:9117"
export TORFLIX_JACKETT_APIKEY="your_api_key_here"
```

**fish:**
```fish
set -gx TORFLIX_JACKETT_URL "http://127.0.0.1:9117"
set -gx TORFLIX_JACKETT_APIKEY "your_api_key_here"
```

---

## Key bindings

### Browse view (default on startup)

| Key | Action |
|-----|--------|
| `j` / `k` | Move down / up |
| `Enter` / `l` | Search for selected film and stream it |
| `s` or `/` | Type a custom search query |
| `Tab` / `→` | Next list: this week → this month → all-time → top rated |
| `Shift+Tab` / `←` | Previous list |
| `]` | Next page (72 films per page) |
| `[` | Previous page |
| `r` | Refresh current page |
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
| `Enter` / `l` | Stream selected result (temp dir, deleted after playback) |
| `d` | Download selected result permanently to `~/Downloads/torflix` |
| `o` | Cycle sort: seeders → name → size (name-sort groups S01E01/E02/E03 together) |
| `s` or `/` | New search |
| `Esc` / `h` | Back to where you came from |

## Environment variables

| Variable | Default | Description |
|----------|---------|-------------|
| `TORFLIX_PROWLARR_URL` | — | Prowlarr base URL (e.g. `http://127.0.0.1:9696`) |
| `TORFLIX_PROWLARR_APIKEY` | — | Prowlarr API key (Settings → General → API Key) |
| `TORFLIX_JACKETT_URL` | — | Jackett base URL (e.g. `http://127.0.0.1:9117`) |
| `TORFLIX_JACKETT_APIKEY` | — | Jackett API key (shown at top of Jackett UI) |
| `TORFLIX_OMDB_KEY` | — | [Free OMDb key](https://www.omdbapi.com/apikey.aspx) for IMDb + RT ratings |
| `TORFLIX_PLAYER` | auto | Override player (e.g. `mpv --fullscreen`) |
| `TORFLIX_DOWNLOAD_DIR` | `~/Videos/torflix` | Download directory (used when no player found) |
| `TORFLIX_RQBIT_URL` | `http://127.0.0.1:3030` | rqbit API URL |

## Ratings

- **Letterboxd** star ratings (★) are always shown in the browse list
- **IMDb + Rotten Tomatoes** appear in the status bar when browsing or searching — set `TORFLIX_OMDB_KEY` to enable

To get a free OMDb API key:
1. Go to https://www.omdbapi.com/apikey.aspx
2. Choose the **Free** tier and enter your email
3. Check your email for the key and activate it
4. Set `TORFLIX_OMDB_KEY=your_key_here` in your shell config

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

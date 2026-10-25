use crate::app::{human_bytes, is_video, poster_url, progress_size, HomeFocus, App, Details, FileSortMode, InputPurpose, Load, Pane, PreviewState, SearchPreview, SearchStatus, SortMode, View};
use crate::history;
use crate::posters;
use crate::stremio::MetaItem;
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Clear, List, ListItem, ListState, Paragraph, Row, Table, TableState, Wrap},
    Frame,
};

// gruvbox-ish palette
const BG: Color = Color::Rgb(0x28, 0x28, 0x28);
const FG: Color = Color::Rgb(0xeb, 0xdb, 0xb2);
const YELLOW: Color = Color::Rgb(0xd7, 0x99, 0x21);
const GREEN: Color = Color::Rgb(0x98, 0x97, 0x1a);
const RED: Color = Color::Rgb(0xcc, 0x24, 0x1d);
const AQUA: Color = Color::Rgb(0x68, 0x9d, 0x6a);
const GRAY: Color = Color::Rgb(0x92, 0x83, 0x74);
const ORANGE: Color = Color::Rgb(0xd6, 0x5d, 0x0e);

pub fn draw(f: &mut Frame, app: &App) {
    let area = f.size();
    f.render_widget(Block::default().style(Style::default().bg(BG).fg(FG)), area);

    // Below this, bordered widgets land outside the buffer and ratatui 0.26
    // panics, so say so instead of drawing.
    if area.width < 20 || area.height < 14 {
        if area.width > 0 && area.height > 0 {
            f.render_widget(
                Paragraph::new(Span::styled("terminal too small", Style::default().fg(YELLOW))),
                area,
            );
        }
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // title bar
            Constraint::Min(3),    // main content
            Constraint::Length(1), // status
            Constraint::Length(1), // help
        ])
        .split(area);

    draw_title(f, chunks[0], app);

    // Popups float over whatever screen opened them.
    let base = match app.view {
        View::ConfirmQuit => &app.quit_return_view,
        View::AddInput => &app.input_return,
        _ => &app.view,
    };
    match base {
        View::Home        => draw_home(f, chunks[1], app),
        View::Files       => draw_files(f, chunks[1], app),
        View::SearchResults => draw_search_results(f, chunks[1], app),
        View::History     => draw_history(f, chunks[1], app),
        View::Discover    => draw_discover(f, chunks[1], app),
        View::Details     => draw_details(f, chunks[1], app),
        View::Tv          => draw_tv(f, chunks[1], app),
        View::Addons      => draw_addons(f, chunks[1], app),
        View::Settings    => draw_settings(f, chunks[1], app),
        _                 => draw_torrents(f, chunks[1], app),
    }

    draw_status(f, chunks[2], app);
    draw_help(f, chunks[3], app);

    if app.view == View::AddInput {
        draw_add_popup(f, area, app);
    }
    if app.view == View::ConfirmDelete {
        draw_confirm_popup(f, area, app);
    }
    if app.view == View::ConfirmQuit {
        draw_quit_popup(f, area, app);
    }
    if app.show_help {
        draw_help_popup(f, area);
    }
}

fn draw_title(f: &mut Frame, area: Rect, app: &App) {
    let engine = if *app.engine_up.lock().unwrap() {
        let label = if app.embedded { " engine: online " } else { " engine: background " };
        Span::styled(label, Style::default().fg(GREEN))
    } else {
        Span::styled(" engine: OFFLINE ", Style::default().fg(RED).add_modifier(Modifier::BOLD))
    };
    let line = Line::from(vec![
        Span::styled(
            " ▶ torflix ",
            Style::default().fg(BG).bg(YELLOW).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" stream torrents in your terminal ", Style::default().fg(GRAY)),
        engine,
    ]);
    f.render_widget(Paragraph::new(line), area);
}


fn draw_home(f: &mut Frame, area: Rect, app: &App) {
    let searching = app.home_focus == HomeFocus::Search;
    let bar_height = 3u16; // border + 1 content row + border

    // Search box across the top, covers filling what is left below it.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(bar_height), Constraint::Min(0)])
        .split(area);

    let bar_rect = Rect {
        x: area.x + 1,
        y: chunks[0].y,
        width: area.width.saturating_sub(2),
        height: bar_height,
    };

    let inner_w = bar_rect.width.saturating_sub(6) as usize;
    let q = &app.search_query;
    let shown: String = if q.chars().count() > inner_w {
        q.chars().skip(q.chars().count() - inner_w).collect()
    } else {
        q.clone()
    };

    let p = Paragraph::new(vec![Line::from(if q.is_empty() {
        vec![
            Span::styled("  › ", Style::default().fg(AQUA).add_modifier(Modifier::BOLD)),
            Span::styled(
                if searching {
                    "search for a movie or show…  (/ for commands, ↓ for the covers)"
                } else {
                    "↑ or any letter comes back here to search"
                },
                Style::default().fg(GRAY),
            ),
        ]
    } else {
        vec![
            Span::styled("  › ", Style::default().fg(AQUA).add_modifier(Modifier::BOLD)),
            Span::styled(shown, Style::default().fg(FG)),
            Span::styled(if searching { "█" } else { "" }, Style::default().fg(AQUA)),
        ]
    })])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(
                format!(" {} · Ctrl+P ", app.config.search_source.label()),
                Style::default().fg(YELLOW),
            ))
            .border_style(Style::default().fg(if searching { AQUA } else { GRAY })),
    );
    f.render_widget(p, bar_rect);

    // Slash-command suggestions take the place of the covers while typing "/…".
    let suggestions = crate::commands::Command::suggest(q);
    if !suggestions.is_empty() {
        let lines: Vec<Line> = suggestions
            .iter()
            .take(chunks[1].height as usize)
            .map(|c| {
                Line::from(vec![
                    Span::styled(format!("  {:<11}", c.name()), Style::default().fg(YELLOW)),
                    Span::styled(c.description(), Style::default().fg(GRAY)),
                ])
            })
            .collect();
        f.render_widget(Paragraph::new(lines), chunks[1]);
        return;
    }

    draw_home_browse(f, chunks[1], app);
}

/// The covers under the search box: whichever browse list is showing.
fn draw_home_browse(f: &mut Frame, area: Rect, app: &App) {
    if area.height < 4 {
        return;
    }
    let focused = app.home_focus == HomeFocus::Browse;
    let items = app.home_items();
    let note = match &*app.home.lock().unwrap() {
        Load::Ready(v) if v.is_empty() => Some(("nothing to show".to_string(), GRAY)),
        Load::Failed(e) => Some((format!("✗ {e}"), RED)),
        Load::Idle => Some(("".to_string(), GRAY)),
        Load::Loading => Some(("loading…".to_string(), YELLOW)),
        Load::Ready(_) => None,
    };

    let title = Line::from(vec![
        Span::styled(
            format!("  {}  ", if app.home_title.is_empty() { "browse" } else { &app.home_title }),
            Style::default().fg(YELLOW).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            if focused { "Enter: open   f: star   Tab: next list   ↑: search   " } else { "↓: browse   " },
            Style::default().fg(GRAY),
        ),
        Span::styled("◆ Letterboxd", Style::default().fg(GREEN)),
    ]);
    f.render_widget(Paragraph::new(title), Rect { height: 1, ..area });

    let grid = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(1),
    };
    if let Some((msg, color)) = note {
        f.render_widget(
            Paragraph::new(Span::styled(msg, Style::default().fg(color))),
            Rect { height: 1, ..grid },
        );
        return;
    }
    let cols = draw_cover_grid(f, grid, app, &items, app.home_selected, focused);
    app.grid_cols.set(cols);
}

/// Cells across for one cover. Narrow enough that a modest terminal still
/// fits several, wide enough that the picture reads.
const COVER_W: u16 = 16;

/// A grid of covers with their titles. Returns the number of columns used, so
/// ↑/↓ can move a whole row at a time.
fn draw_cover_grid(
    f: &mut Frame,
    area: Rect,
    app: &App,
    items: &[MetaItem],
    selected: usize,
    focused: bool,
) -> usize {
    let cover_h = posters::height_for(COVER_W);
    let tile_h = cover_h + 2; // cover, title, year · rating
    let (stride_x, stride_y) = (COVER_W + 2, tile_h + 1);
    let cols = (area.width / stride_x).max(1) as usize;
    let rows = (area.height / stride_y).max(1) as usize;

    // Scroll by whole rows, keeping the selected cover on screen.
    let sel_row = selected / cols;
    let first_row = sel_row.saturating_sub(rows.saturating_sub(1));

    for slot in 0..rows * cols {
        let idx = (first_row + slot / cols) * cols + slot % cols;
        let Some(item) = items.get(idx) else { break };
        let x = area.x + (slot % cols) as u16 * stride_x;
        let y = area.y + (slot / cols) as u16 * stride_y;
        if x + COVER_W > area.x + area.width || y + tile_h > area.y + area.height {
            continue;
        }
        let picked = idx == selected;
        draw_cover(f, Rect { x, y, width: COVER_W, height: cover_h }, app, item, picked && focused);

        let title_style = if picked {
            Style::default().fg(if focused { BG } else { YELLOW }).bg(if focused { YELLOW } else { BG })
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(FG)
        };
        f.render_widget(
            Paragraph::new(Span::styled(clip(&item.name, COVER_W as usize), title_style)),
            Rect { x, y: y + cover_h, width: COVER_W, height: 1 },
        );
        // year · Letterboxd out of five, each in its own colour.
        let mut sub = vec![Span::styled(
            clip(&item.year_label(), 5),
            Style::default().fg(GRAY),
        )];
        if let Some(lb) = app.rating(&item.id) {
            sub.push(Span::styled(format!("  ◆{:.1}", lb.score), Style::default().fg(GREEN)));
        }
        f.render_widget(
            Paragraph::new(Line::from(sub)),
            Rect { x, y: y + cover_h + 1, width: COVER_W, height: 1 },
        );
    }
    cols
}

/// One cover, as half-block rows. Until it has been fetched and decoded — or
/// when there is no picture to be had — a muted panel with the title stands in.
fn draw_cover(f: &mut Frame, area: Rect, app: &App, item: &MetaItem, picked: bool) {
    let cover = poster_url(item).and_then(|u| app.poster(&u, area.width, area.height));
    match cover {
        Some(p) => {
            let lines: Vec<Line> = p
                .rows
                .iter()
                .map(|row| {
                    Line::from(
                        row.iter()
                            .map(|((tr, tg, tb), (br, bg, bb))| {
                                Span::styled(
                                    "▀",
                                    Style::default()
                                        .fg(Color::Rgb(*tr, *tg, *tb))
                                        .bg(Color::Rgb(*br, *bg, *bb)),
                                )
                            })
                            .collect::<Vec<_>>(),
                    )
                })
                .collect();
            f.render_widget(Paragraph::new(lines), area);
        }
        None => {
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(if picked { YELLOW } else { GRAY }));
            f.render_widget(
                Paragraph::new(Span::styled(item.name.clone(), Style::default().fg(GRAY)))
                    .wrap(Wrap { trim: true })
                    .alignment(Alignment::Center)
                    .block(block),
                area,
            );
        }
    }
    if picked {
        // A bar down the left edge, so the pick is obvious even over a picture.
        let marks: Vec<Line> = (0..area.height)
            .map(|_| Line::from(Span::styled("▌", Style::default().fg(YELLOW))))
            .collect();
        f.render_widget(Paragraph::new(marks), Rect { width: 1, ..area });
    }
}

/// Cut to `width` columns, with an ellipsis when something had to go.
fn clip(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        return s.to_string();
    }
    let keep = width.saturating_sub(1);
    format!("{}…", s.chars().take(keep).collect::<String>())
}

fn progress_bar_spans(pct: f64, width: usize) -> Line<'static> {
    let filled = ((pct / 100.0) * width as f64).round() as usize;
    let filled = filled.min(width);
    let bar_color = if pct >= 100.0 { AQUA } else { GREEN };
    Line::from(vec![
        Span::styled("█".repeat(filled), Style::default().fg(bar_color)),
        Span::styled("░".repeat(width - filled), Style::default().fg(GRAY)),
        Span::styled(
            format!(" {:>5.1}%", pct),
            Style::default().fg(YELLOW).add_modifier(Modifier::BOLD),
        ),
    ])
}

fn draw_torrents(f: &mut Frame, area: Rect, app: &App) {
    let rows_data = app.rows_snapshot();

    if rows_data.is_empty() {
        let msg = Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled(
                "no torrents yet",
                Style::default().fg(GRAY).add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "press 'a' to add a magnet link, or Esc to go back to search",
                Style::default().fg(GRAY),
            )),
        ])
        .alignment(Alignment::Center)
        .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(GRAY)));
        f.render_widget(msg, area);
        return;
    }

    let rows: Vec<Row> = rows_data
        .iter()
        .map(|t| {
            let (pct, size, speed, peers, eta, state) = match &t.stats {
                Some(s) => {
                    let state_str = if s.finished { "done".to_string() } else { s.state.clone() };
                    (
                        s.progress_pct(),
                        progress_size(s.progress_bytes, s.total_bytes),
                        s.down_speed(),
                        s.peers().to_string(),
                        s.eta().unwrap_or_else(|| "-".into()),
                        state_str,
                    )
                }
                None => (0.0, "-".into(), "-".into(), "-".into(), "-".into(), "…".into()),
            };
            let state_style = match state.as_str() {
                "live"   => Style::default().fg(GREEN),
                "done"   => Style::default().fg(AQUA),
                "paused" => Style::default().fg(YELLOW),
                "error"  => Style::default().fg(RED),
                _        => Style::default().fg(GRAY),
            };
            Row::new(vec![
                ratatui::widgets::Cell::from(t.name.clone()),
                ratatui::widgets::Cell::from(progress_bar_spans(pct, 14)),
                ratatui::widgets::Cell::from(size),
                ratatui::widgets::Cell::from(speed),
                ratatui::widgets::Cell::from(peers),
                ratatui::widgets::Cell::from(eta),
                ratatui::widgets::Cell::from(state).style(state_style),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Min(24),
            Constraint::Length(22), // 14 bar + space + 6 for " xx.x%"
            Constraint::Length(18), // "900.0 MiB/2.0 GiB"
            Constraint::Length(12),
            Constraint::Length(6),
            Constraint::Length(12),
            Constraint::Length(8),
        ],
    )
    .header(
        Row::new(vec!["name", "progress", "size", "speed", "peers", "eta", "state"])
            .style(Style::default().fg(YELLOW).add_modifier(Modifier::BOLD)),
    )
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(" downloads ")
            .border_style(Style::default().fg(GRAY)),
    )
    .highlight_style(Style::default().bg(Color::Rgb(0x3c, 0x38, 0x36)).add_modifier(Modifier::BOLD))
    .highlight_symbol("▶ ");

    let mut state = TableState::default();
    state.select(Some(app.selected.min(rows_data.len().saturating_sub(1))));
    f.render_stateful_widget(table, area, &mut state);
}

fn draw_files(f: &mut Frame, area: Rect, app: &App) {
    let items: Vec<ListItem> = app
        .files
        .iter()
        .map(|file| {
            let video = is_video(&file.name);
            let icon = if video { "▶ " } else { "  " };
            let style = if video { Style::default().fg(FG) } else { Style::default().fg(GRAY) };
            ListItem::new(Line::from(vec![
                Span::styled(icon, Style::default().fg(ORANGE)),
                Span::styled(file.name.clone(), style),
                Span::styled(format!("  ({})", human_bytes(file.length)), Style::default().fg(GRAY)),
            ]))
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" files — {} ", app.files_torrent_name))
                .border_style(Style::default().fg(GRAY)),
        )
        .highlight_style(Style::default().bg(Color::Rgb(0x3c, 0x38, 0x36)).add_modifier(Modifier::BOLD))
        .highlight_symbol("→ ");

    let mut state = ListState::default();
    state.select(Some(app.file_selected.min(app.files.len().saturating_sub(1))));
    f.render_stateful_widget(list, area, &mut state);
}

fn draw_status(f: &mut Frame, area: Rect, app: &App) {
    let (text, style) = if app.view == View::SearchResults {
        let r = app.search_ratings_line();
        if !r.is_empty() { (r, Style::default().fg(AQUA)) } else { status_style(&app.status) }
    } else {
        status_style(&app.status)
    };
    f.render_widget(
        Paragraph::new(Span::styled(format!(" {}", text), style)),
        area,
    );
}

fn status_style(status: &str) -> (String, Style) {
    let style = if status.starts_with('✗') {
        Style::default().fg(RED)
    } else if status.starts_with('✓') || status.starts_with('▶') {
        Style::default().fg(GREEN)
    } else {
        Style::default().fg(GRAY)
    };
    (status.to_string(), style)
}

fn draw_help(f: &mut Frame, area: Rect, app: &App) {
    let help: &str = match app.view {
        View::Home          => " Enter: search   ↓: covers   Ctrl+P: torrents/catalog   /: commands   Ctrl+J: downloads   Ctrl+H: history   ?: help",
        View::Discover      => " ←→↑↓: move   Enter: open   d: download   D: download to…   f: star   b: next list   Esc: back",
        View::Details       => " Enter: select/play   Tab: next pane   d: download   D: download to…   f: star   t: torrents   Esc: back",
        View::Tv if app.tv_filter_active
                            => " type: filter channels   Enter: keep   Esc: clear",
        View::Tv if app.tv_manage
                            => " a: add playlist   x: remove   j/k: move   Esc: back to channels",
        View::Tv            => " Enter: play   /: filter   a: add playlist   m: manage playlists   r: reload   Esc: home",
        View::Addons        => " Space: enable/disable   a: install by manifest URL   x: remove   Esc: home",
        View::Settings      => " Enter/→: change   ←: previous   j/k: move   Esc: home",
        View::Files         => " Enter: play   p: playlist   j/k: move   Esc: back   q: quit",
        View::SearchResults if app.search_preview.is_some()
                            => " Enter: stream   d: download   D: download to…   j/k: move   o: sort   f/Esc: close preview",
        View::SearchResults if app.search_filter_active
                            => " type: filter   Backspace: delete   Ctrl+U: clear   Esc: clear+close   Enter: keep+close",
        View::SearchResults => " /: filter   f: files   Enter: stream   d: download   D: download to…   o: sort   s: new search   Esc: back",
        View::ConfirmDelete => " y: confirm   n/Esc: cancel",
        View::Torrents      => " s/Esc: search   Tab: history   a: add   Enter: files   Space: pause   d: remove   q: quit",
        View::History       => " Enter: stream again   d: download   D: download to…   x: remove   j/k: move   Tab/Esc: home   q: quit",
        View::ConfirmQuit   => " y: keep downloading in background   n: quit and pause   Esc: cancel",
        View::AddInput      => " Enter: add   Esc: cancel",
    };
    f.render_widget(
        Paragraph::new(Span::styled(help, Style::default().fg(GRAY).bg(Color::Rgb(0x1d, 0x20, 0x21)))),
        area,
    );
}

fn draw_help_popup(f: &mut Frame, area: Rect) {
    let popup = centered_rect(70, 52, area);
    f.render_widget(Clear, popup);

    fn key(k: &'static str) -> Span<'static> {
        Span::styled(format!("{:<12}", k), Style::default().fg(YELLOW).add_modifier(Modifier::BOLD))
    }
    fn desc(d: &'static str) -> Span<'static> {
        Span::styled(d, Style::default().fg(FG))
    }
    fn section(s: &'static str) -> Line<'static> {
        Line::from(Span::styled(format!("  {}", s), Style::default().fg(AQUA).add_modifier(Modifier::BOLD)))
    }
    fn row(k: &'static str, d: &'static str) -> Line<'static> {
        Line::from(vec![Span::raw("    "), key(k), desc(d)])
    }

    let lines: Vec<Line> = vec![
        Line::from(""),
        section("Anywhere"),
        row("Ctrl+H",      "history"),
        row("Ctrl+J",      "downloads"),
        row("?",           "this help"),
        Line::from(""),
        section("Home / Search"),
        row("type",        "type to build search query"),
        row("Enter",       "search for the typed query"),
        row("↓",           "move into the covers under the search box"),
        row("Esc",         "clear search query"),
        row("Tab",         "go to downloads view"),
        row("Ctrl+P",      "switch search: torrents ↔ catalog (Stremio)"),
        row("/",           "commands: /browse /favorites /history /tidy …"),
        Line::from(""),
        section("Covers (home row, /browse, catalog results)"),
        row("←→↑↓",        "move between covers"),
        row("Enter",       "open the title"),
        row("f",           "star / unstar"),
        row("Tab / b",     "next browse list (home row / browse page)"),
        Line::from(""),
        section("Catalog list & details"),
        row("Enter",       "open title · pick season/episode · play stream"),
        row("d / D",       "download a movie straight from the list (D picks the folder)"),
        row("Tab",         "next pane (seasons → episodes → streams)"),
        row("f",           "star / unstar"),
        row("d",           "download the selected stream"),
        row("D",           "download it into a folder you choose"),
        row("t",           "search torrent indexers for this title"),
        row("b",           "next /browse list"),
        Line::from(""),
        section("More screens"),
        row("/tv",         "live TV from M3U playlists (a: add, m: manage)"),
        row("/addons",     "install stream/subtitle addons by manifest URL"),
        row("/settings",   "player, subtitle language, download folder"),
        Line::from(""),
        section("Search Results"),
        row("/",           "open filter bar (narrow by title substring)"),
        row("f",           "preview file list"),
        row("Enter",       "stream selected result"),
        row("d",           "download permanently"),
        row("D",           "download into a folder you choose"),
        row("o",           "cycle sort: seeders → name → size"),
        row("s",           "new search (go back to home)"),
        row("Esc",         "back to home"),
        Line::from(""),
        section("Files view"),
        row("Enter",       "stream selected file"),
        row("p",           "play all files as playlist"),
        Line::from(""),
        section("Downloads view"),
        row("a",           "add magnet link / URL / .torrent path"),
        row("Space",       "pause / resume torrent"),
        row("d / D",       "remove (keep files) / remove + delete files"),
        row("s  or  Esc",  "go to home / search"),
        row("Tab / H",     "go to history"),
        row("q / Q",       "quit / quit and stop background downloads"),
        Line::from(""),
        section("History view"),
        row("Enter",       "stream it again"),
        row("d",           "download permanently"),
        row("D",           "download into a folder you choose"),
        row("x",           "remove from history"),
        Line::from(""),
        Line::from(Span::styled("  press ? to open this again  ·  any key to close", Style::default().fg(GRAY))),
    ];

    let p = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" keyboard shortcuts ")
            .border_style(Style::default().fg(YELLOW))
            .style(Style::default().bg(BG)),
    );
    f.render_widget(p, popup);
}

fn draw_history(f: &mut Frame, area: Rect, app: &App) {
    if app.history.is_empty() {
        let msg = Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled(
                "no history yet",
                Style::default().fg(GRAY).add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "everything you stream or download shows up here",
                Style::default().fg(GRAY),
            )),
        ])
        .alignment(Alignment::Center)
        .block(Block::default().borders(Borders::ALL).title(" history ").border_style(Style::default().fg(GRAY)));
        f.render_widget(msg, area);
        return;
    }

    let now = history::now();
    let rows: Vec<Row> = app
        .history
        .iter()
        .map(|e| {
            let (kind, color) = match e.kind {
                history::Kind::Stream => ("▶ stream", AQUA),
                history::Kind::Download => ("⬇ download", GREEN),
            };
            let mut title = vec![Span::styled(e.title.clone(), Style::default().fg(FG))];
            if let Some(dest) = &e.dest {
                title.push(Span::styled(format!("  → {}", dest), Style::default().fg(GRAY)));
            }
            Row::new(vec![
                ratatui::widgets::Cell::from(history::format_when(e.at, now)).style(Style::default().fg(GRAY)),
                ratatui::widgets::Cell::from(kind).style(Style::default().fg(color)),
                ratatui::widgets::Cell::from(Line::from(title)),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [Constraint::Length(11), Constraint::Length(11), Constraint::Min(20)],
    )
    .header(
        Row::new(vec!["when", "type", "title"])
            .style(Style::default().fg(YELLOW).add_modifier(Modifier::BOLD)),
    )
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!(" history ({}) ", app.history.len()))
            .border_style(Style::default().fg(GRAY)),
    )
    .highlight_style(Style::default().bg(Color::Rgb(0x3c, 0x38, 0x36)).add_modifier(Modifier::BOLD))
    .highlight_symbol("▶ ");

    let mut state = TableState::default();
    state.select(Some(app.history_selected.min(app.history.len().saturating_sub(1))));
    f.render_stateful_widget(table, area, &mut state);
}

fn centered_message(f: &mut Frame, area: Rect, block: Block, msg: String, color: Color) {
    f.render_widget(
        Paragraph::new(vec![Line::from(""), Line::from(Span::styled(msg, Style::default().fg(color)))])
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true })
            .block(block),
        area,
    );
}

fn highlight() -> Style {
    Style::default().bg(Color::Rgb(0x3c, 0x38, 0x36)).add_modifier(Modifier::BOLD)
}

fn draw_discover(f: &mut Frame, area: Rect, app: &App) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", app.catalog_title))
        .border_style(Style::default().fg(GRAY));
    let items = {
        let catalog = app.catalog.lock().unwrap();
        match &*catalog {
            Load::Ready(items) if !items.is_empty() => items.clone(),
            Load::Ready(_) if app.catalog_title == "favorites" => {
                return centered_message(f, area, block, "no favorites yet — press f on a title to star it".into(), GRAY);
            }
            Load::Ready(_) => return centered_message(f, area, block, "nothing found".into(), GRAY),
            Load::Failed(e) => return centered_message(f, area, block, format!("✗ {e}"), RED),
            Load::Idle | Load::Loading => return centered_message(f, area, block, "loading…".into(), YELLOW),
        }
    };

    let inner = block.inner(area);
    f.render_widget(block, area);
    let starred = app.is_favorite(&items[app.catalog_selected.min(items.len() - 1)].id);
    let hint = Line::from(vec![
        Span::styled(
            format!(
                "  {} of {}   Enter: open   f: {}   b: next list   ←→↑↓: move   ",
                app.catalog_selected + 1,
                items.len(),
                if starred { "unstar" } else { "star" }
            ),
            Style::default().fg(GRAY),
        ),
        Span::styled("◆ Letterboxd", Style::default().fg(GREEN)),
    ]);
    f.render_widget(Paragraph::new(hint), Rect { height: 1, ..inner });

    let grid = Rect { y: inner.y + 1, height: inner.height.saturating_sub(1), ..inner };
    let cols = draw_cover_grid(f, grid, app, &items, app.catalog_selected, true);
    app.grid_cols.set(cols);
}

fn pane_list(f: &mut Frame, area: Rect, title: String, items: Vec<ListItem>, selected: usize, focused: bool) {
    let n = items.len();
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_style(Style::default().fg(if focused { AQUA } else { GRAY })),
        )
        .highlight_style(if focused { highlight() } else { Style::default().fg(YELLOW) })
        .highlight_symbol("› ");
    let mut state = ListState::default();
    if n > 0 {
        state.select(Some(selected.min(n - 1)));
    }
    f.render_stateful_widget(list, area, &mut state);
}

fn draw_details(f: &mut Frame, area: Rect, app: &App) {
    let Some(d) = app.details.as_ref() else { return };
    let series = d.item.is_series();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(6), Constraint::Min(5)])
        .split(area);

    // Header: name, facts, description.
    let mut facts: Vec<String> = Vec::new();
    let mut extra = Line::from(Span::styled("loading details…", Style::default().fg(GRAY)));
    {
        let meta = d.meta.lock().unwrap();
        match &*meta {
            Load::Ready(m) => {
                let year = m.year_label();
                if !year.is_empty() {
                    facts.push(year);
                }
                if let Load::Ready(Some(lb)) = &*d.letterboxd.lock().unwrap() {
                    facts.push(lb.label());
                }
                if let Some(rt) = &m.runtime {
                    facts.push(rt.clone());
                }
                if !m.all_genres().is_empty() {
                    facts.push(m.all_genres().iter().take(3).cloned().collect::<Vec<_>>().join(", "));
                }
                if !m.cast.is_empty() {
                    facts.push(m.cast.iter().take(3).cloned().collect::<Vec<_>>().join(", "));
                }
                extra = Line::from(Span::styled(m.description.clone().unwrap_or_default(), Style::default().fg(FG)));
            }
            Load::Failed(e) => extra = Line::from(Span::styled(format!("✗ {e}"), Style::default().fg(RED))),
            Load::Idle | Load::Loading => {}
        }
    }
    let star = if app.is_favorite(&d.item.id) { "★ " } else { "" };
    let header = vec![
        Line::from(vec![
            Span::styled(format!("{star}{}", d.item.name), Style::default().fg(YELLOW).add_modifier(Modifier::BOLD)),
            Span::styled(if series { "   series" } else { "   movie" }, Style::default().fg(GRAY)),
        ]),
        Line::from(Span::styled(facts.join("  ·  "), Style::default().fg(AQUA))),
        extra,
    ];
    let block = Block::default().borders(Borders::ALL).border_style(Style::default().fg(GRAY));
    let inner = block.inner(chunks[0]);
    f.render_widget(block, chunks[0]);
    // The title's own cover alongside the facts, when it fits.
    let cover_w = 8u16;
    let text = if inner.width > cover_w * 3 && inner.height >= 4 {
        let cover = Rect { width: cover_w, height: inner.height, ..inner };
        draw_cover(f, cover, app, &d.item, false);
        Rect { x: inner.x + cover_w + 2, width: inner.width - cover_w - 2, ..inner }
    } else {
        inner
    };
    f.render_widget(Paragraph::new(header).wrap(Wrap { trim: true }), text);

    let streams_area = if series {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(14), Constraint::Percentage(32), Constraint::Min(30)])
            .split(chunks[1]);
        let seasons = app.details_seasons();
        let season_items = seasons
            .iter()
            .map(|s| ListItem::new(if s.number == 0 { "Specials".to_string() } else { format!("Season {}", s.number) }))
            .collect();
        pane_list(f, cols[0], " seasons ".into(), season_items, d.season_idx, d.pane == Pane::Seasons);
        let episodes = seasons
            .get(d.season_idx)
            .map(|s| {
                s.episodes
                    .iter()
                    .map(|e| {
                        let title = if e.title.is_empty() { format!("Episode {}", e.number) } else { e.title.clone() };
                        ListItem::new(format!("{:>2}  {}", e.number, title))
                    })
                    .collect()
            })
            .unwrap_or_default();
        pane_list(f, cols[1], " episodes ".into(), episodes, d.episode_idx, d.pane == Pane::Episodes);
        cols[2]
    } else {
        chunks[1]
    };
    draw_streams(f, streams_area, d);
}

fn draw_streams(f: &mut Frame, area: Rect, d: &Details) {
    let focused = d.pane == Pane::Streams;
    let heading = match (d.item.is_series(), d.streams_for) {
        (true, Some((s, e))) => format!(" streams — S{s:02}E{e:02} "),
        (true, None) => " streams ".to_string(),
        (false, _) => " streams ".to_string(),
    };
    let make_block = |title: String| {
        Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(Style::default().fg(if focused { AQUA } else { GRAY }))
    };
    let block = make_block(heading.clone());
    let streams = d.streams.lock().unwrap();
    let list = match &*streams {
        Load::Ready((list, _)) if !list.is_empty() => list,
        Load::Ready((_, errors)) if !errors.is_empty() => {
            return centered_message(f, area, block, format!("no streams — {}", errors.join("; ")), GRAY);
        }
        Load::Ready(_) => return centered_message(f, area, block, "no streams found — press t to search torrents instead".into(), GRAY),
        Load::Loading => return centered_message(f, area, block, "finding streams…".into(), YELLOW),
        Load::Failed(e) => return centered_message(f, area, block, format!("✗ {e}"), RED),
        Load::Idle => return centered_message(f, area, block, "pick an episode and press Enter".into(), GRAY),
    };

    // Say which addon (or the built-in search) the streams came from: the source
    // column shows the site each torrent was found on, which doesn't reveal that.
    let mut by_addon: Vec<(&str, usize)> = Vec::new();
    for s in list.iter() {
        match by_addon.iter_mut().find(|(name, _)| *name == s.addon.as_str()) {
            Some((_, count)) => *count += 1,
            None => by_addon.push((s.addon.as_str(), 1)),
        }
    }
    let from = by_addon
        .iter()
        .map(|(name, count)| format!("{name} {count}"))
        .collect::<Vec<_>>()
        .join(", ");
    let block = make_block(format!("{}· from {from} ", heading));

    let rows: Vec<Row> = list
        .iter()
        .map(|s| {
            let quality_color = match s.quality.as_deref() {
                Some("2160p") => ORANGE,
                Some("1080p") => GREEN,
                Some("720p") => AQUA,
                _ => GRAY,
            };
            let (seeds, seed_color) = match s.seeders {
                Some(n) if n >= 20 => (n.to_string(), GREEN),
                Some(n) if n > 0 => (n.to_string(), YELLOW),
                Some(n) => (n.to_string(), RED),
                None if s.is_torrent() => ("?".to_string(), GRAY),
                None => ("http".to_string(), AQUA),
            };
            let tags: Vec<String> = [s.codec.clone(), s.languages.clone()].into_iter().flatten().collect();
            let release = Line::from(vec![
                Span::raw(s.release.clone()),
                Span::styled(
                    if tags.is_empty() { String::new() } else { format!("  {}", tags.join(" · ")) },
                    Style::default().fg(GRAY),
                ),
            ]);
            Row::new(vec![
                Cell::from(s.quality.clone().unwrap_or_else(|| "—".into())).style(Style::default().fg(quality_color)),
                Cell::from(s.size.map(human_bytes).unwrap_or_default()),
                Cell::from(seeds).style(Style::default().fg(seed_color)),
                Cell::from(s.origin.clone().unwrap_or_else(|| s.addon.clone())).style(Style::default().fg(GRAY)),
                Cell::from(release),
            ])
        })
        .collect();
    let table = Table::new(
        rows,
        [Constraint::Length(6), Constraint::Length(10), Constraint::Length(5), Constraint::Length(14), Constraint::Min(20)],
    )
    .header(Row::new(vec!["res", "size", "seeds", "source", "release"]).style(Style::default().fg(YELLOW).add_modifier(Modifier::BOLD)))
    .block(block)
    .highlight_style(if focused { highlight() } else { Style::default() })
    .highlight_symbol("▶ ");
    let mut state = TableState::default();
    state.select(Some(d.stream_selected.min(list.len() - 1)));
    f.render_stateful_widget(table, area, &mut state);
}

fn draw_tv(f: &mut Frame, area: Rect, app: &App) {
    let block = |title: String| {
        Block::default().borders(Borders::ALL).title(title).border_style(Style::default().fg(GRAY))
    };

    if app.tv_manage {
        let title = format!(" playlists ({}) — a: add   x: remove   Esc: channels ", app.tv_playlists.len());
        if app.tv_playlists.is_empty() {
            return centered_message(f, area, block(title), "no playlists — press a to add an M3U URL or file path".into(), GRAY);
        }
        let items = app.tv_playlists.iter().map(|p| ListItem::new(p.clone())).collect();
        return pane_list(f, area, title, items, app.tv_playlist_selected, true);
    }

    let show_filter = app.tv_filter_active || !app.tv_filter.is_empty();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(if show_filter { 1 } else { 0 }), Constraint::Min(3)])
        .split(area);
    if show_filter {
        let mut spans = vec![
            Span::styled("  / ", Style::default().fg(AQUA).add_modifier(Modifier::BOLD)),
            Span::styled(app.tv_filter.clone(), Style::default().fg(if app.tv_filter_active { FG } else { YELLOW })),
        ];
        if app.tv_filter_active {
            spans.push(Span::styled("█", Style::default().fg(AQUA)));
        }
        f.render_widget(Paragraph::new(Line::from(spans)), chunks[0]);
    }

    let channels = app.tv_channels.lock().unwrap();
    let (list, errors) = match &*channels {
        Load::Idle | Load::Loading => {
            return centered_message(f, chunks[1], block(" live TV ".into()), "loading playlists…".into(), YELLOW);
        }
        Load::Failed(e) => return centered_message(f, chunks[1], block(" live TV ".into()), format!("✗ {e}"), RED),
        Load::Ready((list, errors)) => (list, errors),
    };
    if list.is_empty() {
        let msg = if app.tv_playlists.is_empty() {
            "no playlists yet — press a to add an M3U URL or file path".to_string()
        } else {
            format!("no channels loaded — {}", errors.join("; "))
        };
        return centered_message(f, chunks[1], block(" live TV ".into()), msg, GRAY);
    }

    let visible = crate::tv::filter(list, &app.tv_filter);
    let mut title = format!(" live TV — {} channels ", visible.len());
    if !errors.is_empty() {
        title.push_str(&format!("· {} playlist(s) failed ", errors.len()));
    }
    // Build rows only for the window on screen: playlists can hold tens of thousands of channels.
    let height = chunks[1].height.saturating_sub(3).max(1) as usize;
    let selected = app.tv_selected.min(visible.len().saturating_sub(1));
    let start = selected.saturating_sub(height - 1);
    let rows: Vec<Row> = visible
        .iter()
        .skip(start)
        .take(height)
        .map(|c| {
            Row::new(vec![
                Cell::from(c.name.clone()),
                Cell::from(c.group.clone()).style(Style::default().fg(GRAY)),
            ])
        })
        .collect();
    let table = Table::new(rows, [Constraint::Min(20), Constraint::Length(24)])
        .header(Row::new(vec!["channel", "group"]).style(Style::default().fg(YELLOW).add_modifier(Modifier::BOLD)))
        .block(block(title))
        .highlight_style(highlight())
        .highlight_symbol("▶ ");
    let mut state = TableState::default();
    if !visible.is_empty() {
        state.select(Some(selected - start));
    }
    f.render_stateful_widget(table, chunks[1], &mut state);
}

fn draw_addons(f: &mut Frame, area: Rect, app: &App) {
    let rows: Vec<Row> = app
        .addons
        .iter()
        .map(|a| {
            let (state, color) = if a.enabled { ("on", GREEN) } else { ("off", GRAY) };
            let name = if a.is_core() { format!("{} (core)", a.name) } else { a.name.clone() };
            Row::new(vec![
                Cell::from(state).style(Style::default().fg(color)),
                Cell::from(name),
                Cell::from(a.capabilities()).style(Style::default().fg(AQUA)),
                Cell::from(a.manifest_url.clone()).style(Style::default().fg(GRAY)),
            ])
        })
        .collect();
    let title = if app.addons.iter().any(|a| a.enabled && a.stream) {
        format!(" addons ({}) ", app.addons.len())
    } else {
        format!(" addons ({}) — streams come from torrent search; a: add a stream addon for more ", app.addons.len())
    };
    let table = Table::new(
        rows,
        [Constraint::Length(4), Constraint::Length(24), Constraint::Length(30), Constraint::Min(20)],
    )
    .header(Row::new(vec!["", "name", "provides", "manifest"]).style(Style::default().fg(YELLOW).add_modifier(Modifier::BOLD)))
    .block(Block::default().borders(Borders::ALL).title(title).border_style(Style::default().fg(GRAY)))
    .highlight_style(highlight())
    .highlight_symbol("▶ ");
    let mut state = TableState::default();
    if !app.addons.is_empty() {
        state.select(Some(app.addon_selected.min(app.addons.len() - 1)));
    }
    f.render_stateful_widget(table, area, &mut state);
}

fn draw_settings(f: &mut Frame, area: Rect, app: &App) {
    let rows: Vec<Row> = app
        .settings_rows()
        .into_iter()
        .map(|(label, value)| Row::new(vec![Cell::from(label).style(Style::default().fg(YELLOW)), Cell::from(value)]))
        .collect();
    let table = Table::new(rows, [Constraint::Length(18), Constraint::Min(20)])
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" settings — saved to {} ", crate::config::config_dir().join("config.json").display()))
                .border_style(Style::default().fg(AQUA)),
        )
        .highlight_style(highlight())
        .highlight_symbol("▶ ");
    let mut state = TableState::default();
    state.select(Some(app.settings_selected.min(App::SETTINGS_ROWS - 1)));
    f.render_stateful_widget(table, area, &mut state);
}

fn draw_quit_popup(f: &mut Frame, area: Rect, app: &App) {
    let popup = centered_rect(62, 9, area);
    f.render_widget(Clear, popup);
    let n = app.active_download_count();
    let key = |k: &'static str| Span::styled(format!("  {:<5}", k), Style::default().fg(YELLOW).add_modifier(Modifier::BOLD));
    let p = Paragraph::new(vec![
        Line::from(""),
        Line::from(Span::styled(
            format!("  {} download{} still in progress", n, if n == 1 { "" } else { "s" }),
            Style::default().fg(FG).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![key("y"), Span::styled("keep downloading in the background", Style::default().fg(FG))]),
        Line::from(vec![key("n"), Span::styled("quit and pause them (resume next launch)", Style::default().fg(FG))]),
        Line::from(vec![key("Esc"), Span::styled("cancel", Style::default().fg(GRAY))]),
    ])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(" quit torflix? ")
            .border_style(Style::default().fg(YELLOW))
            .style(Style::default().bg(BG)),
    );
    f.render_widget(p, popup);
}

fn centered_rect(width: u16, height: u16, r: Rect) -> Rect {
    let x = r.x + r.width.saturating_sub(width) / 2;
    let y = r.y + r.height.saturating_sub(height) / 2;
    Rect { x, y, width: width.min(r.width), height: height.min(r.height) }
}

fn draw_add_popup(f: &mut Frame, area: Rect, app: &App) {
    let w = area.width.saturating_sub(8).min(90).max(30);
    let popup = centered_rect(w, 5, area);
    f.render_widget(Clear, popup);

    let inner_w = popup.width.saturating_sub(4) as usize;
    let shown: String = if app.input.chars().count() > inner_w {
        let skip = app.input.chars().count() - inner_w;
        app.input.chars().skip(skip).collect()
    } else {
        app.input.clone()
    };

    let p = Paragraph::new(vec![
        Line::from(""),
        Line::from(vec![
            Span::styled(" › ", Style::default().fg(YELLOW)),
            Span::raw(shown),
            Span::styled("█", Style::default().fg(YELLOW)),
        ]),
    ])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(match app.input_purpose {
                InputPurpose::Torrent => " add torrent — magnet link, URL, or local path ",
                InputPurpose::Playlist => " add M3U playlist — URL or file path ",
                InputPurpose::Addon => " install Stremio addon — manifest URL ",
                InputPurpose::DownloadDir => " download folder — leave empty for the default ",
                InputPurpose::DownloadTo => " download into — edit the folder, or leave it as is ",
            })
            .border_style(Style::default().fg(YELLOW))
            .style(Style::default().bg(BG)),
    );
    f.render_widget(p, popup);
}

fn draw_confirm_popup(f: &mut Frame, area: Rect, app: &App) {
    let popup = centered_rect(56, 5, area);
    f.render_widget(Clear, popup);
    let name = app.selected_row().map(|r| r.name).unwrap_or_default();
    let (title, warn) = if app.delete_with_files {
        (" remove torrent AND delete files? ", RED)
    } else {
        (" remove torrent (keep files)? ", YELLOW)
    };
    let p = Paragraph::new(vec![
        Line::from(""),
        Line::from(Span::styled(name, Style::default().fg(FG).add_modifier(Modifier::BOLD))),
    ])
    .alignment(Alignment::Center)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(Style::default().fg(warn))
            .style(Style::default().bg(BG)),
    );
    f.render_widget(p, popup);
}

fn render_results_table(
    f: &mut Frame,
    area: Rect,
    block: Block,
    results: &[&crate::search::SearchResult],
    sort: &SortMode,
    selected: usize,
) {
    let rows: Vec<Row> = results.iter().map(|r| {
        let seed_style = if r.seeders >= 20 {
            Style::default().fg(GREEN)
        } else if r.seeders > 0 {
            Style::default().fg(YELLOW)
        } else {
            Style::default().fg(RED)
        };
        let rating_cell = match r.rating {
            Some(v) => ratatui::widgets::Cell::from(format!("{:.1}★", v)).style(Style::default().fg(YELLOW)),
            None    => ratatui::widgets::Cell::from(""),
        };
        Row::new(vec![
            ratatui::widgets::Cell::from(r.title.clone()),
            ratatui::widgets::Cell::from(human_bytes(r.size)),
            ratatui::widgets::Cell::from(r.seeders.to_string()).style(seed_style),
            ratatui::widgets::Cell::from(r.leechers.to_string()).style(Style::default().fg(GRAY)),
            rating_cell,
            ratatui::widgets::Cell::from(r.indexer.clone()).style(Style::default().fg(GRAY)),
        ])
    }).collect();
    let n = results.len();
    let hdr_title = if *sort == SortMode::Name    { "title ▼" } else { "title" };
    let hdr_size  = if *sort == SortMode::Size    { "size ▼"  } else { "size" };
    let hdr_seed  = if *sort == SortMode::Seeders { "seed ▼"  } else { "seed" };
    let table = Table::new(
        rows,
        [
            Constraint::Min(28),
            Constraint::Length(10),
            Constraint::Length(6),
            Constraint::Length(6),
            Constraint::Length(6),
            Constraint::Length(12),
        ],
    )
    .header(
        Row::new(vec![hdr_title, hdr_size, hdr_seed, "leech", "imdb", "indexer"])
            .style(Style::default().fg(YELLOW).add_modifier(Modifier::BOLD)),
    )
    .block(block)
    .highlight_style(Style::default().bg(Color::Rgb(0x3c, 0x38, 0x36)).add_modifier(Modifier::BOLD))
    .highlight_symbol("▶ ");
    let mut state = TableState::default();
    state.select(Some(selected.min(n.saturating_sub(1))));
    f.render_stateful_widget(table, area, &mut state);
}

fn results_title(q: &str, filtered: usize, total: usize, suffix: &str, filter: &str) -> String {
    if !filter.is_empty() {
        format!(" results — '{}' | filter: '{}' ({}/{}{}) ", q, filter, filtered, total, suffix)
    } else {
        format!(" results — '{}' ({}{}) ", q, total, suffix)
    }
}

fn draw_search_results(f: &mut Frame, area: Rect, app: &App) {
    let filter_visible = app.search_filter_active || !app.search_filter.is_empty();
    let filter_h = if filter_visible { 3u16 } else { 0 };
    let preview_h = if app.search_preview.is_some() { 14u16.min(area.height * 37 / 100) } else { 0 };

    // Build layout constraints dynamically
    let mut constraints = vec![];
    if filter_visible { constraints.push(Constraint::Length(filter_h)); }
    constraints.push(Constraint::Min(3));
    if app.search_preview.is_some() { constraints.push(Constraint::Length(preview_h)); }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    let mut idx = 0usize;
    let filter_area = if filter_visible { let a = Some(chunks[idx]); idx += 1; a } else { None };
    let results_area = chunks[idx]; idx += 1;
    let preview_area = if app.search_preview.is_some() { Some(chunks[idx]) } else { None };

    let q = app.search_query.trim();
    let filter = &app.search_filter;

    let status = app.search.lock().unwrap();
    match &*status {
        SearchStatus::Searching(partial) => {
            let raw = partial.lock().unwrap();
            if raw.is_empty() {
                let block = Block::default()
                    .borders(Borders::ALL)
                    .title(format!(" results — '{}' ", q))
                    .border_style(Style::default().fg(GRAY));
                let p = Paragraph::new(vec![
                    Line::from(""),
                    Line::from(Span::styled(
                        "searching indexers…",
                        Style::default().fg(YELLOW).add_modifier(Modifier::BOLD),
                    )),
                ])
                .alignment(Alignment::Center)
                .block(block);
                f.render_widget(p, results_area);
            } else {
                let filtered = app.filtered_results(&raw);
                let title = results_title(q, filtered.len(), raw.len(), ", searching…", filter);
                let block = Block::default()
                    .borders(Borders::ALL)
                    .title(title)
                    .border_style(Style::default().fg(YELLOW));
                render_results_table(f, results_area, block, &filtered, &app.search_sort, app.search_selected);
            }
        }
        SearchStatus::Failed(e) => {
            let block = Block::default()
                .borders(Borders::ALL)
                .title(format!(" results — '{}' ", q))
                .border_style(Style::default().fg(GRAY));
            let p = Paragraph::new(vec![
                Line::from(""),
                Line::from(Span::styled(format!("✗ {}", e), Style::default().fg(RED))),
                Line::from(""),
                Line::from(Span::styled("press Esc to go back and try again", Style::default().fg(GRAY))),
            ])
            .alignment(Alignment::Center)
            .block(block);
            f.render_widget(p, results_area);
        }
        SearchStatus::Done(raw) if raw.is_empty() => {
            let block = Block::default()
                .borders(Borders::ALL)
                .title(format!(" results — '{}' ", q))
                .border_style(Style::default().fg(GRAY));
            let p = Paragraph::new(vec![
                Line::from(""),
                Line::from(Span::styled("no results", Style::default().fg(GRAY))),
            ])
            .alignment(Alignment::Center)
            .block(block);
            f.render_widget(p, results_area);
        }
        SearchStatus::Done(raw) => {
            let filtered = app.filtered_results(raw);
            let title = results_title(q, filtered.len(), raw.len(), "", filter);
            let block = Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_style(Style::default().fg(GRAY));
            render_results_table(f, results_area, block, &filtered, &app.search_sort, app.search_selected);
        }
        SearchStatus::Idle => {
            let block = Block::default()
                .borders(Borders::ALL)
                .title(format!(" results — '{}' ", q))
                .border_style(Style::default().fg(GRAY));
            f.render_widget(block, results_area);
        }
    }
    drop(status);

    if let Some(fa) = filter_area {
        draw_filter_bar(f, fa, app);
    }
    if let (Some(pa), Some(preview)) = (preview_area, &app.search_preview) {
        draw_preview_panel(f, pa, preview);
    }
}

fn draw_filter_bar(f: &mut Frame, area: Rect, app: &App) {
    let active = app.search_filter_active;
    let border_color = if active { AQUA } else { GRAY };
    let title = if active {
        " filter — Esc: clear+close   Enter or /: keep+close "
    } else {
        " filter — /: edit   Esc: clear "
    };

    let content = if app.search_filter.is_empty() {
        Line::from(vec![
            Span::styled("  › ", Style::default().fg(AQUA)),
            Span::styled("type to narrow results…", Style::default().fg(GRAY)),
        ])
    } else if active {
        Line::from(vec![
            Span::styled("  › ", Style::default().fg(AQUA).add_modifier(Modifier::BOLD)),
            Span::styled(app.search_filter.clone(), Style::default().fg(FG)),
            Span::styled("█", Style::default().fg(AQUA)),
        ])
    } else {
        Line::from(vec![
            Span::styled("  › ", Style::default().fg(GRAY)),
            Span::styled(app.search_filter.clone(), Style::default().fg(YELLOW)),
            Span::styled("  (/ to edit)", Style::default().fg(GRAY)),
        ])
    };

    let p = Paragraph::new(vec![content]).block(
        Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(Style::default().fg(border_color)),
    );
    f.render_widget(p, area);
}

fn draw_preview_panel(f: &mut Frame, area: Rect, preview: &SearchPreview) {
    let state = preview.state.lock().unwrap();
    match &*state {
        PreviewState::Loading => {
            let p = Paragraph::new(vec![
                Line::from(""),
                Line::from(Span::styled(
                    "⧗ fetching file list from peers…",
                    Style::default().fg(YELLOW),
                )),
            ])
            .alignment(Alignment::Center)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" files ")
                    .border_style(Style::default().fg(AQUA)),
            );
            f.render_widget(p, area);
        }
        PreviewState::Error(e) => {
            let p = Paragraph::new(vec![
                Line::from(""),
                Line::from(Span::styled(format!("✗ {}", e), Style::default().fg(RED))),
            ])
            .alignment(Alignment::Center)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" files ")
                    .border_style(Style::default().fg(RED)),
            );
            f.render_widget(p, area);
        }
        PreviewState::Ready(files) => {
            // Sort the same way play_from_preview does so visual position == file_selected.
            let mut sorted: Vec<&(usize, crate::rqbit::FileDetails)> = files.iter().collect();
            match preview.file_sort {
                FileSortMode::Name => sorted.sort_by(|a, b| a.1.name.to_lowercase().cmp(&b.1.name.to_lowercase())),
                FileSortMode::Size => sorted.sort_by(|a, b| b.1.length.cmp(&a.1.length)),
            }
            let sort_label = match preview.file_sort {
                FileSortMode::Name => "name ▼",
                FileSortMode::Size => "size ▼",
            };

            let items: Vec<ListItem> = sorted
                .iter()
                .map(|(_, file)| {
                    let video = is_video(&file.name);
                    let icon = if video { "▶ " } else { "  " };
                    let style = if video { Style::default().fg(FG) } else { Style::default().fg(GRAY) };
                    ListItem::new(Line::from(vec![
                        Span::styled(icon, Style::default().fg(ORANGE)),
                        Span::styled(file.name.clone(), style),
                        Span::styled(
                            format!("  ({})", human_bytes(file.length)),
                            Style::default().fg(GRAY),
                        ),
                    ]))
                })
                .collect();

            let list = List::new(items)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(format!(" files ({}) [{}] — o: sort   Enter: stream   d: download   f/Esc: close ", files.len(), sort_label))
                        .border_style(Style::default().fg(AQUA)),
                )
                .highlight_style(
                    Style::default()
                        .bg(Color::Rgb(0x3c, 0x38, 0x36))
                        .add_modifier(Modifier::BOLD),
                )
                .highlight_symbol("→ ");

            let mut list_state = ListState::default();
            list_state.select(Some(preview.file_selected.min(files.len().saturating_sub(1))));
            f.render_stateful_widget(list, area, &mut list_state);
        }
    }
}

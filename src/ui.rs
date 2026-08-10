use crate::app::{human_bytes, is_video, App, FileSortMode, PreviewState, SearchPreview, SearchStatus, SortMode, View};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Row, Table, TableState},
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

    match app.view {
        View::Home        => draw_home(f, chunks[1], app),
        View::Files       => draw_files(f, chunks[1], app),
        View::SearchResults => draw_search_results(f, chunks[1], app),
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
    if app.show_help {
        draw_help_popup(f, area);
    }
}

fn draw_title(f: &mut Frame, area: Rect, app: &App) {
    let engine = if *app.engine_up.lock().unwrap() {
        Span::styled(" engine: online ", Style::default().fg(GREEN))
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

const ASCII_ART: &[&str] = &[
    "████████╗ ██████╗ ██████╗ ███████╗██╗     ██╗██╗  ██╗",
    "   ██╔══╝██╔═══██╗██╔══██╗██╔════╝██║     ██║╚██╗██╔╝",
    "   ██║   ██║   ██║██████╔╝█████╗  ██║     ██║ ╚███╔╝ ",
    "   ██║   ██║   ██║██╔══██╗██╔══╝  ██║     ██║ ██╔██╗ ",
    "   ██║   ╚██████╔╝██║  ██║██║     ███████╗██║██╔╝ ██╗",
    "   ╚═╝    ╚═════╝ ╚═╝  ╚═╝╚═╝     ╚══════╝╚═╝╚═╝  ╚═╝",
];

fn draw_home(f: &mut Frame, area: Rect, app: &App) {
    // Vertical layout: spacer | art | gap | search bar | spacer
    let art_height = ASCII_ART.len() as u16;
    let bar_height = 3u16; // border + 1 content row + border
    let gap = 2u16;
    let total = art_height + gap + bar_height;

    // Center the block vertically
    let top_pad = area.height.saturating_sub(total) / 2;

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(top_pad),
            Constraint::Length(art_height),
            Constraint::Length(gap),
            Constraint::Length(bar_height),
            Constraint::Min(0),
        ])
        .split(area);

    // ASCII art lines
    let art_lines: Vec<Line> = ASCII_ART
        .iter()
        .map(|l| Line::from(Span::styled(*l, Style::default().fg(YELLOW).add_modifier(Modifier::BOLD))))
        .collect();
    f.render_widget(
        Paragraph::new(art_lines).alignment(Alignment::Center),
        chunks[1],
    );

    // Search bar — centered horizontally
    let bar_width = area.width.min(60).max(20);
    let bar_x = area.x + area.width.saturating_sub(bar_width) / 2;
    let bar_rect = Rect { x: bar_x, y: chunks[3].y, width: bar_width, height: bar_height };

    let inner_w = bar_width.saturating_sub(6) as usize;
    let q = &app.search_query;
    let shown: String = if q.chars().count() > inner_w {
        q.chars().skip(q.chars().count() - inner_w).collect()
    } else {
        q.clone()
    };

    let p = Paragraph::new(vec![
        Line::from(if q.is_empty() {
            vec![
                Span::styled("  › ", Style::default().fg(AQUA).add_modifier(Modifier::BOLD)),
                Span::styled("search for a movie or show…", Style::default().fg(GRAY)),
            ]
        } else {
            vec![
                Span::styled("  › ", Style::default().fg(AQUA).add_modifier(Modifier::BOLD)),
                Span::styled(shown, Style::default().fg(FG)),
                Span::styled("█", Style::default().fg(AQUA)),
            ]
        }),
    ])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(if q.is_empty() { GRAY } else { AQUA })),
    );
    f.render_widget(p, bar_rect);

    // Hint below search bar when empty
    if q.is_empty() {
        let hint_y = bar_rect.y + bar_height;
        if hint_y < area.y + area.height {
            let hint_rect = Rect { x: area.x, y: hint_y, width: area.width, height: 1 };
            f.render_widget(
                Paragraph::new(Span::styled(
                    "Tab: downloads   q: quit (from downloads)",
                    Style::default().fg(GRAY),
                ))
                .alignment(Alignment::Center),
                hint_rect,
            );
        }
    }
}

fn progress_bar(pct: f64, width: usize) -> String {
    let filled = ((pct / 100.0) * width as f64).round() as usize;
    let filled = filled.min(width);
    format!(
        "{}{} {:>5.1}%",
        "█".repeat(filled),
        "░".repeat(width - filled),
        pct
    )
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
            let (pct, speed, peers, eta, state) = match &t.stats {
                Some(s) => {
                    let state_str = if s.finished { "done".to_string() } else { s.state.clone() };
                    (s.progress_pct(), s.down_speed(), s.peers().to_string(), s.eta().unwrap_or_else(|| "-".into()), state_str)
                }
                None => (0.0, "-".into(), "-".into(), "-".into(), "…".into()),
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
                ratatui::widgets::Cell::from(progress_bar(pct, 20))
                    .style(Style::default().fg(if pct >= 100.0 { AQUA } else { GREEN })),
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
            Constraint::Length(28),
            Constraint::Length(12),
            Constraint::Length(6),
            Constraint::Length(12),
            Constraint::Length(8),
        ],
    )
    .header(
        Row::new(vec!["name", "progress", "speed", "peers", "eta", "state"])
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
        View::Home          => " Enter: search   Tab: downloads   Esc: clear   ?: help",
        View::Files         => " Enter: play   p: playlist   j/k: move   Esc: back   q: quit",
        View::SearchResults if app.search_preview.is_some()
                            => " Enter: stream   d: download   j/k: move   o: sort   f/Esc: close preview",
        View::SearchResults if app.search_filter_active
                            => " type: filter   Backspace: delete   Ctrl+U: clear   Esc: clear+close   Enter: keep+close",
        View::SearchResults => " /: filter   f: files   Enter: stream   d: download   o: sort   s: new search   Esc: back",
        View::ConfirmDelete => " y: confirm   n/Esc: cancel",
        View::Torrents      => " s/Esc: search   a: add   Enter: files   Space: pause   d: remove   q: quit",
        View::AddInput      => " Enter: add   Esc: cancel",
    };
    f.render_widget(
        Paragraph::new(Span::styled(help, Style::default().fg(GRAY).bg(Color::Rgb(0x1d, 0x20, 0x21)))),
        area,
    );
}

fn draw_help_popup(f: &mut Frame, area: Rect) {
    let popup = centered_rect(70, 22, area);
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
        section("Home / Search"),
        row("type",        "type to build search query"),
        row("Enter",       "search for the typed query"),
        row("Esc",         "clear search query"),
        row("Tab",         "go to downloads view"),
        Line::from(""),
        section("Search Results"),
        row("/",           "open filter bar (narrow by title substring)"),
        row("f",           "preview file list"),
        row("Enter",       "stream selected result"),
        row("d",           "download permanently"),
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
        row("q / Q",       "quit / quit and stop engine"),
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
            .title(" add torrent — magnet link, URL, or local path ")
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

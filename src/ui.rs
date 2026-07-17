use crate::app::{human_bytes, is_video, App, PopularStatus, SearchStatus, View};
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
            Constraint::Length(1), // title
            Constraint::Min(3),    // main
            Constraint::Length(1), // status
            Constraint::Length(1), // help
        ])
        .split(area);

    draw_title(f, chunks[0], app);

    match app.view {
        View::Files => draw_files(f, chunks[1], app),
        View::SearchResults => draw_search_results(f, chunks[1], app),
        View::Popular => draw_popular(f, chunks[1], app),
        _ => draw_torrents(f, chunks[1], app),
    }

    draw_status(f, chunks[2], app);
    draw_help(f, chunks[3], app);

    if app.view == View::AddInput {
        draw_add_popup(f, area, app);
    }
    if app.view == View::SearchInput {
        draw_search_popup(f, area, app);
    }
    if app.view == View::ConfirmDelete {
        draw_confirm_popup(f, area, app);
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
                "press 'a' and paste a magnet link, torrent URL, or local .torrent path",
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
                    let state_str = if s.finished {
                        "done".to_string()
                    } else {
                        s.state.clone()
                    };
                    (
                        s.progress_pct(),
                        s.down_speed(),
                        s.peers().to_string(),
                        s.eta().unwrap_or_else(|| "-".into()),
                        state_str,
                    )
                }
                None => (0.0, "-".into(), "-".into(), "-".into(), "…".into()),
            };
            let state_style = match state.as_str() {
                "live" => Style::default().fg(GREEN),
                "done" => Style::default().fg(AQUA),
                "paused" => Style::default().fg(YELLOW),
                "error" => Style::default().fg(RED),
                _ => Style::default().fg(GRAY),
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
            .title(" torrents ")
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
            let style = if video {
                Style::default().fg(FG)
            } else {
                Style::default().fg(GRAY)
            };
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
    let (text, style) = if app.view == View::Popular {
        let r = app.popular_ratings_line();
        if !r.is_empty() { (r, Style::default().fg(AQUA)) } else { status_style(&app.status) }
    } else if app.view == View::SearchResults {
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
    let help = match app.view {
        View::Files => " Enter play  p playlist  j/k move  Esc back  q quit",
        View::AddInput => " Enter add  Esc cancel  (paste magnet / URL / .torrent path)",
        View::SearchInput => " Enter search  Esc cancel",
        View::SearchResults => " Enter stream  s new search  j/k move  Esc back  q quit",
        View::ConfirmDelete => " y confirm  n/Esc cancel",
        View::Popular => " s search  Enter stream  Tab/→ list  ]/[ page  r refresh  j/k move  Esc back  q quit",
        View::Torrents => {
            " b browse  s search  a add  Enter files  Space pause  d remove  D remove+files  j/k  q quit"
        }
    };
    f.render_widget(
        Paragraph::new(Span::styled(help, Style::default().fg(GRAY).bg(Color::Rgb(0x1d, 0x20, 0x21)))),
        area,
    );
}

fn centered_rect(width: u16, height: u16, r: Rect) -> Rect {
    let x = r.x + r.width.saturating_sub(width) / 2;
    let y = r.y + r.height.saturating_sub(height) / 2;
    Rect {
        x,
        y,
        width: width.min(r.width),
        height: height.min(r.height),
    }
}

fn draw_add_popup(f: &mut Frame, area: Rect, app: &App) {
    let w = area.width.saturating_sub(8).min(90).max(30);
    let popup = centered_rect(w, 5, area);
    f.render_widget(Clear, popup);

    // Show the tail of the input if it overflows.
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
    let name = app
        .selected_row()
        .map(|r| r.name)
        .unwrap_or_default();
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

fn draw_search_popup(f: &mut Frame, area: Rect, app: &App) {
    let w = area.width.saturating_sub(8).min(80).max(30);
    let popup = centered_rect(w, 5, area);
    f.render_widget(Clear, popup);

    let inner_w = popup.width.saturating_sub(6) as usize;
    let q = &app.search_query;
    let shown: String = if q.chars().count() > inner_w {
        q.chars().skip(q.chars().count() - inner_w).collect()
    } else {
        q.clone()
    };

    let p = Paragraph::new(vec![
        Line::from(""),
        Line::from(vec![
            Span::styled(" 🔍 ", Style::default().fg(AQUA)),
            Span::raw(shown),
            Span::styled("█", Style::default().fg(AQUA)),
        ]),
    ])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(" search torrents ")
            .border_style(Style::default().fg(AQUA))
            .style(Style::default().bg(BG)),
    );
    f.render_widget(p, popup);
}

fn draw_popular(f: &mut Frame, area: Rect, app: &App) {
    let label = app.popular_list.label();
    let title = if app.popular_page > 1 {
        format!(" Letterboxd — {}  [page {}] ", label, app.popular_page)
    } else {
        format!(" Letterboxd — {} ", label)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(ORANGE));

    let status = app.popular.lock().unwrap();
    match &*status {
        PopularStatus::Idle => {
            f.render_widget(block, area);
        }
        PopularStatus::Loading => {
            let p = Paragraph::new(vec![
                Line::from(""),
                Line::from(Span::styled(
                    "fetching from Letterboxd…",
                    Style::default().fg(YELLOW).add_modifier(Modifier::BOLD),
                )),
            ])
            .alignment(Alignment::Center)
            .block(block);
            f.render_widget(p, area);
        }
        PopularStatus::Failed(e) => {
            let p = Paragraph::new(vec![
                Line::from(""),
                Line::from(Span::styled(format!("✗ {}", e), Style::default().fg(RED))),
                Line::from(""),
                Line::from(Span::styled(
                    "press 'r' to retry, Esc to go back",
                    Style::default().fg(GRAY),
                )),
            ])
            .alignment(Alignment::Center)
            .block(block);
            f.render_widget(p, area);
        }
        PopularStatus::Done(movies) if movies.is_empty() => {
            let p = Paragraph::new(vec![
                Line::from(""),
                Line::from(Span::styled("no films found", Style::default().fg(GRAY))),
            ])
            .alignment(Alignment::Center)
            .block(block);
            f.render_widget(p, area);
        }
        PopularStatus::Done(movies) => {
            let items: Vec<ListItem> = movies
                .iter()
                .enumerate()
                .map(|(i, m)| {
                    let num = Span::styled(
                        format!("{:>3}. ", i + 1),
                        Style::default().fg(GRAY),
                    );
                    let title_span = Span::styled(m.title.clone(), Style::default().fg(FG));
                    let year_span = if m.year.is_empty() {
                        Span::raw("")
                    } else {
                        Span::styled(format!("  ({})", m.year), Style::default().fg(GRAY))
                    };
                    let rating_span = if let Some(r) = m.lb_rating {
                        let color = if r >= 4.0 { GREEN } else if r >= 3.5 { YELLOW } else { GRAY };
                        Span::styled(format!("  ★ {:.1}", r), Style::default().fg(color))
                    } else {
                        Span::raw("")
                    };
                    ListItem::new(Line::from(vec![num, title_span, year_span, rating_span]))
                })
                .collect();

            let list = List::new(items)
                .block(block)
                .highlight_style(
                    Style::default()
                        .bg(Color::Rgb(0x3c, 0x38, 0x36))
                        .add_modifier(Modifier::BOLD),
                )
                .highlight_symbol("▶ ");

            let mut state = ListState::default();
            state.select(Some(
                app.popular_selected.min(movies.len().saturating_sub(1)),
            ));
            f.render_stateful_widget(list, area, &mut state);
        }
    }
}

fn draw_search_results(f: &mut Frame, area: Rect, app: &App) {
    let title = format!(" results — '{}' ", app.search_query.trim());
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(GRAY));

    let status = app.search.lock().unwrap();
    match &*status {
        SearchStatus::Searching => {
            let p = Paragraph::new(vec![
                Line::from(""),
                Line::from(Span::styled(
                    "searching indexers…",
                    Style::default().fg(YELLOW).add_modifier(Modifier::BOLD),
                )),
            ])
            .alignment(Alignment::Center)
            .block(block);
            f.render_widget(p, area);
        }
        SearchStatus::Failed(e) => {
            let p = Paragraph::new(vec![
                Line::from(""),
                Line::from(Span::styled(
                    format!("✗ {}", e),
                    Style::default().fg(RED),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "press 's' to try again, Esc to go back",
                    Style::default().fg(GRAY),
                )),
            ])
            .alignment(Alignment::Center)
            .block(block);
            f.render_widget(p, area);
        }
        SearchStatus::Done(results) if results.is_empty() => {
            let p = Paragraph::new(vec![
                Line::from(""),
                Line::from(Span::styled("no results", Style::default().fg(GRAY))),
            ])
            .alignment(Alignment::Center)
            .block(block);
            f.render_widget(p, area);
        }
        SearchStatus::Done(results) => {
            let rows: Vec<Row> = results
                .iter()
                .map(|r| {
                    let seed_style = if r.seeders >= 20 {
                        Style::default().fg(GREEN)
                    } else if r.seeders > 0 {
                        Style::default().fg(YELLOW)
                    } else {
                        Style::default().fg(RED)
                    };
                    let rating_cell = match r.rating {
                        Some(v) => ratatui::widgets::Cell::from(format!("{:.1}★", v))
                            .style(Style::default().fg(YELLOW)),
                        None => ratatui::widgets::Cell::from(""),
                    };
                    Row::new(vec![
                        ratatui::widgets::Cell::from(r.title.clone()),
                        ratatui::widgets::Cell::from(human_bytes(r.size)),
                        ratatui::widgets::Cell::from(r.seeders.to_string()).style(seed_style),
                        ratatui::widgets::Cell::from(r.leechers.to_string())
                            .style(Style::default().fg(GRAY)),
                        rating_cell,
                        ratatui::widgets::Cell::from(r.indexer.clone())
                            .style(Style::default().fg(GRAY)),
                    ])
                })
                .collect();

            let n = results.len();
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
                Row::new(vec!["title", "size", "seed", "leech", "imdb", "indexer"])
                    .style(Style::default().fg(YELLOW).add_modifier(Modifier::BOLD)),
            )
            .block(block)
            .highlight_style(
                Style::default()
                    .bg(Color::Rgb(0x3c, 0x38, 0x36))
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("▶ ");

            let mut state = TableState::default();
            state.select(Some(app.search_selected.min(n.saturating_sub(1))));
            f.render_stateful_widget(table, area, &mut state);
        }
        SearchStatus::Idle => {
            f.render_widget(block, area);
        }
    }
}

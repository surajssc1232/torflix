mod app;
mod omdb;
mod rqbit;
mod search;
mod ui;

use anyhow::Result;
use app::{App, View};
use crossterm::{
    event::{self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use rqbit::Client;
use std::io;
use std::path::PathBuf;
use std::time::Duration;

fn purge_stale_temp_dirs() {
    let tmp = std::env::temp_dir();
    if let Ok(entries) = std::fs::read_dir(&tmp) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("torflix-") && entry.path().is_dir() {
                std::fs::remove_dir_all(entry.path()).ok();
            }
        }
    }
}

fn download_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("TORFLIX_DOWNLOAD_DIR") {
        return PathBuf::from(dir);
    }
    dirs::video_dir()
        .or_else(dirs::download_dir)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("torflix")
}

fn main() -> Result<()> {
    let api_url =
        std::env::var("TORFLIX_RQBIT_URL").unwrap_or_else(|_| rqbit::DEFAULT_API.to_string());
    let client = Client::new(&api_url);

    // Start the embedded rqbit engine if no external one is running.
    let mut embedded_engine = None;
    if !client.is_up() {
        let engine = rqbit::start_embedded_engine(&download_dir())?;
        embedded_engine = Some(engine);
        let mut ok = false;
        for _ in 0..40 {
            std::thread::sleep(Duration::from_millis(250));
            if client.is_up() {
                ok = true;
                break;
            }
        }
        if !ok {
            if let Some(mut e) = embedded_engine {
                e.stop();
            }
            anyhow::bail!("rqbit engine did not come up on {}", api_url);
        }
    }

    purge_stale_temp_dirs();

    let mut app = App::new(client);
    app.spawn_poller();

    // Add anything passed on the command line (magnet, URL, or .torrent path).
    for arg in std::env::args().skip(1) {
        if arg.starts_with('-') {
            continue;
        }
        let label = arg.clone();
        app.add_and_play_async(&arg, &label);
    }

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableBracketedPaste)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableBracketedPaste);
        default_hook(info);
    }));

    let res = run(&mut terminal, &mut app);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableBracketedPaste)?;
    terminal.show_cursor()?;

    if let Some(mut e) = embedded_engine {
        e.stop();
    }

    res
}

fn run(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, app: &mut App) -> Result<()> {
    loop {
        while let Ok(msg) = app.status_rx.try_recv() {
            app.status = msg;
        }

        if app.view == View::SearchResults {
            app.maybe_fetch_search_ratings();
        }

        terminal.draw(|f| ui::draw(f, app))?;

        if !event::poll(Duration::from_millis(250))? {
            continue;
        }
        match event::read()? {
            Event::Paste(text) => match app.view {
                View::AddInput => app.input.push_str(&text),
                View::Home => app.search_query.push_str(&text),
                _ => {}
            },
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                if key.code == KeyCode::Char('?') {
                    app.show_help = !app.show_help;
                    continue;
                }
                if app.show_help {
                    app.show_help = false;
                    continue;
                }
                let n_rows = app.rows_snapshot().len();
                match app.view {
                    View::Home => {
                        let query_empty = app.search_query.is_empty();
                        match key.code {
                            KeyCode::Enter => app.start_search(),
                            KeyCode::Backspace => { app.search_query.pop(); }
                            KeyCode::Esc => app.search_query.clear(),
                            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                app.search_query.clear();
                            }
                            // Navigation shortcuts only work when search bar is empty
                            KeyCode::Char('q') if query_empty => app.should_quit = true,
                            KeyCode::Char('t') if query_empty => {
                                app.view = View::Torrents;
                                app.status = "a: add magnet/URL  Enter: files  Space: pause  q: quit".into();
                            }
                            KeyCode::Char('a') if query_empty => {
                                app.input.clear();
                                app.view = View::AddInput;
                            }
                            KeyCode::Char(c) => app.search_query.push(c),
                            _ => {}
                        }
                    }
                    View::AddInput => match key.code {
                        KeyCode::Esc => {
                            app.input.clear();
                            app.view = View::Home;
                        }
                        KeyCode::Enter => app.submit_add(),
                        KeyCode::Backspace => { app.input.pop(); }
                        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            app.input.clear();
                        }
                        KeyCode::Char(c) => app.input.push(c),
                        _ => {}
                    },
                    View::SearchResults => match key.code {
                        KeyCode::Esc | KeyCode::Char('h') => {
                            app.view = View::Home;
                        }
                        KeyCode::Char('q') => app.should_quit = true,
                        KeyCode::Char('s') | KeyCode::Char('/') => {
                            app.search_query.clear();
                            app.view = View::Home;
                        }
                        KeyCode::Up | KeyCode::Char('k') => {
                            app.search_selected = app.search_selected.saturating_sub(1);
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            let n = app.search_results_len();
                            if n > 0 {
                                app.search_selected = (app.search_selected + 1).min(n - 1);
                            }
                        }
                        KeyCode::Enter | KeyCode::Char('l') => app.add_search_selected(),
                        KeyCode::Char('d') => app.download_search_selected(),
                        KeyCode::Char('o') => {
                            app.search_sort = app.search_sort.next();
                            app.search_selected = 0;
                        }
                        _ => {}
                    },
                    View::ConfirmDelete => match key.code {
                        KeyCode::Char('y') | KeyCode::Char('Y') => app.confirm_delete(),
                        _ => app.view = View::Torrents,
                    },
                    View::Files => match key.code {
                        KeyCode::Esc | KeyCode::Char('h') | KeyCode::Backspace => {
                            app.view = View::Torrents;
                            app.status = "a: add magnet/URL  Enter: files  Space: pause  q: quit".into();
                        }
                        KeyCode::Char('q') => app.should_quit = true,
                        KeyCode::Up | KeyCode::Char('k') => {
                            app.file_selected = app.file_selected.saturating_sub(1);
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            if !app.files.is_empty() {
                                app.file_selected = (app.file_selected + 1).min(app.files.len() - 1);
                            }
                        }
                        KeyCode::Enter | KeyCode::Char('l') => app.play_selected_file(),
                        KeyCode::Char('p') => app.play_playlist(),
                        _ => {}
                    },
                    View::Torrents => match key.code {
                        KeyCode::Char('q') => app.should_quit = true,
                        KeyCode::Char('Q') => {
                            app.stop_engine_on_quit = true;
                            app.should_quit = true;
                        }
                        KeyCode::Char('a') => {
                            app.input.clear();
                            app.view = View::AddInput;
                        }
                        KeyCode::Char('s') | KeyCode::Char('/') | KeyCode::Esc => {
                            app.search_query.clear();
                            app.view = View::Home;
                        }
                        KeyCode::Up | KeyCode::Char('k') => {
                            app.selected = app.selected.saturating_sub(1);
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            if n_rows > 0 {
                                app.selected = (app.selected + 1).min(n_rows - 1);
                            }
                        }
                        KeyCode::Enter | KeyCode::Char('l') => app.open_files(),
                        KeyCode::Char(' ') => app.toggle_pause(),
                        KeyCode::Char('d') => {
                            if n_rows > 0 {
                                app.delete_with_files = false;
                                app.view = View::ConfirmDelete;
                            }
                        }
                        KeyCode::Char('D') => {
                            if n_rows > 0 {
                                app.delete_with_files = true;
                                app.view = View::ConfirmDelete;
                            }
                        }
                        _ => {}
                    },
                }
                app.clamp_selection(app.rows_snapshot().len());
            }
            _ => {}
        }

        if app.should_quit {
            return Ok(());
        }
    }
}

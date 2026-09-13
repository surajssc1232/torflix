mod app;
mod daemon;
mod history;
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
    // Parse CLI flags before anything else so download_dir() picks up -d.
    let raw_args: Vec<String> = std::env::args().skip(1).collect();
    let mut positional: Vec<String> = Vec::new();
    let mut daemon_mode = false;
    let mut stop_mode = false;
    let mut i = 0;
    while i < raw_args.len() {
        match raw_args[i].as_str() {
            "-d" => {
                i += 1;
                match raw_args.get(i) {
                    Some(path) => std::env::set_var("TORFLIX_DOWNLOAD_DIR", path),
                    None => anyhow::bail!("'-d' requires a path argument"),
                }
            }
            "--daemon" => daemon_mode = true,
            "--stop" => stop_mode = true,
            a if !a.starts_with('-') => positional.push(a.to_string()),
            _ => {}
        }
        i += 1;
    }

    let api_url =
        std::env::var("TORFLIX_RQBIT_URL").unwrap_or_else(|_| rqbit::DEFAULT_API.to_string());

    if daemon_mode {
        return daemon::run(&download_dir(), &api_url);
    }
    if stop_mode {
        if daemon::request_stop() {
            println!("torflix: background downloads stopped — they'll resume next time you open torflix.");
        } else {
            println!("torflix: no background downloads are running.");
        }
        return Ok(());
    }

    let client = Client::new(&api_url);

    // Use an engine that's already running (typically our own background daemon);
    // otherwise start one in-process.
    let mut embedded_engine = None;
    if !client.is_up() {
        daemon::clear_stale();
        embedded_engine = Some(rqbit::start_embedded_engine(&download_dir(), &api_url)?);
        // Only clean up when the engine is ours: another torflix could be
        // streaming through a shared engine right now.
        rqbit::forget_temp_torrents(&client);
        purge_stale_temp_dirs();
    }

    let mut app = App::new(client);
    app.embedded = embedded_engine.is_some();
    if !app.embedded && daemon::is_running() {
        app.status = "connected to background engine — downloads kept going while torflix was closed".into();
    }
    app.spawn_poller();

    // Add magnet/URL/.torrent paths passed on the command line.
    for arg in positional {
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

    let active = app.active_download_count();
    if let Some(mut e) = embedded_engine {
        // Stopping first frees the API port for the daemon and flushes resume state.
        e.stop();
        if app.keep_downloading {
            match daemon::spawn_detached() {
                Ok(()) => println!(
                    "torflix: {} download(s) continuing in the background.\n  \
                     Open torflix to check on them, or run `torflix --stop` to stop them.",
                    active
                ),
                Err(err) => eprintln!(
                    "torflix: couldn't keep downloading in the background ({err:#}).\n  \
                     They'll resume next time you open torflix."
                ),
            }
        } else if active > 0 {
            println!(
                "torflix: {} unfinished download(s) paused — they'll resume next time you open torflix.",
                active
            );
        }
    } else if app.stop_engine_on_quit && daemon::request_stop() {
        println!("torflix: background downloads stopped — they'll resume next time you open torflix.");
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

        app.tick = app.tick.wrapping_add(1);
        terminal.draw(|f| ui::draw(f, app))?;

        if !event::poll(Duration::from_millis(250))? {
            continue;
        }
        match event::read()? {
            Event::Paste(text) => match app.view {
                View::AddInput => app.input.push_str(&text),
                View::Home => app.search_query.push_str(&text),
                View::SearchResults if app.search_filter_active => {
                    app.search_filter.push_str(&text);
                    app.search_selected = 0;
                }
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
                        match key.code {
                            KeyCode::Enter => app.start_search(),
                            KeyCode::Backspace => { app.search_query.pop(); }
                            KeyCode::Esc => app.search_query.clear(),
                            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                app.search_query.clear();
                            }
                            // Tab goes to downloads — Tab never appears in a search query
                            KeyCode::Tab => {
                                app.view = View::Torrents;
                                app.status = "a: add magnet/URL  Enter: files  Space: pause  q: quit".into();
                            }
                            // q quits when the search bar is empty, otherwise types into it
                            KeyCode::Char('q') if app.search_query.is_empty() => {
                                app.request_quit();
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
                    View::SearchResults => {
                        if app.search_preview.is_some() {
                            // Preview panel is open — j/k/Enter/d operate on the file list
                            match key.code {
                                KeyCode::Up | KeyCode::Char('k') => app.preview_up(),
                                KeyCode::Down | KeyCode::Char('j') => app.preview_down(),
                                KeyCode::Enter | KeyCode::Char('l') => app.play_from_preview(),
                                KeyCode::Char('d') => app.download_from_preview(),
                                KeyCode::Char('o') => app.preview_cycle_sort(),
                                KeyCode::Char('f') | KeyCode::Esc | KeyCode::Char('h') => app.close_search_preview(),
                                KeyCode::Char('q') => app.request_quit(),
                                _ => {}
                            }
                        } else if app.search_filter_active {
                            // Filter bar is open — keys type into the filter
                            match key.code {
                                KeyCode::Backspace => {
                                    app.search_filter.pop();
                                    app.search_selected = 0;
                                }
                                KeyCode::Esc => {
                                    app.search_filter.clear();
                                    app.search_filter_active = false;
                                    app.search_selected = 0;
                                }
                                KeyCode::Enter => {
                                    app.search_filter_active = false;
                                }
                                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                    app.search_filter.clear();
                                    app.search_selected = 0;
                                }
                                KeyCode::Char('/') => {
                                    app.search_filter_active = false;
                                }
                                KeyCode::Char(c) => {
                                    app.search_filter.push(c);
                                    app.search_selected = 0;
                                }
                                _ => {}
                            }
                        } else {
                            // Normal navigation
                            match key.code {
                                KeyCode::Char('/') => {
                                    app.search_filter_active = true;
                                }
                                KeyCode::Esc | KeyCode::Char('h') => {
                                    app.view = View::Home;
                                }
                                KeyCode::Char('q') => app.request_quit(),
                                KeyCode::Char('s') => {
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
                                KeyCode::Char('f') => {
                                    app.search_filter_active = false;
                                    app.open_search_preview();
                                }
                                KeyCode::Char('o') => {
                                    app.search_sort = app.search_sort.next();
                                    app.search_selected = 0;
                                }
                                _ => {}
                            }
                        }
                    }
                    View::ConfirmDelete => match key.code {
                        KeyCode::Char('y') | KeyCode::Char('Y') => app.confirm_delete(),
                        _ => app.view = View::Torrents,
                    },
                    View::ConfirmQuit => match key.code {
                        KeyCode::Char('y') | KeyCode::Char('Y') => {
                            app.keep_downloading = true;
                            app.should_quit = true;
                        }
                        KeyCode::Char('n') | KeyCode::Char('N') => app.should_quit = true,
                        _ => app.view = app.quit_return_view.clone(),
                    },
                    View::History => match key.code {
                        KeyCode::Esc | KeyCode::Char('s') | KeyCode::Tab => app.view = View::Home,
                        KeyCode::Char('q') => app.request_quit(),
                        KeyCode::Up | KeyCode::Char('k') => {
                            app.history_selected = app.history_selected.saturating_sub(1);
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            if !app.history.is_empty() {
                                app.history_selected = (app.history_selected + 1).min(app.history.len() - 1);
                            }
                        }
                        KeyCode::Enter | KeyCode::Char('l') => app.history_play(),
                        KeyCode::Char('d') => app.history_download(),
                        KeyCode::Char('x') => app.history_remove(),
                        _ => {}
                    },
                    View::Files => match key.code {
                        KeyCode::Esc | KeyCode::Char('h') | KeyCode::Backspace => {
                            app.view = View::Torrents;
                            app.status = "a: add magnet/URL  Enter: files  Space: pause  q: quit".into();
                        }
                        KeyCode::Char('q') => app.request_quit(),
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
                        KeyCode::Char('q') => app.request_quit(),
                        KeyCode::Tab | KeyCode::Char('H') => app.open_history(),
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

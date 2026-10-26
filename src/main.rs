mod app;
mod commands;
mod config;
mod daemon;
mod favorites;
mod history;
mod letterboxd;
mod omdb;
mod picker;
mod posters;
mod progress;
mod rqbit;
mod search;
mod signals;
mod sixel;
mod stremio;
mod term;
mod tv;
mod ui;

use anyhow::Result;
use app::{App, HomeFocus, InputPurpose, View};
use crossterm::{
    event::{self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use rqbit::Client;
use std::io::{self, Write};
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

/// The engine's default output folder. Same place downloads go, so there's one
/// folder to look in rather than two.
fn download_dir() -> PathBuf {
    app::download_dir()
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
    // The app owns the engine from here: it may start a replacement if the one
    // we attached to goes away.
    app.engine = embedded_engine;
    if !app.embedded && daemon::is_running() {
        app.status = "connected to background engine — downloads kept going while torflix was closed".into();
    }
    app.spawn_poller();
    // The home screen shows covers from the first browse list; start fetching now.
    app.load_home(0);

    // Add magnet/URL/.torrent paths passed on the command line.
    for arg in positional {
        let label = arg.clone();
        app.add_and_play_async(&arg, &label);
    }

    // Closing the terminal must hand the downloads over, not drop them.
    signals::listen();

    enable_raw_mode()?;
    // Before anything else reads stdin: the terminal answers on it.
    app.caps = term::probe();
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

    // Cosmetic, and the terminal may already be gone — never let tidying up
    // stand between a hangup and the handoff below.
    let _ = disable_raw_mode();
    let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableBracketedPaste);
    let _ = terminal.show_cursor();

    let active = app.active_download_count();
    if let Some(mut e) = app.engine.take() {
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

/// Write the covers the frame left room for.
///
/// ratatui only sends the cells that changed, so a picture written over empty
/// cells stays put between frames and only the ones that moved, arrived or
/// went away are written again. Popups are the exception: they draw text
/// across the grid, so the screen is repainted from scratch while one is up
/// and the pictures come back when it closes.
/// Answers whether the screen was thrown away and wants drawing again now.
fn flush_images(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
) -> Result<bool> {
    let wanted: Vec<app::Placed> = std::mem::take(&mut app.frame_images.borrow_mut());
    if app.overlay_up() {
        if !app.shown_images.is_empty() {
            terminal.clear()?;
            app.shown_images.clear();
            return Ok(true);
        }
        return Ok(false);
    }
    // A picture that is no longer wanted, or has moved, leaves a hole the
    // frame below it doesn't know to fill.
    let stale = app
        .shown_images
        .iter()
        .any(|was| !wanted.iter().any(|now| now == was));
    if stale {
        terminal.clear()?;
        app.shown_images.clear();
        return Ok(true);
    }
    let fresh: Vec<&app::Placed> = wanted
        .iter()
        .filter(|now| !app.shown_images.contains(now))
        .collect();
    if !fresh.is_empty() {
        let out = terminal.backend_mut();
        for img in fresh {
            if let posters::Art::Sixel(data) = &img.data.art {
                // Put the cursor on the cover's top-left cell and draw.
                write!(out, "\x1b[{};{}H{}", img.row, img.col, data)?;
            }
        }
        out.flush()?;
    }
    app.shown_images = wanted;
    Ok(false)
}

fn run(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, app: &mut App) -> Result<()> {
    loop {
        while let Ok(msg) = app.status_rx.try_recv() {
            app.status = msg;
        }
        app.poll_background();

        if app.view == View::SearchResults {
            app.maybe_fetch_search_ratings();
        }

        if signals::caught() {
            // The terminal is going away. Leave quietly, keeping whatever is
            // still downloading alive in the background.
            app.keep_downloading = app.embedded && app.active_download_count() > 0;
            return Ok(());
        }

        app.tick = app.tick.wrapping_add(1);
        terminal.draw(|f| ui::draw(f, app))?;
        // A cover that moved takes the whole screen with it; draw it back
        // straight away rather than leaving a gap until the next tick.
        if flush_images(terminal, app)? {
            terminal.draw(|f| ui::draw(f, app))?;
            flush_images(terminal, app)?;
        }

        let ready = match event::poll(Duration::from_millis(250)) {
            Ok(ready) => ready,
            // A signal cuts the wait short; the check at the top of the loop
            // decides what to make of it.
            Err(_) if signals::caught() => continue,
            Err(e) => return Err(e.into()),
        };
        if !ready {
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
                View::Tv if app.tv_filter_active => {
                    app.tv_filter.push_str(&text);
                    app.tv_selected = 0;
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
                // History and downloads are reachable from anywhere.
                if key.modifiers.contains(KeyModifiers::CONTROL) {
                    match key.code {
                        // Terminals send Ctrl+H as backspace and Ctrl+J as a newline;
                        // match those too for the ones that pass the modifier along.
                        KeyCode::Char('h') | KeyCode::Backspace => {
                            if app.view == View::AddInput {
                                app.cancel_input();
                            }
                            app.open_history();
                            continue;
                        }
                        KeyCode::Char('j') | KeyCode::Enter => {
                            if app.view == View::AddInput {
                                app.cancel_input();
                            }
                            app.view = View::Torrents;
                            continue;
                        }
                        _ => {}
                    }
                }
                let n_rows = app.rows_snapshot().len();
                match app.view {
                    View::Home if app.home_focus == HomeFocus::Browse => match key.code {
                        KeyCode::Up => app.home_move(-(app.grid_cols.get() as isize)),
                        KeyCode::Down => app.home_move(app.grid_cols.get() as isize),
                        KeyCode::Left => app.home_move(-1),
                        KeyCode::Right => app.home_move(1),
                        KeyCode::Enter => app.open_home_selected(),
                        KeyCode::Tab => app.home_next_list(),
                        KeyCode::Char('f') => app.toggle_favorite(),
                        KeyCode::Char('d') => app.download_catalog_selected(false),
                        KeyCode::Char('D') => app.download_catalog_selected(true),
                        KeyCode::Esc => app.home_focus = HomeFocus::Search,
                        // Anything else typed belongs in the search box, so go back to it.
                        KeyCode::Char(c) => {
                            app.home_focus = HomeFocus::Search;
                            app.search_query.push(c);
                        }
                        KeyCode::Backspace => {
                            app.home_focus = HomeFocus::Search;
                            app.search_query.pop();
                        }
                        _ => {}
                    },
                    View::Home => {
                        match key.code {
                            KeyCode::Down => {
                                app.focus_browse();
                            }
                            KeyCode::Enter => app.submit_home(),
                            KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                app.toggle_search_source();
                            }
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
                        KeyCode::Esc => app.cancel_input(),
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
                                KeyCode::Char('D') => app.prompt_download_dest(),
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
                                KeyCode::Char('D') => app.prompt_download_dest(),
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
                        KeyCode::Up | KeyCode::Char('k') => app.delete_move(-1),
                        KeyCode::Down | KeyCode::Char('j') => app.delete_move(1),
                        KeyCode::Enter => app.confirm_delete_choice(),
                        // The answers also have a letter each, for the hurried.
                        KeyCode::Char('y') | KeyCode::Char('Y') => {
                            app.delete_choice = 0;
                            app.confirm_delete_choice();
                        }
                        KeyCode::Char('d') | KeyCode::Char('D') => {
                            app.delete_choice = 1;
                            app.confirm_delete_choice();
                        }
                        _ => app.view = View::Torrents,
                    },
                    View::Discover => match key.code {
                        KeyCode::Esc | KeyCode::Char('h') => app.view = View::Home,
                        KeyCode::Char('q') => app.request_quit(),
                        KeyCode::Up | KeyCode::Char('k') => {
                            app.catalog_move(-(app.grid_cols.get() as isize));
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            app.catalog_move(app.grid_cols.get() as isize);
                        }
                        KeyCode::Left => app.catalog_move(-1),
                        KeyCode::Right => app.catalog_move(1),
                        KeyCode::Enter | KeyCode::Char('l') => app.open_details(),
                        KeyCode::Char('d') => app.download_catalog_selected(false),
                        KeyCode::Char('D') => app.download_catalog_selected(true),
                        KeyCode::Char('f') => app.toggle_favorite(),
                        KeyCode::Char('b') => app.next_browse(),
                        _ => {}
                    },
                    View::PickFolder => match key.code {
                        KeyCode::Up | KeyCode::Char('k') => app.picker_move(-1),
                        KeyCode::Down | KeyCode::Char('j') => app.picker_move(1),
                        KeyCode::PageUp => app.picker_move(-10),
                        KeyCode::PageDown => app.picker_move(10),
                        KeyCode::Home => app.picker_move(isize::MIN / 2),
                        KeyCode::End => app.picker_move(isize::MAX / 2),
                        KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => app.picker_enter(),
                        KeyCode::Left | KeyCode::Char('h') | KeyCode::Backspace => app.picker_up(),
                        KeyCode::Char('u') | KeyCode::Char(' ') => app.picker_use_this(),
                        KeyCode::Char('t') => app.picker_to_input(),
                        KeyCode::Esc | KeyCode::Char('q') => app.cancel_picker(),
                        _ => {}
                    },
                    View::Details => match key.code {
                        KeyCode::Esc | KeyCode::Char('h') | KeyCode::Backspace => app.details_back(),
                        KeyCode::Char('q') => app.request_quit(),
                        KeyCode::Up | KeyCode::Char('k') => app.details_move(false),
                        KeyCode::Down | KeyCode::Char('j') => app.details_move(true),
                        KeyCode::Tab | KeyCode::Right => app.details_pane(true),
                        KeyCode::BackTab | KeyCode::Left => app.details_pane(false),
                        KeyCode::Enter | KeyCode::Char('l') => app.details_enter(),
                        KeyCode::Char('d') => app.download_selected_stream(),
                        KeyCode::Char('D') => app.prompt_download_dest(),
                        KeyCode::Char('f') => app.toggle_favorite(),
                        KeyCode::Char('t') => app.search_torrents_for_details(),
                        _ => {}
                    },
                    View::Tv if app.tv_filter_active => match key.code {
                        KeyCode::Esc => {
                            app.tv_filter.clear();
                            app.tv_filter_active = false;
                            app.tv_selected = 0;
                        }
                        KeyCode::Enter => app.tv_filter_active = false,
                        KeyCode::Backspace => {
                            app.tv_filter.pop();
                            app.tv_selected = 0;
                        }
                        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            app.tv_filter.clear();
                            app.tv_selected = 0;
                        }
                        KeyCode::Char(c) => {
                            app.tv_filter.push(c);
                            app.tv_selected = 0;
                        }
                        _ => {}
                    },
                    View::Tv if app.tv_manage => match key.code {
                        KeyCode::Esc | KeyCode::Char('m') | KeyCode::Char('h') => app.tv_manage = false,
                        KeyCode::Char('q') => app.request_quit(),
                        KeyCode::Up | KeyCode::Char('k') => {
                            app.tv_playlist_selected = app.tv_playlist_selected.saturating_sub(1);
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            let n = app.tv_playlists.len();
                            if n > 0 {
                                app.tv_playlist_selected = (app.tv_playlist_selected + 1).min(n - 1);
                            }
                        }
                        KeyCode::Char('a') => app.open_input(InputPurpose::Playlist),
                        KeyCode::Char('x') | KeyCode::Delete => app.remove_playlist(),
                        _ => {}
                    },
                    View::Tv => match key.code {
                        KeyCode::Esc | KeyCode::Char('h') => app.view = View::Home,
                        KeyCode::Char('q') => app.request_quit(),
                        KeyCode::Char('/') => app.tv_filter_active = true,
                        KeyCode::Up | KeyCode::Char('k') => app.tv_selected = app.tv_selected.saturating_sub(1),
                        KeyCode::Down | KeyCode::Char('j') => {
                            let n = app.tv_visible_len();
                            if n > 0 {
                                app.tv_selected = (app.tv_selected + 1).min(n - 1);
                            }
                        }
                        KeyCode::Enter | KeyCode::Char('l') => app.play_channel(),
                        KeyCode::Char('a') => app.open_input(InputPurpose::Playlist),
                        KeyCode::Char('m') => app.tv_manage = true,
                        KeyCode::Char('r') => app.reload_tv(),
                        _ => {}
                    },
                    View::Addons => match key.code {
                        KeyCode::Esc | KeyCode::Char('h') => app.view = View::Home,
                        KeyCode::Char('q') => app.request_quit(),
                        KeyCode::Up | KeyCode::Char('k') => app.addon_selected = app.addon_selected.saturating_sub(1),
                        KeyCode::Down | KeyCode::Char('j') => {
                            let n = app.addons.len();
                            if n > 0 {
                                app.addon_selected = (app.addon_selected + 1).min(n - 1);
                            }
                        }
                        KeyCode::Char(' ') | KeyCode::Enter => app.toggle_addon(),
                        KeyCode::Char('a') => app.open_input(InputPurpose::Addon),
                        KeyCode::Char('x') | KeyCode::Delete => app.remove_addon(),
                        _ => {}
                    },
                    View::Settings => match key.code {
                        KeyCode::Esc | KeyCode::Char('h') => app.view = View::Home,
                        KeyCode::Char('q') => app.request_quit(),
                        KeyCode::Up | KeyCode::Char('k') => {
                            app.settings_selected = app.settings_selected.saturating_sub(1);
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            app.settings_selected = (app.settings_selected + 1).min(App::SETTINGS_ROWS - 1);
                        }
                        KeyCode::Enter | KeyCode::Char(' ') | KeyCode::Right | KeyCode::Char('l') => {
                            app.settings_change(true);
                        }
                        KeyCode::Left => app.settings_change(false),
                        _ => {}
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
                        KeyCode::Char('D') => app.prompt_download_dest(),
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
                        KeyCode::Char('a') => app.open_input(InputPurpose::Torrent),
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
                        KeyCode::Char('d') => app.ask_delete(false),
                        KeyCode::Char('D') => app.ask_delete(true),
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

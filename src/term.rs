//! What this terminal can do.
//!
//! Covers are drawn as real pictures where the terminal supports sixel, and as
//! half-blocks everywhere else. Two things have to be known before the first
//! frame: whether sixel is understood, and how many pixels a character cell is,
//! since a cover has to be rendered to fit an exact number of cells.
//!
//! Sixel support is asked for with a Primary Device Attributes query — the
//! terminal answers `\e[?…;4;…c`, and the `4` is the sixel bit. The cell size
//! comes from the window size the kernel already knows.

/// Set `TORFLIX_SIXEL=0` to force half-blocks, `=1` to insist on pictures.
const OVERRIDE: &str = "TORFLIX_SIXEL";

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Caps {
    /// Pictures can be drawn, and `cell` is known.
    pub sixel: bool,
    /// Pixels per character cell, (width, height).
    pub cell: Option<(u16, u16)>,
}

/// Ask the terminal what it can do. Call this once, in raw mode, before
/// anything else reads a key — the answer arrives on stdin like one.
pub fn probe() -> Caps {
    let cell = cell_size();
    let sixel = match std::env::var(OVERRIDE).ok().as_deref() {
        Some("0") | Some("no") | Some("false") => false,
        Some("1") | Some("yes") | Some("true") => true,
        // Multiplexers don't forward sixel, and a reply would come from the
        // multiplexer rather than the terminal drawing the pixels.
        _ if std::env::var_os("TMUX").is_some() => false,
        _ if std::env::var("TERM").is_ok_and(|t| t.starts_with("screen")) => false,
        _ => has_sixel(&ask("\x1b[c", 300)),
    };
    Caps { sixel: sixel && cell.is_some(), cell }
}

/// Pixels per cell, from the window size the terminal reports. Terminals that
/// don't report a pixel size give 0, which tells us nothing.
fn cell_size() -> Option<(u16, u16)> {
    let w = crossterm::terminal::window_size().ok()?;
    let (cols, rows) = (w.columns, w.rows);
    if w.width == 0 || w.height == 0 || cols == 0 || rows == 0 {
        return None;
    }
    Some((w.width / cols, w.height / rows))
}

/// `\e[?62;4;22c` — the attributes are the numbers between `?` and `c`, and
/// attribute 4 is sixel.
fn has_sixel(reply: &str) -> bool {
    let Some(rest) = reply.split_once("\x1b[?") else {
        return false;
    };
    let Some((params, _)) = rest.1.split_once('c') else {
        return false;
    };
    params.split(';').any(|p| p == "4")
}

/// Write a query and read what comes back, giving up after `ms`. Anything
/// typed while waiting is returned with the reply and would be lost, which is
/// why this runs before the key loop starts.
#[cfg(unix)]
fn ask(query: &str, ms: u64) -> String {
    use std::io::Write;
    use std::time::{Duration, Instant};

    let mut out = std::io::stdout();
    if out.write_all(query.as_bytes()).is_err() || out.flush().is_err() {
        return String::new();
    }
    let deadline = Instant::now() + Duration::from_millis(ms);
    let mut buf = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        let mut pfd = libc::pollfd { fd: 0, events: libc::POLLIN, revents: 0 };
        if unsafe { libc::poll(&mut pfd, 1, left.as_millis() as libc::c_int) } <= 0 {
            break;
        }
        let mut chunk = [0u8; 256];
        let n = unsafe { libc::read(0, chunk.as_mut_ptr().cast(), chunk.len()) };
        if n <= 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n as usize]);
        // `c` ends a device-attributes reply; there's nothing more to wait for.
        if buf.contains(&b'c') {
            break;
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

#[cfg(not(unix))]
fn ask(_query: &str, _ms: u64) -> String {
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attribute_four_means_sixel() {
        assert!(has_sixel("\x1b[?62;4;22c"));
        assert!(has_sixel("\x1b[?4c"));
        assert!(!has_sixel("\x1b[?62;22c"));
        // 14 is not 4, and 40 is not 4 either.
        assert!(!has_sixel("\x1b[?14;40c"));
    }

    #[test]
    fn a_reply_that_never_came_is_not_support() {
        assert!(!has_sixel(""));
        assert!(!has_sixel("\x1b[?62;4;22"));
        assert!(!has_sixel("rubbish"));
    }
}

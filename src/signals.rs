//! Noticing that the terminal is going away.
//!
//! Closing a terminal window sends SIGHUP to the processes in it, and a
//! logout or `kill` sends SIGTERM. Either way torflix would simply die, taking
//! its embedded engine — and every download in it — down with it.
//!
//! A handler can't do the handoff itself: spawning processes and stopping an
//! engine are not things you may do inside a signal handler. So the handler
//! does the one thing that is always safe, setting a flag, and the draw loop
//! picks it up and quits the ordinary way, which hands the downloads to the
//! background daemon on the way out.
//!
//! SIGTERM is the exception to the handoff: it means the session or the whole
//! machine is going down. A daemon started now would miss that signal, and
//! systemd would then sit waiting for it until its stop timeout ran out.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

static CAUGHT: AtomicBool = AtomicBool::new(false);
static TERMINATING: AtomicBool = AtomicBool::new(false);

/// How long quitting may take once a signal has arrived, before giving up on
/// tidiness. Fastresume state can be rebuilt; a held-up shutdown can't be undone.
const GRACE: Duration = Duration::from_secs(10);

/// True once a signal asking torflix to go away has arrived.
pub fn caught() -> bool {
    CAUGHT.load(Ordering::SeqCst)
}

/// True if that signal was SIGTERM, so nothing should be left running.
pub fn terminating() -> bool {
    TERMINATING.load(Ordering::SeqCst)
}

/// The terminal hung up without a signal reaching us. Same meaning as SIGHUP.
pub fn terminal_gone() {
    CAUGHT.store(true, Ordering::SeqCst);
}

/// Start listening. Call once, before the draw loop.
#[cfg(unix)]
pub fn listen() {
    for sig in [libc::SIGHUP, libc::SIGTERM, libc::SIGINT] {
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = on_signal as *const () as usize;
            // No SA_RESTART: the wait for a keypress should break off at once
            // rather than resume, so the flag is acted on straight away.
            libc::sigemptyset(&mut action.sa_mask);
            libc::sigaction(sig, &action, std::ptr::null_mut());
        }
    }
    std::thread::spawn(|| {
        while !caught() {
            std::thread::sleep(Duration::from_millis(250));
        }
        std::thread::sleep(GRACE);
        unsafe { libc::_exit(0) };
    });
}

#[cfg(not(unix))]
pub fn listen() {}

/// Setting an atomic is one of the few things a handler may safely do.
#[cfg(unix)]
extern "C" fn on_signal(sig: libc::c_int) {
    if sig == libc::SIGTERM {
        TERMINATING.store(true, Ordering::SeqCst);
    }
    CAUGHT.store(true, Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(unix)]
    fn a_hangup_is_noticed_without_killing_us() {
        assert!(!caught(), "nothing has been raised yet");
        listen();
        // Without a handler this would end the test process.
        assert_eq!(unsafe { libc::raise(libc::SIGHUP) }, 0);
        assert!(caught());
        CAUGHT.store(false, Ordering::SeqCst);
    }
}

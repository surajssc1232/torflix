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

use std::sync::atomic::{AtomicBool, Ordering};

static CAUGHT: AtomicBool = AtomicBool::new(false);

/// True once a signal asking torflix to go away has arrived.
pub fn caught() -> bool {
    CAUGHT.load(Ordering::SeqCst)
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
}

#[cfg(not(unix))]
pub fn listen() {}

/// Setting an atomic is one of the few things a handler may safely do.
#[cfg(unix)]
extern "C" fn on_signal(_sig: libc::c_int) {
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

//! Runtime permission for diagnostic output. Saved photo/review writes never use this gate.
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Mutex,
};

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct StartupNote {
    pub(crate) timestamp_ms: u128,
    pub(crate) message: String,
}
#[derive(Default)]
pub(crate) struct StartupNotes {
    pub(crate) entries: Vec<StartupNote>,
    pub(crate) dropped: usize,
    bytes: usize,
}
impl StartupNotes {
    pub(crate) fn replay(self, enabled: bool, mut emit: impl FnMut(&StartupNote)) -> usize {
        if !enabled {
            return 0;
        }
        for note in &self.entries {
            emit(note);
        }
        self.dropped
    }
}
thread_local! {
    static STARTUP_NOTES: std::cell::RefCell<Option<StartupNotes>> = const { std::cell::RefCell::new(None) };
}
struct CaptureGuard(Option<StartupNotes>);
impl Drop for CaptureGuard {
    fn drop(&mut self) {
        STARTUP_NOTES.with(|slot| *slot.borrow_mut() = self.0.take());
    }
}

/// The preference is itself loaded here, so hold only this thread's bounded
/// settings-load diagnostics until the caller knows whether recording is allowed.
pub(crate) fn capture_startup<T>(load: impl FnOnce() -> T) -> (T, StartupNotes) {
    let guard = CaptureGuard(STARTUP_NOTES.with(|s| s.replace(Some(StartupNotes::default()))));
    let value = load();
    let notes = STARTUP_NOTES.with(|s| s.borrow_mut().take().unwrap_or_default());
    drop(guard);
    (value, notes)
}

pub(crate) fn capture_note(timestamp_ms: u128, message: &str) -> bool {
    STARTUP_NOTES.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(notes) = slot.as_mut() else {
            return false;
        };
        if notes.entries.len() < 64 && message.len() <= 65_536 - notes.bytes {
            notes.entries.push(StartupNote {
                timestamp_ms,
                message: message.to_owned(),
            });
            notes.bytes += message.len();
        } else {
            notes.dropped += 1;
        }
        true
    })
}
pub(crate) fn startup_capture_active() -> bool {
    STARTUP_NOTES.with(|slot| slot.borrow().is_some())
}

struct State {
    started: bool,
}

pub(crate) struct LogGate {
    // Low bit is permission; other bits identify an enabled interval. A producer
    // reads both together without taking the mutex held by the disk writer.
    stamp: AtomicU64,
    state: Mutex<State>,
    console: Mutex<()>,
}

impl LogGate {
    pub(crate) const fn new() -> Self {
        Self {
            stamp: AtomicU64::new(0),
            state: Mutex::new(State { started: false }),
            console: Mutex::new(()),
        }
    }

    pub(crate) fn enabled(&self) -> bool {
        self.stamp.load(Ordering::Acquire) & 1 != 0
    }

    /// Returns true only on an actual state change. First enable initializes the
    /// session file; subsequent enables append. `start` must not call the logger.
    pub(crate) fn set_enabled(&self, enabled: bool, start: impl FnOnce()) -> bool {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let old = self.stamp.load(Ordering::Acquire);
        if (old & 1 != 0) == enabled {
            return false;
        }
        if enabled && !state.started {
            start();
            state.started = true;
        }
        // Echoes never take the disk lock. Only a switch waits for both outputs,
        // ensuring Off has stopped file appends and console echoes when it returns.
        let _console = self.console.lock().unwrap_or_else(|e| e.into_inner());
        let next = (old & !1).wrapping_add(2) | u64::from(enabled);
        self.stamp.store(next, Ordering::Release);
        true
    }

    pub(crate) fn ticket(&self) -> Option<u64> {
        let stamp = self.stamp.load(Ordering::Acquire);
        (stamp & 1 != 0).then_some(stamp)
    }

    pub(crate) fn echo(&self, ticket: u64, write: impl FnOnce()) {
        let _console = self.console.lock().unwrap_or_else(|e| e.into_inner());
        if self.ticket() == Some(ticket) {
            write();
        }
    }

    /// Synchronize Off with the actual output, not merely queue submission. A
    /// queued line from a previous enabled interval cannot reappear after Off→On.
    pub(crate) fn emit(&self, ticket: u64, write: impl FnOnce()) {
        let _state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if self.ticket() == Some(ticket) {
            write();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{mpsc, Arc};

    // Y2 falsifier: bypass capture or keep it active after load. The original
    // timestamps must survive and runtime logs must not remain in this buffer.
    #[test]
    fn startup_notes_are_scoped_bounded_and_keep_their_event_times() {
        let (value, notes) = capture_startup(|| {
            assert!(capture_note(17, "settings migration"));
            assert!(capture_note(19, "panel migration"));
            for _ in 0..70 {
                assert!(capture_note(20, "x"));
            }
            42
        });
        assert_eq!(value, 42);
        assert_eq!(
            notes.entries[0],
            StartupNote {
                timestamp_ms: 17,
                message: "settings migration".into()
            }
        );
        assert_eq!(notes.entries.len(), 64);
        assert_eq!(notes.dropped, 8);
        assert!(!capture_note(30, "runtime"));
        let (_, large) = capture_startup(|| assert!(capture_note(1, &"x".repeat(65_537))));
        assert!(large.entries.is_empty());
        assert_eq!(large.dropped, 1);
    }

    #[test]
    fn startup_capture_does_not_leak_after_a_failed_load() {
        let result = std::panic::catch_unwind(|| {
            capture_startup(|| {
                capture_note(1, "before error");
                panic!("synthetic settings failure");
            })
        });
        assert!(result.is_err());
        assert!(!capture_note(2, "after error"));
    }

    // Y2 falsifier: replay irrespective of the saved opt-in, or discard all
    // captured lines. Assert the output callback and original event timestamp.
    #[test]
    fn captured_startup_output_obeys_the_loaded_preference() {
        for enabled in [false, true] {
            let (_, notes) = capture_startup(|| assert!(capture_note(17, "migration")));
            let mut output = Vec::new();
            assert_eq!(
                notes.replay(enabled, |n| output
                    .push((n.timestamp_ms, n.message.clone()))),
                0
            );
            assert_eq!(
                output,
                if enabled {
                    vec![(17, "migration".into())]
                } else {
                    vec![]
                }
            );
        }
    }

    // Falsifier: enable by default or call start when disabling; old files change.
    #[test]
    fn off_never_initializes_output_and_reenable_does_not_restart_session() {
        let gate = LogGate::new();
        let mut starts = 0;
        assert_eq!(gate.ticket(), None);
        assert!(!gate.set_enabled(false, || starts += 1));
        assert_eq!(starts, 0);
        assert!(gate.set_enabled(true, || starts += 1));
        assert!(gate.enabled());
        gate.set_enabled(false, || starts += 1);
        gate.set_enabled(true, || starts += 1);
        assert_eq!(
            starts, 1,
            "only the first opt-in may rotate the session log"
        );
    }

    // Falsifier: remove the write-time permission or generation check.
    #[test]
    fn queued_messages_cannot_write_after_off_or_cross_an_off_on_boundary() {
        let gate = LogGate::new();
        gate.set_enabled(true, || {});
        let old = gate.ticket().unwrap();
        let mut lines = Vec::new();
        gate.emit(old, || lines.push("before"));
        gate.set_enabled(false, || {});
        gate.emit(old, || lines.push("off"));
        gate.set_enabled(true, || {});
        gate.emit(old, || lines.push("stale"));
        gate.emit(gate.ticket().unwrap(), || lines.push("after"));
        assert_eq!(lines, ["before", "after"]);
    }

    // Falsifier: drop the state lock before the output closure; Off returns while
    // an append is still allowed to finish later.
    #[test]
    fn turning_off_waits_for_an_append_already_in_progress() {
        let gate = Arc::new(LogGate::new());
        gate.set_enabled(true, || {});
        let ticket = gate.ticket().unwrap();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let g = gate.clone();
        let writer = std::thread::spawn(move || {
            g.emit(ticket, || {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            })
        });
        entered_rx.recv().unwrap();
        assert!(
            gate.state.try_lock().is_err(),
            "output must hold the permission lock"
        );
        release_tx.send(()).unwrap();
        gate.set_enabled(false, || {});
        writer.join().unwrap();
        assert_eq!(gate.ticket(), None);
    }

    // O1 falsifier: take the file-output lock in ticket(). The producer then
    // cannot report its ticket until the simulated slow append is released.
    #[test]
    fn ticket_does_not_wait_for_a_file_append() {
        let gate = Arc::new(LogGate::new());
        gate.set_enabled(true, || {});
        let ticket = gate.ticket().unwrap();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let writer_gate = gate.clone();
        let writer = std::thread::spawn(move || {
            writer_gate.emit(ticket, || {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            })
        });
        entered_rx.recv().unwrap();
        let (answer_tx, answer_rx) = mpsc::channel();
        let producer_gate = gate.clone();
        let producer = std::thread::spawn(move || answer_tx.send(producer_gate.ticket()).unwrap());
        let answer = answer_rx.recv_timeout(std::time::Duration::from_secs(1));
        release_tx.send(()).unwrap();
        writer.join().unwrap();
        producer.join().unwrap();
        assert_eq!(
            answer.ok().flatten(),
            Some(ticket),
            "logging producer waited for file I/O"
        );
    }

    // Falsifier: implement echo through emit (the disk lock). Routine console
    // output must finish while the file writer is deliberately held open.
    #[test]
    fn console_echo_does_not_wait_for_file_io_and_rejects_old_tickets() {
        let gate = Arc::new(LogGate::new());
        gate.set_enabled(true, || {});
        let ticket = gate.ticket().unwrap();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let g = gate.clone();
        let writer = std::thread::spawn(move || {
            g.emit(ticket, || {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            })
        });
        entered_rx.recv().unwrap();
        let (echo_tx, echo_rx) = mpsc::channel();
        let g = gate.clone();
        let echo = std::thread::spawn(move || g.echo(ticket, || echo_tx.send(()).unwrap()));
        let result = echo_rx.recv_timeout(std::time::Duration::from_secs(1));
        release_tx.send(()).unwrap();
        writer.join().unwrap();
        echo.join().unwrap();
        assert!(result.is_ok(), "console output waited for disk I/O");
        gate.set_enabled(false, || {});
        gate.set_enabled(true, || {});
        gate.echo(ticket, || panic!("old console line crossed Off/On"));
    }
}

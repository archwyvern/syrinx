//! The watchdog: terminates an isolate's execution when a budget runs out. One per isolate,
//! armed for the setup budget, then armed around every block of a stream and disarmed
//! between blocks, so time a consumer spends not pulling is never charged to the source.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Terminates the isolate's execution if a deadline passes while armed. One per isolate; armed
/// for the setup budget, disarmed once setup is over, then armed around every block.
pub(super) struct Watchdog {
    commands: Sender<WatchdogCommand>,
    acks: Receiver<()>,
    fired: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

enum WatchdogCommand {
    Arm(Instant),
    Disarm,
}

impl Watchdog {
    pub(super) fn start(handle: v8::IsolateHandle) -> Self {
        let (commands, inbox) = mpsc::channel::<WatchdogCommand>();
        let (ack, acks) = mpsc::channel::<()>();
        let fired = Arc::new(AtomicBool::new(false));
        let flag = fired.clone();
        let thread = std::thread::spawn(move || {
            let mut deadline: Option<Instant> = None;
            loop {
                let message = match deadline {
                    None => inbox.recv().map_err(|_| RecvTimeoutError::Disconnected),
                    Some(at) => inbox.recv_timeout(at.saturating_duration_since(Instant::now())),
                };
                match message {
                    Ok(WatchdogCommand::Arm(at)) => deadline = Some(at),
                    Ok(WatchdogCommand::Disarm) => {
                        deadline = None;
                        let _ = ack.send(());
                    }
                    Err(RecvTimeoutError::Timeout) => {
                        flag.store(true, Ordering::SeqCst);
                        handle.terminate_execution();
                        deadline = None;
                    }
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
        });
        Self { commands, acks, fired, thread: Some(thread) }
    }

    pub(super) fn arm(&self, budget: Duration) {
        self.fired.store(false, Ordering::SeqCst);
        let _ = self.commands.send(WatchdogCommand::Arm(Instant::now() + budget));
    }

    /// Stops the clock and says whether it went off while armed. Synchronous: the watchdog
    /// acknowledges, so a termination that raced the disarm is seen here, not on the next call.
    pub(super) fn disarm(&self) -> bool {
        if self.commands.send(WatchdogCommand::Disarm).is_ok() {
            let _ = self.acks.recv();
        }
        self.fired.load(Ordering::SeqCst)
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        // Closing the command channel ends the thread's loop.
        let (dead, _) = mpsc::channel();
        drop(std::mem::replace(&mut self.commands, dead));
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// What a `with_source` body gets besides the scope: the isolate's watchdog and its handle.
pub(super) struct Guard {
    pub(super) watchdog: Watchdog,
    pub(super) handle: v8::IsolateHandle,
}

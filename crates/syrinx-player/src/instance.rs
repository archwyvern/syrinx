//! One player per user: a later launch hands its paths to the running one and exits, so
//! double-clicking a second track replaces what is playing instead of opening a second window.
//!
//! The channel is a local socket (an abstract Unix socket on Linux, a named pipe on Windows;
//! neither leaves a stale file behind). The protocol is one JSON line each way.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};

use interprocess::local_socket::{
    GenericFilePath, GenericNamespaced, ListenerOptions, Name, ToFsName, ToNsName, prelude::*,
};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// Launch arguments replace the playlist; drops append.
    pub replace: bool,
    pub paths: Vec<PathBuf>,
}

pub enum Acquire {
    /// A running player took the request; this process should exit.
    Handled,
    /// This process is the player. Requests from later launches arrive on the receiver.
    Listening(Receiver<Request>),
}

fn socket_name(suffix: &str) -> std::io::Result<Name<'static>> {
    let base = format!("syrinx-player{suffix}.sock");
    if GenericNamespaced::is_supported() {
        base.to_ns_name::<GenericNamespaced>()
    } else {
        let dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        dir.join(base).to_fs_name::<GenericFilePath>()
    }
}

/// Tries to hand `request` to a running player; failing that, becomes the player.
pub fn acquire(suffix: &str, request: Request) -> Acquire {
    let name = match socket_name(suffix) {
        Ok(n) => n,
        Err(_) => {
            // No usable socket name on this system: run standalone, no single-instance.
            let (_, rx) = mpsc::channel();
            return Acquire::Listening(rx);
        }
    };
    if let Ok(mut stream) = LocalSocketStream::connect(name.clone()) {
        let line = serde_json::to_string(&request).expect("a request serialises") + "\n";
        if stream.write_all(line.as_bytes()).is_ok() {
            let mut reply = String::new();
            let _ = BufReader::new(&mut stream).read_line(&mut reply);
            if reply.trim() == "ok" {
                return Acquire::Handled;
            }
        }
    }
    let (tx, rx) = mpsc::channel();
    let listener = match ListenerOptions::new().name(name).create_sync() {
        Ok(l) => l,
        Err(_) => return Acquire::Listening(rx),
    };
    std::thread::Builder::new()
        .name("syrinx-player-instance".into())
        .spawn(move || {
            for conn in listener.incoming().flatten() {
                let mut reader = BufReader::new(conn);
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() {
                    continue;
                }
                let Ok(request) = serde_json::from_str::<Request>(line.trim()) else {
                    continue;
                };
                let mut conn = reader.into_inner();
                let _ = conn.write_all(b"ok\n");
                if tx.send(request).is_err() {
                    return;
                }
            }
        })
        .expect("spawn the instance listener");
    Acquire::Listening(rx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn a_second_acquire_is_handled_by_the_first() {
        let suffix = format!(
            "-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        );
        let first = acquire(&suffix, Request { replace: true, paths: vec![] });
        let Acquire::Listening(rx) = first else { panic!("the first launch must listen") };
        let request = Request { replace: false, paths: vec![PathBuf::from("/x/a.syr"), PathBuf::from("/x/b.syr")] };
        match acquire(&suffix, request.clone()) {
            Acquire::Handled => {}
            Acquire::Listening(..) => panic!("the second launch must be handled by the first"),
        }
        let got = rx.recv_timeout(Duration::from_secs(2)).expect("the request arrives");
        assert_eq!(got, request);
    }
}

use std::env;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::protocol::{Request, Response};

const SPAWN_WAIT: Duration = Duration::from_secs(3);

/// One round trip per connection: the daemon reads a request line and answers
/// with a response line, so there is no session state to get out of sync.
pub struct Client {
    socket: PathBuf,
}

impl Client {
    /// Talks to a daemon that is already running, or `None` if there isn't one.
    pub fn connect(socket: &Path) -> Option<Self> {
        UnixStream::connect(socket).ok().map(|_| Self {
            socket: socket.to_path_buf(),
        })
    }

    /// Like `connect`, but starts a daemon first when nothing answers. Still
    /// `None` if the daemon never came up, so callers can fall back to `op`.
    pub fn connect_or_spawn(socket: &Path) -> Option<Self> {
        if let Some(client) = Self::connect(socket) {
            return Some(client);
        }
        spawn_daemon().ok()?;
        let deadline = Instant::now() + SPAWN_WAIT;
        while Instant::now() < deadline {
            if let Some(client) = Self::connect(socket) {
                return Some(client);
            }
            thread::sleep(Duration::from_millis(20));
        }
        None
    }

    pub fn call(&self, request: &Request) -> Result<Response> {
        let mut stream = UnixStream::connect(&self.socket)
            .with_context(|| format!("connecting to {}", self.socket.display()))?;
        serde_json::to_writer(&mut stream, request)?;
        stream.write_all(b"\n")?;
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line)?;
        serde_json::from_str(&line).context("reading the daemon's reply; if op-cache was just upgraded, run `op-cache stop` so a fresh daemon starts")
    }
}

fn spawn_daemon() -> Result<()> {
    Command::new(env::current_exe()?)
        .arg("daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .current_dir("/")
        .process_group(0)
        .spawn()
        .context("starting the op-cache daemon")?;
    Ok(())
}

use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use fs2::FileExt;

use crate::cache::Cache;
use crate::config::Config;
use crate::protocol::{Request, Response, Status};

struct State {
    cache: Cache,
    last_activity: Instant,
}

/// Serves the cache on the configured socket until told to stop, the idle
/// timeout passes, or another daemon already holds the socket's lock.
pub fn run(config: &Config) -> Result<()> {
    let socket = config.socket_path();
    let lock_path = socket.with_extension("lock");
    let lock =
        File::create(&lock_path).with_context(|| format!("creating {}", lock_path.display()))?;
    if lock.try_lock_exclusive().is_err() {
        return Ok(());
    }

    let _ = fs::remove_file(&socket);
    let listener =
        UnixListener::bind(&socket).with_context(|| format!("binding {}", socket.display()))?;
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;

    let started = Instant::now();
    let state = Arc::new(Mutex::new(State {
        cache: Cache::default(),
        last_activity: started,
    }));

    if let Some(idle) = config.idle_timeout {
        let state = Arc::clone(&state);
        let socket = socket.clone();
        thread::spawn(move || {
            loop {
                thread::sleep(Duration::from_secs(1).min(idle));
                if state.lock().unwrap().last_activity.elapsed() >= idle {
                    shutdown(&socket);
                }
            }
        });
    }

    let idle_timeout_secs = config.idle_timeout.map(|d| d.as_secs());
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let state = Arc::clone(&state);
        let socket = socket.clone();
        thread::spawn(move || {
            let _ = serve(stream, &state, &socket, idle_timeout_secs, started);
        });
    }
    Ok(())
}

fn serve(
    mut stream: UnixStream,
    state: &Mutex<State>,
    socket: &Path,
    idle_timeout_secs: Option<u64>,
    started: Instant,
) -> Result<()> {
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line)?;
    let request: Request = serde_json::from_str(&line)?;
    let now = Instant::now();
    let mut state = state.lock().unwrap();
    state.last_activity = now;

    let response = match request {
        Request::Get { key } => match state.cache.get(&key, now) {
            Some(value) => Response::Hit {
                value: value.to_vec(),
            },
            None => Response::Miss,
        },
        Request::Put {
            key,
            value,
            ttl_secs,
        } => {
            state
                .cache
                .put(key, value, ttl_secs.map(Duration::from_secs), now);
            Response::Done
        }
        Request::Clear => {
            state.cache.clear();
            Response::Done
        }
        Request::Status => Response::Status(Status {
            pid: std::process::id(),
            uptime_secs: started.elapsed().as_secs(),
            idle_timeout_secs,
            keys: state.cache.keys(now),
        }),
        Request::Stop => {
            reply(&mut stream, &Response::Done)?;
            shutdown(socket);
        }
    };
    reply(&mut stream, &response)
}

fn reply(stream: &mut UnixStream, response: &Response) -> Result<()> {
    serde_json::to_writer(&mut *stream, response)?;
    stream.write_all(b"\n")?;
    Ok(())
}

fn shutdown(socket: &Path) -> ! {
    let _ = fs::remove_file(socket);
    std::process::exit(0)
}

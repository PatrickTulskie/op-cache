mod cache;
mod client;
mod config;
mod daemon;
mod op;
mod prompt;
mod protocol;
mod wizard;

use std::collections::HashMap;
use std::env;
use std::ffi::OsString;
use std::io::{self, Write};
use std::os::unix::process::CommandExt;
use std::process::{Command, exit};

use anyhow::Result;

use client::Client;
use config::Config;
use protocol::{Request, Response};

const OP_REF_PREFIX: &str = "op://";

const HELP: &str = concat!(
    env!("CARGO_PKG_DESCRIPTION"),
    "

Usage: op-cache [COMMAND]

Commands:
  read     Read a secret reference, from the cache when it's there
  run      Run a command with its op:// environment variables resolved
  config   Configure caching interactively
  status   Show whether the daemon is up and how it's configured
  inspect  List what's in memory, with a masked peek at each value and its expiry
  clear    Drop every cached secret
  stop     Stop the daemon, dropping every cached secret
  help     Print this message

Anything else is passed straight to `op`.

Options:
  -h, --help     Print help
  -V, --version  Print version
"
);

fn main() {
    let config = match Config::load() {
        Ok(config) => config,
        Err(e) => fail(e),
    };
    if let Err(e) = dispatch(&config) {
        fail(e);
    }
}

fn fail(e: anyhow::Error) -> ! {
    eprintln!("op-cache: {e:#}");
    exit(1)
}

fn dispatch(config: &Config) -> Result<()> {
    let args: Vec<OsString> = env::args_os().skip(1).collect();
    let words: Vec<&str> = args
        .iter()
        .map(|a| a.to_str().unwrap_or_default())
        .collect();
    match words.as_slice() {
        [] | ["-h" | "--help" | "help"] => {
            print!("{HELP}");
            Ok(())
        }
        ["-V" | "--version"] => {
            println!("op-cache {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        ["read", _, ..] => read(config, &args[1..]),
        ["run", _, ..] => run(config, &args[1..]),
        [command @ ("read" | "run")] => {
            eprintln!("op-cache: {command} needs arguments; see op-cache --help");
            exit(2)
        }
        ["config"] => wizard::run(config.clone()),
        ["status"] => status(config),
        ["inspect"] => inspect(config),
        ["clear"] => send(config, Request::Clear, "cleared", "nothing to clear"),
        ["stop"] => send(config, Request::Stop, "stopped", "not running"),
        ["daemon"] => daemon::run(config),
        _ => Err(op::exec(&config.op, &args)),
    }
}

fn read(config: &Config, args: &[OsString]) -> Result<()> {
    if args
        .iter()
        .any(|a| matches!(a.to_str(), Some("-o" | "--out-file")))
    {
        return Err(op::exec(&config.op, &with_subcommand("read", args)));
    }
    let client = Client::connect_or_spawn(&config.socket_path());
    let value = resolve(config, client.as_ref(), args)?;
    io::stdout().write_all(&value)?;
    Ok(())
}

fn run(config: &Config, args: &[OsString]) -> Result<()> {
    let Some(command) = command_after_dashes(args) else {
        return Err(op::exec(&config.op, &with_subcommand("run", args)));
    };
    let client = Client::connect_or_spawn(&config.socket_path());
    let mut resolved = HashMap::new();
    let vars =
        env::vars_os().filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)));
    for (name, reference) in op_refs(vars) {
        let mut value = resolve(config, client.as_ref(), &[OsString::from(&reference)])?;
        if value.last() == Some(&b'\n') {
            value.pop();
        }
        resolved.insert(name, String::from_utf8(value)?);
    }
    let err = Command::new(&command[0])
        .args(&command[1..])
        .envs(resolved)
        .exec();
    Err(anyhow::Error::from(err).context(format!("running {}", command[0].to_string_lossy())))
}

fn with_subcommand(name: &str, args: &[OsString]) -> Vec<OsString> {
    let mut full = vec![OsString::from(name)];
    full.extend_from_slice(args);
    full
}

/// `run -- cmd` and `run cmd` both mean cmd; any flag before the `--` is
/// something op-cache doesn't know, so the caller hands the whole thing to op.
fn command_after_dashes(args: &[OsString]) -> Option<&[OsString]> {
    match args.iter().position(|a| a == "--") {
        Some(0) => Some(&args[1..]),
        Some(_) => None,
        None if args
            .first()
            .is_some_and(|a| a.to_string_lossy().starts_with('-')) =>
        {
            None
        }
        None => Some(args),
    }
    .filter(|cmd| !cmd.is_empty())
}

fn op_refs(vars: impl Iterator<Item = (String, String)>) -> Vec<(String, String)> {
    let mut refs: Vec<_> = vars.filter(|(_, v)| v.starts_with(OP_REF_PREFIX)).collect();
    refs.sort();
    refs
}

/// The read-through path: answer from the daemon, otherwise ask op and tell
/// the daemon what it said. With no daemon it is just `op read`.
fn resolve(config: &Config, client: Option<&Client>, args: &[OsString]) -> Result<Vec<u8>> {
    let key = cache_key(args);
    if let Some(client) = client
        && let Ok(Response::Hit { value }) = client.call(&Request::Get { key: key.clone() })
    {
        return Ok(value);
    }
    let output = op::read(&config.op, args)?;
    if !output.status.success() {
        exit(op::exit_code(output.status));
    }
    if let Some(client) = client {
        let _ = client.call(&Request::Put {
            key,
            value: output.stdout.clone(),
            ttl_secs: config
                .ttl_for(&args.iter().map(|a| a.to_string_lossy()).collect::<Vec<_>>())
                .map(|d| d.as_secs()),
        });
    }
    Ok(output.stdout)
}

fn cache_key(args: &[OsString]) -> String {
    args.iter()
        .map(|a| a.to_string_lossy())
        .collect::<Vec<_>>()
        .join("\u{1f}")
}

fn status(config: &Config) -> Result<()> {
    let socket = config.socket_path();
    println!("config   {}", config::config_path().display());
    println!("socket   {}", socket.display());
    println!(
        "ttl      {}",
        config::format_lifetime(config.ttl, "until the daemon exits")
    );
    let Some(client) = Client::connect(&socket) else {
        println!("daemon   not running");
        return Ok(());
    };
    let Response::Status(status) = client.call(&Request::Status)? else {
        anyhow::bail!("unexpected reply from the daemon");
    };
    println!(
        "daemon   pid {}, up {}, idles out {}",
        status.pid,
        humantime::format_duration(std::time::Duration::from_secs(status.uptime_secs)),
        status
            .idle_timeout_secs
            .map(|s| format!(
                "after {}",
                humantime::format_duration(std::time::Duration::from_secs(s))
            ))
            .unwrap_or_else(|| "never".into()),
    );
    println!("cached   {}", status.cached);
    Ok(())
}

fn inspect(config: &Config) -> Result<()> {
    let Some(client) = Client::connect(&config.socket_path()) else {
        println!("op-cache: not running");
        return Ok(());
    };
    let Response::Entries { entries } = client.call(&Request::Inspect)? else {
        anyhow::bail!("unexpected reply from the daemon");
    };
    if entries.is_empty() {
        println!("op-cache: nothing cached");
        return Ok(());
    }
    let rows: Vec<(String, String, String)> = entries
        .into_iter()
        .map(|e| {
            let expires = e
                .expires_in_secs
                .map(|s| {
                    format!(
                        "in {}",
                        humantime::format_duration(std::time::Duration::from_secs(s))
                    )
                })
                .unwrap_or_else(|| "when the daemon exits".into());
            (e.key.replace('\u{1f}', " "), e.preview, expires)
        })
        .collect();
    let key_width = rows
        .iter()
        .map(|r| r.0.len())
        .max()
        .unwrap_or(0)
        .max("REFERENCE".len());
    let value_width = rows
        .iter()
        .map(|r| r.1.chars().count())
        .max()
        .unwrap_or(0)
        .max("VALUE".len());
    println!(
        "{:<key_width$}  {:<value_width$}  EXPIRES",
        "REFERENCE", "VALUE"
    );
    for (key, value, expires) in rows {
        println!("{key:<key_width$}  {value:<value_width$}  {expires}");
    }
    Ok(())
}

fn send(config: &Config, request: Request, done: &str, absent: &str) -> Result<()> {
    match Client::connect(&config.socket_path()) {
        Some(client) => {
            client.call(&request)?;
            println!("op-cache: {done}");
        }
        None => println!("op-cache: {absent}"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn run_takes_the_command_after_the_dashes() {
        assert_eq!(
            command_after_dashes(&os(&["--", "things", "-x"])),
            Some(&os(&["things", "-x"])[..])
        );
        assert_eq!(
            command_after_dashes(&os(&["things", "-x"])),
            Some(&os(&["things", "-x"])[..])
        );
        assert_eq!(
            command_after_dashes(&os(&["--env-file", ".env", "--", "things"])),
            None
        );
        assert_eq!(command_after_dashes(&os(&["--no-masking", "things"])), None);
        assert_eq!(command_after_dashes(&os(&["--"])), None);
    }

    #[test]
    fn only_op_references_are_resolved() {
        let vars = vec![
            ("PATH".to_string(), "/bin".to_string()),
            ("TOKEN".to_string(), "op://vault/item/field".to_string()),
            ("OTHER".to_string(), "op://vault/other/field".to_string()),
            ("NOT".to_string(), "not op://".to_string()),
        ];
        assert_eq!(
            op_refs(vars.into_iter()),
            vec![
                ("OTHER".to_string(), "op://vault/other/field".to_string()),
                ("TOKEN".to_string(), "op://vault/item/field".to_string()),
            ]
        );
    }
}

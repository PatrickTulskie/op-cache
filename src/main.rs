mod cache;
mod client;
mod config;
mod daemon;
mod op;
mod protocol;
mod wizard;

use std::collections::HashMap;
use std::env;
use std::ffi::OsString;
use std::io::{self, Write};
use std::os::unix::process::CommandExt;
use std::process::{Command, exit};

use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand};

use client::Client;
use config::Config;
use protocol::{Request, Response};

const OP_REF_PREFIX: &str = "op://";

/// A read-through, in-memory cache in front of the 1Password CLI.
///
/// Anything op-cache doesn't handle itself is passed straight to `op`.
#[derive(Parser)]
#[command(version, about, long_about = None, allow_external_subcommands = true)]
struct Cli {
    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Read a secret reference, from the cache when it's there
    #[command(disable_help_flag = true)]
    Read {
        /// Passed to `op read` verbatim on a miss; the whole list is the cache key
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<OsString>,
    },
    /// Run a command with its op:// environment variables resolved
    #[command(disable_help_flag = true)]
    Run {
        /// The command; anything before `--` is handed to `op run` instead
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<OsString>,
    },
    /// Configure caching interactively
    Config,
    /// Show the daemon and what it's holding
    Status,
    /// Drop every cached secret
    Clear,
    /// Stop the daemon, dropping every cached secret
    Stop,
    #[command(hide = true)]
    Daemon,
    #[command(external_subcommand)]
    Op(Vec<OsString>),
}

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
    let raw: Vec<OsString> = env::args_os().skip(1).collect();
    if raw.first().is_some_and(is_op_flag) {
        return Err(op::exec(&config.op, &raw));
    }
    match Cli::parse().command {
        None => Ok(Cli::command().print_help()?),
        Some(Cmd::Read { args }) => read(config, &args),
        Some(Cmd::Run { args }) => run(config, &args),
        Some(Cmd::Config) => wizard::run(config.clone()),
        Some(Cmd::Status) => status(config),
        Some(Cmd::Clear) => send(config, Request::Clear, "cleared", "nothing to clear"),
        Some(Cmd::Stop) => send(config, Request::Stop, "stopped", "not running"),
        Some(Cmd::Daemon) => daemon::run(config),
        Some(Cmd::Op(args)) => Err(op::exec(&config.op, &args)),
    }
}

fn is_op_flag(arg: &OsString) -> bool {
    let s = arg.to_string_lossy();
    s.starts_with('-') && !matches!(s.as_ref(), "-h" | "--help" | "-V" | "--version")
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
    for (name, reference) in op_refs(env::vars()) {
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
            ttl_secs: config.ttl.map(|d| d.as_secs()),
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
    println!("cached   {}", status.keys.len());
    for key in status.keys {
        println!("  {}", key.replace('\u{1f}', " "));
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

    #[test]
    fn leading_flags_go_to_op_except_help_and_version() {
        assert!(is_op_flag(&OsString::from("--account")));
        assert!(!is_op_flag(&OsString::from("--help")));
        assert!(!is_op_flag(&OsString::from("read")));
    }
}

use std::io::{IsTerminal, stderr, stdin};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use cliclack::{confirm, input, intro, log, note, outro, outro_cancel, select};
use console::style;

use crate::client::Client;
use crate::config::{Config, config_path, default_socket_path, format_lifetime, parse_lifetime};

pub fn run(current: Config) -> Result<()> {
    if !(stdin().is_terminal() && stderr().is_terminal()) {
        print_current(&current);
        return Ok(());
    }

    intro(style(" op-cache ").on_cyan().black())?;
    log::remark(
        "Secrets are held in memory by a background daemon.\nThese settings decide how long they stay there.",
    )?;

    let ttl = ask_lifetime(
        "How long should a cached secret live?",
        current.ttl,
        (
            "Until the daemon exits",
            "the default; nothing ever goes stale on its own",
        ),
        (
            "For a fixed duration",
            "re-read from 1Password once it lapses",
        ),
        "1h",
    )?;
    let idle_timeout = ask_lifetime(
        "Should the daemon shut down after sitting idle?",
        current.idle_timeout,
        (
            "No, keep it running",
            "until op-cache stop, or the machine reboots",
        ),
        (
            "Yes, after a quiet period",
            "drops every secret from memory when it exits",
        ),
        "8h",
    )?;
    let op: String = input("Which op binary should it call?")
        .default_input(&current.op)
        .interact()?;
    let socket = ask_socket(current.socket.clone())?;

    let next = Config {
        ttl,
        idle_timeout,
        op,
        socket,
    };
    note(
        "One last look",
        format!(
            "{:<14}{}\n{:<14}{}\n{:<14}{}\n{:<14}{}",
            style("Secrets live").dim(),
            format_lifetime(next.ttl, "until the daemon exits"),
            style("Daemon idles").dim(),
            format_lifetime(next.idle_timeout, "forever"),
            style("op binary").dim(),
            next.op,
            style("Socket").dim(),
            next.socket_path().display(),
        ),
    )?;

    let path = config_path();
    if !confirm(format!("Write {}?", display_path(&path)))
        .initial_value(true)
        .interact()?
    {
        outro_cancel("Nothing was written.")?;
        return Ok(());
    }
    next.save()?;

    let daemon_affected =
        next.idle_timeout != current.idle_timeout || next.socket != current.socket;
    if daemon_affected && Client::connect(&current.socket_path()).is_some() {
        log::warning(
            "A daemon is already running with the old settings. Run `op-cache stop` to restart it.",
        )?;
    }
    outro(format!("Saved to {}", display_path(&path)))?;
    Ok(())
}

fn ask_lifetime(
    question: &str,
    current: Option<Duration>,
    never: (&str, &str),
    fixed: (&str, &str),
    example: &str,
) -> Result<Option<Duration>> {
    let choice = select(question)
        .item(false, never.0, never.1)
        .item(true, fixed.0, fixed.1)
        .initial_value(current.is_some())
        .interact()?;
    if !choice {
        return Ok(None);
    }
    let default = current.map(|d| humantime::format_duration(d).to_string());
    let answer: String = input("For how long?")
        .placeholder(&format!("e.g. {example}, 30m, 2d"))
        .default_input(default.as_deref().unwrap_or(example))
        .validate(|s: &String| match parse_lifetime(s) {
            Ok(Some(_)) => Ok(()),
            _ => Err("use a duration like 30m, 2h or 1d"),
        })
        .interact()?;
    Ok(parse_lifetime(&answer).unwrap_or(None))
}

fn ask_socket(current: Option<PathBuf>) -> Result<Option<PathBuf>> {
    let default = default_socket_path();
    let custom = select("Where should the daemon's socket live?")
        .item(false, "The default location", display_path(&default))
        .item(true, "A path of my choosing", "")
        .initial_value(current.is_some())
        .interact()?;
    if !custom {
        return Ok(None);
    }
    let answer: String = input("Socket path?")
        .default_input(&current.unwrap_or(default).display().to_string())
        .validate(|s: &String| {
            if s.starts_with('/') {
                Ok(())
            } else {
                Err("give an absolute path")
            }
        })
        .interact()?;
    Ok(Some(PathBuf::from(answer)))
}

fn print_current(config: &Config) {
    println!("# {}", config_path().display());
    println!("# `op-cache config` on a terminal walks through these interactively.");
    print!("{}", toml::to_string(config).unwrap_or_default());
}

fn display_path(path: &std::path::Path) -> String {
    let shown = path.display().to_string();
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() && shown.starts_with(&home) => {
            format!("~{}", &shown[home.len()..])
        }
        _ => shown,
    }
}

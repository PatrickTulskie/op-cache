use std::ffi::OsStr;
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitStatus, Output, Stdio};

use anyhow::{Context, Result};

/// Runs `op read <args>` with the terminal attached, so any sign-in prompt
/// reaches the user, and captures only stdout.
pub fn read<S: AsRef<OsStr>>(op: &str, args: &[S]) -> Result<Output> {
    Command::new(op)
        .arg("read")
        .args(args)
        .stdin(Stdio::inherit())
        .stderr(Stdio::inherit())
        .output()
        .with_context(|| format!("running {op}"))
}

/// Replaces this process with `op <args>`. Only returns if exec itself failed.
pub fn exec<S: AsRef<OsStr>>(op: &str, args: &[S]) -> anyhow::Error {
    let err = Command::new(op).args(args).exec();
    anyhow::Error::from(err).context(format!("running {op}"))
}

pub fn exit_code(status: ExitStatus) -> i32 {
    status.code().unwrap_or(1)
}

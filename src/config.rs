use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const CONFIG_ENV: &str = "OP_CACHE_CONFIG";
pub const SOCKET_ENV: &str = "OP_CACHE_SOCKET";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// How long a cached secret lives. `None` keeps it until the daemon exits.
    #[serde(with = "ttl_serde")]
    pub ttl: Option<Duration>,
    /// How long the daemon stays up with no requests. `None` means forever.
    #[serde(with = "idle_serde")]
    pub idle_timeout: Option<Duration>,
    pub op: String,
    pub socket: Option<PathBuf>,
    /// Per-reference lifetimes. A key ending in `/` covers everything under it.
    #[serde(with = "overrides_serde", skip_serializing_if = "BTreeMap::is_empty")]
    pub overrides: BTreeMap<String, Option<Duration>>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            ttl: None,
            idle_timeout: None,
            op: "op".into(),
            socket: None,
            overrides: BTreeMap::new(),
        }
    }
}

impl Config {
    pub fn load() -> Result<Self> {
        let path = config_path();
        match fs::read_to_string(&path) {
            Ok(text) => {
                toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = config_path();
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let text = toml::to_string(self)?;
        fs::write(&path, text).with_context(|| format!("writing {}", path.display()))
    }

    /// The lifetime for a `read` invocation: the most specific override that
    /// matches one of its arguments, otherwise the global `ttl`.
    pub fn ttl_for<S: AsRef<str>>(&self, args: &[S]) -> Option<Duration> {
        args.iter()
            .flat_map(|arg| {
                let arg = arg.as_ref();
                self.overrides.iter().filter(move |(pattern, _)| {
                    *pattern == arg || (pattern.ends_with('/') && arg.starts_with(pattern.as_str()))
                })
            })
            .max_by_key(|(pattern, _)| pattern.len())
            .map_or(self.ttl, |(_, ttl)| *ttl)
    }

    pub fn socket_path(&self) -> PathBuf {
        if let Some(p) = env::var_os(SOCKET_ENV) {
            return PathBuf::from(p);
        }
        self.socket.clone().unwrap_or_else(default_socket_path)
    }
}

pub fn config_path() -> PathBuf {
    if let Some(p) = env::var_os(CONFIG_ENV) {
        return PathBuf::from(p);
    }
    let base = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home().join(".config"));
    base.join("op-cache").join("config.toml")
}

pub fn default_socket_path() -> PathBuf {
    if let Some(dir) = env::var_os("XDG_RUNTIME_DIR") {
        return PathBuf::from(dir).join("op-cache.sock");
    }
    let uid = fs::metadata(home()).map(|m| m.uid()).unwrap_or(0);
    let tmp = env::var_os("TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    tmp.join(format!("op-cache-{uid}.sock"))
}

fn home() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

pub fn parse_lifetime(s: &str) -> Result<Option<Duration>, String> {
    match s.trim() {
        "" | "never" | "forever" | "until-exit" => Ok(None),
        other => humantime::parse_duration(other)
            .map(Some)
            .map_err(|e| e.to_string()),
    }
}

pub fn format_lifetime(d: Option<Duration>, none: &str) -> String {
    match d {
        Some(d) => humantime::format_duration(d).to_string(),
        None => none.to_string(),
    }
}

macro_rules! lifetime_serde {
    ($name:ident, $none:literal) => {
        mod $name {
            use super::*;
            use serde::{Deserializer, Serializer, de::Error};

            pub fn serialize<S: Serializer>(d: &Option<Duration>, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(&format_lifetime(*d, $none))
            }

            pub fn deserialize<'de, D: Deserializer<'de>>(
                d: D,
            ) -> Result<Option<Duration>, D::Error> {
                let s = String::deserialize(d)?;
                parse_lifetime(&s).map_err(D::Error::custom)
            }
        }
    };
}

lifetime_serde!(ttl_serde, "until-exit");
lifetime_serde!(idle_serde, "never");

mod overrides_serde {
    use super::*;
    use serde::{Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(
        map: &BTreeMap<String, Option<Duration>>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        let rendered: BTreeMap<&str, String> = map
            .iter()
            .map(|(k, d)| (k.as_str(), format_lifetime(*d, "until-exit")))
            .collect();
        rendered.serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<BTreeMap<String, Option<Duration>>, D::Error> {
        BTreeMap::<String, String>::deserialize(d)?
            .into_iter()
            .map(|(k, v)| {
                if !k.starts_with("op://") {
                    return Err(D::Error::custom(format!(
                        "override {k:?} should be an op:// reference"
                    )));
                }
                Ok((k, parse_lifetime(&v).map_err(D::Error::custom)?))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_round_trip_through_toml() {
        let text = toml::to_string(&Config::default()).unwrap();
        assert!(text.contains("ttl = \"until-exit\""));
        assert!(text.contains("idle_timeout = \"never\""));
        assert_eq!(toml::from_str::<Config>(&text).unwrap(), Config::default());
    }

    #[test]
    fn durations_parse_and_render() {
        let cfg: Config =
            toml::from_str("ttl = \"90m\"\nidle_timeout = \"2h\"\nop = \"/opt/op\"").unwrap();
        assert_eq!(cfg.ttl, Some(Duration::from_secs(5400)));
        assert_eq!(cfg.idle_timeout, Some(Duration::from_secs(7200)));
        assert_eq!(cfg.op, "/opt/op");
        assert!(toml::to_string(&cfg).unwrap().contains("ttl = \"1h 30m\""));
        assert!(toml::from_str::<Config>("ttl = \"soon\"").is_err());
        assert!(toml::from_str::<Config>("bogus = 1").is_err());
    }

    #[test]
    fn the_most_specific_override_wins() {
        let cfg: Config = toml::from_str(
            r#"
ttl = "1h"
[overrides]
"op://vault/" = "5m"
"op://vault/item/field" = "until-exit"
"#,
        )
        .unwrap();
        let hour = Some(Duration::from_secs(3600));
        assert_eq!(cfg.ttl_for(&["op://other/item/field"]), hour);
        assert_eq!(
            cfg.ttl_for(&["--account", "work", "op://vault/x/y"]),
            Some(Duration::from_secs(300))
        );
        assert_eq!(cfg.ttl_for(&["op://vault/item/field"]), None);
        assert_eq!(cfg.ttl_for(&["op://vault"]), hour);
        assert!(
            toml::to_string(&cfg)
                .unwrap()
                .contains("\"op://vault/item/field\" = \"until-exit\"")
        );
        assert!(
            !toml::to_string(&Config::default())
                .unwrap()
                .contains("overrides")
        );
        assert!(toml::from_str::<Config>("[overrides]\nnope = \"5m\"").is_err());
    }

    #[test]
    fn socket_env_overrides_config() {
        let cfg = Config {
            socket: Some("/from/config.sock".into()),
            ..Config::default()
        };
        assert_eq!(cfg.socket_path(), PathBuf::from("/from/config.sock"));
        unsafe { env::set_var(SOCKET_ENV, "/from/env.sock") };
        assert_eq!(cfg.socket_path(), PathBuf::from("/from/env.sock"));
        unsafe { env::remove_var(SOCKET_ENV) };
    }
}

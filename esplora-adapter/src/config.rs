use std::str::FromStr;

use electrum_client::bitcoin::Network;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    /// Electrum server, e.g. `tcp://127.0.0.1:50001` or `ssl://host:50002`.
    pub electrum_address: String,
    /// bitcoin | testnet | signet | regtest
    pub network: String,
    #[serde(default = "defaults::listen_address")]
    pub listen_address: String,
    #[serde(default = "defaults::port")]
    pub port: u16,
    #[serde(default = "defaults::enable_cors")]
    pub enable_cors: bool,
    /// Allowed origin. Empty means any origin.
    #[serde(default)]
    pub cors_domain: String,
    /// Electrum connections, used round-robin so one slow call does not block all.
    #[serde(default = "defaults::connections")]
    pub electrum_connections: usize,
    /// Per-IP requests per second. 0 turns the limit off.
    #[serde(default = "defaults::rate_per_sec")]
    pub rate_limit_per_sec: u32,
    #[serde(default = "defaults::rate_burst")]
    pub rate_limit_burst: u32,
    /// Read the client IP from `X-Forwarded-For`. Only enable behind a proxy you control.
    #[serde(default)]
    pub trust_forwarded_for: bool,
    /// Max requests being served at once. More get 503.
    #[serde(default = "defaults::max_concurrent")]
    pub max_concurrent: usize,
    #[serde(default = "defaults::timeout_secs")]
    pub request_timeout_secs: u64,
    /// Transactions kept in memory (they never change, so this saves lookups).
    #[serde(default = "defaults::tx_cache")]
    pub tx_cache_size: usize,
}

const ENV_PREFIX: &str = "ESPLORA_";
const FIELDS: [&str; 13] = [
    "electrum_address",
    "network",
    "listen_address",
    "port",
    "enable_cors",
    "cors_domain",
    "electrum_connections",
    "rate_limit_per_sec",
    "rate_limit_burst",
    "trust_forwarded_for",
    "max_concurrent",
    "request_timeout_secs",
    "tx_cache_size",
];

impl Config {
    /// Load the TOML file (optional), then let `ESPLORA_<FIELD>` env vars win.
    /// Example: `ESPLORA_RATE_LIMIT_PER_SEC=50`.
    pub fn load(path: &str) -> anyhow::Result<Self> {
        let mut table: toml::Table = match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => toml::Table::new(),
            Err(e) => return Err(anyhow::anyhow!("cannot read {path}: {e}")),
        };
        for field in FIELDS {
            let key = format!("{ENV_PREFIX}{}", field.to_uppercase());
            if let Ok(raw) = std::env::var(&key) {
                table.insert(field.to_string(), parse_value(&raw));
            }
        }
        table.try_into().map_err(|e| {
            anyhow::anyhow!(
                "bad config (electrum_address and network are required, in {path} or as \
                 ESPLORA_ELECTRUM_ADDRESS / ESPLORA_NETWORK): {e}"
            )
        })
    }

    pub fn net(&self) -> anyhow::Result<Network> {
        Network::from_str(&self.network)
            .map_err(|e| anyhow::anyhow!("bad network '{}': {e}", self.network))
    }
}

mod defaults {
    pub fn listen_address() -> String {
        "127.0.0.1".into()
    }
    pub fn port() -> u16 {
        3002
    }
    pub fn enable_cors() -> bool {
        true
    }
    pub fn connections() -> usize {
        4
    }
    pub fn rate_per_sec() -> u32 {
        20
    }
    pub fn rate_burst() -> u32 {
        40
    }
    pub fn max_concurrent() -> usize {
        64
    }
    pub fn timeout_secs() -> u64 {
        30
    }
    pub fn tx_cache() -> usize {
        20_000
    }
}

/// Env values are plain strings: read `true`/`false` and integers as such.
fn parse_value(raw: &str) -> toml::Value {
    let raw = raw.trim();
    if let Ok(b) = raw.parse::<bool>() {
        toml::Value::Boolean(b)
    } else if let Ok(i) = raw.parse::<i64>() {
        toml::Value::Integer(i)
    } else {
        toml::Value::String(raw.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_env_style_values() {
        assert_eq!(parse_value("true"), toml::Value::Boolean(true));
        assert_eq!(parse_value(" 50 "), toml::Value::Integer(50));
        assert_eq!(
            parse_value("tcp://h:1"),
            toml::Value::String("tcp://h:1".into())
        );
    }

    #[test]
    fn env_overrides_file_and_file_is_optional() {
        // single test touches env, so no race with other tests
        unsafe {
            std::env::set_var("ESPLORA_ELECTRUM_ADDRESS", "tcp://x:50001");
            std::env::set_var("ESPLORA_NETWORK", "regtest");
            std::env::set_var("ESPLORA_RATE_LIMIT_PER_SEC", "7");
            std::env::set_var("ESPLORA_ENABLE_CORS", "false");
        }
        let c = Config::load("/nonexistent/esplora.toml").unwrap();
        assert_eq!(c.electrum_address, "tcp://x:50001");
        assert_eq!(c.rate_limit_per_sec, 7);
        assert!(!c.enable_cors);
        assert_eq!(c.port, 3002); // default kept
    }
}

use rgb_lib::{AssetSchema, BitcoinNetwork, wallet::EvmChain};
use serde::{Deserialize, Serialize};
use serde_valid::toml::FromTomlStr;
use std::collections::HashMap;
use std::path::Path;

#[derive(Serialize, Deserialize, Default, Clone, Debug, serde_valid::Validate)]
pub struct Config {
    pub alias: Option<String>,
    pub data_dir: String,
    pub network: String,
    pub btc_rpc_address: String,
    pub btc_rpc_user: String,
    pub btc_rpc_password: String,
    pub indexer_address: String,
    pub proxy_address: Vec<String>,
    #[serde(default)]
    pub eth_rpc_url: Option<String>,
    #[serde(default)]
    pub eth_chain_id: Option<u64>,
    #[serde(default)]
    pub eth_min_mint_lock_blocks: Option<u64>,
    #[serde(default)]
    pub eth_max_mint_lock_blocks: Option<u64>,
    #[serde(default)]
    pub bfa_enabled: bool,
    /// Allow `/wallet/generatekeys`, `/wallet/send` and `/wallet/burn`. They
    /// handle a mnemonic on the server, so they are off by default.
    #[serde(default)]
    pub allow_server_signing: bool,
    /// Shared secret that callers of the signing endpoints send in the
    /// `x-signing-token` header. Required (16+ characters) when
    /// `allow_server_signing` is set.
    #[serde(default)]
    pub server_signing_token: Option<String>,
}

impl Config {
    /// Check the server-signing settings. Call once at startup.
    pub fn check_server_signing(&self) -> Result<(), String> {
        if !self.allow_server_signing {
            return Ok(());
        }
        match self.server_signing_token.as_deref() {
            Some(t) if t.len() >= 16 => Ok(()),
            _ => Err("allow_server_signing needs server_signing_token (16+ characters)".into()),
        }
    }

    /// True when `token` is the configured signing token.
    pub fn signing_token_matches(&self, token: &str) -> bool {
        let Some(expected) = self.server_signing_token.as_deref() else {
            return false;
        };
        // Constant time: compare every byte, whatever the first difference.
        let (a, b) = (expected.as_bytes(), token.as_bytes());
        let mut diff = a.len() ^ b.len();
        for (i, x) in a.iter().enumerate() {
            diff |= usize::from(x ^ b.get(i).copied().unwrap_or(0));
        }
        diff == 0
    }

    pub fn supported_schemas(&self) -> Result<Vec<AssetSchema>, rgb_lib::Error> {
        let mut schemas = vec![AssetSchema::Nia];
        if self.bfa_enabled {
            if self.net() == BitcoinNetwork::Mainnet {
                return Err(rgb_lib::Error::CannotUseSchemaOnMainnet {
                    asset_schema: AssetSchema::Bfa,
                });
            }
            self.evm_chains()?;
            schemas.push(AssetSchema::Bfa);
        }
        if self.net() != BitcoinNetwork::Mainnet {
            schemas.push(AssetSchema::Ifa);
        }
        Ok(schemas)
    }

    pub fn evm_chains(&self) -> Result<HashMap<u64, EvmChain>, rgb_lib::Error> {
        if !self.bfa_enabled {
            return Ok(HashMap::new());
        }
        let rpc_url = self
            .eth_rpc_url
            .as_ref()
            .filter(|url| !url.trim().is_empty())
            .ok_or_else(|| rgb_lib::Error::InvalidEvmRpcUrl {
                details: "BFA requires eth_rpc_url".into(),
            })?;
        let chain_id = self
            .eth_chain_id
            .ok_or_else(|| rgb_lib::Error::InvalidEvmChain {
                details: "BFA requires eth_chain_id".into(),
            })?;
        let (Some(min), Some(max)) = (self.eth_min_mint_lock_blocks, self.eth_max_mint_lock_blocks)
        else {
            return Err(rgb_lib::Error::InvalidEvmChain {
                details: "BFA requires eth_min_mint_lock_blocks and eth_max_mint_lock_blocks"
                    .into(),
            });
        };
        if min > max {
            return Err(rgb_lib::Error::InvalidEvmChain {
                details: "eth_max_mint_lock_blocks must be at least eth_min_mint_lock_blocks"
                    .into(),
            });
        }
        Ok(HashMap::from([(
            chain_id,
            EvmChain {
                rpc_url: rpc_url.clone(),
                min_mint_lock_blocks: min,
                max_mint_lock_blocks: max,
            },
        )]))
    }

    pub fn datadir(&self) -> String {
        let mut data_dir = Path::new(&self.data_dir).to_path_buf();

        if let Some(a) = self.alias.as_ref() {
            data_dir = data_dir.join(a);
        };

        data_dir.to_str().unwrap().to_owned()
    }

    pub fn net(&self) -> BitcoinNetwork {
        match self.network.to_ascii_lowercase().as_str() {
            "mainnet" => BitcoinNetwork::Mainnet,
            "regtest" => BitcoinNetwork::Regtest,
            "signet" => BitcoinNetwork::Signet,
            "testnet" => BitcoinNetwork::Testnet,
            "testnet4" => BitcoinNetwork::Testnet4,
            _ => BitcoinNetwork::Mainnet,
        }
    }

    pub fn read(path: &str) -> anyhow::Result<Config> {
        let contents = std::fs::read_to_string(path)?;
        let config: Config = Config::from_toml_str(&contents)?;
        config.supported_schemas()?;

        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bfa_is_explicit_and_requires_an_rpc() {
        assert_eq!(
            Config::default().supported_schemas().unwrap(),
            vec![AssetSchema::Nia]
        );
        assert!(Config::default().evm_chains().unwrap().is_empty());
        let mut cfg = Config {
            bfa_enabled: true,
            network: "mainnet".into(),
            eth_rpc_url: Some("http://127.0.0.1:31014".into()),
            eth_chain_id: Some(11155111),
            ..Default::default()
        };
        assert!(matches!(
            cfg.supported_schemas(),
            Err(rgb_lib::Error::CannotUseSchemaOnMainnet { .. })
        ));
        cfg.network = "regtest".into();
        cfg.eth_rpc_url = None;
        cfg.eth_chain_id = None;
        assert!(cfg.supported_schemas().is_err());
        cfg.eth_rpc_url = Some("http://127.0.0.1:31014".into());
        assert!(cfg.supported_schemas().is_err());
        cfg.eth_chain_id = Some(11155111);
        assert!(cfg.supported_schemas().is_err());
        cfg.eth_min_mint_lock_blocks = Some(2);
        assert!(cfg.supported_schemas().is_err());
        cfg.eth_max_mint_lock_blocks = Some(1000);
        assert_eq!(
            cfg.supported_schemas().unwrap(),
            vec![AssetSchema::Nia, AssetSchema::Bfa, AssetSchema::Ifa]
        );
        assert_eq!(
            cfg.evm_chains().unwrap(),
            HashMap::from([(
                11155111,
                EvmChain {
                    rpc_url: "http://127.0.0.1:31014".into(),
                    min_mint_lock_blocks: 2,
                    max_mint_lock_blocks: 1000,
                }
            )])
        );
        let unused = Config {
            eth_rpc_url: Some("http://127.0.0.1:31014".into()),
            eth_chain_id: Some(11155111),
            ..Default::default()
        };
        assert!(unused.evm_chains().unwrap().is_empty());
    }

    #[test]
    fn bfa_rejects_inconsistent_mint_lock_bounds() {
        let mut cfg = Config {
            bfa_enabled: true,
            network: "regtest".into(),
            eth_rpc_url: Some("http://127.0.0.1:8545".into()),
            eth_chain_id: Some(31337),
            ..Default::default()
        };
        for (min, max) in [(2, 1), (100, 99)] {
            cfg.eth_min_mint_lock_blocks = Some(min);
            cfg.eth_max_mint_lock_blocks = Some(max);
            assert!(matches!(
                cfg.supported_schemas(),
                Err(rgb_lib::Error::InvalidEvmChain { .. })
            ));
        }
        cfg.eth_min_mint_lock_blocks = Some(0);
        cfg.eth_max_mint_lock_blocks = Some(0);
        assert!(cfg.supported_schemas().is_ok());
    }

    #[test]
    fn ifa_is_enabled_off_mainnet_only() {
        let mut cfg = Config {
            network: "regtest".into(),
            ..Default::default()
        };
        assert_eq!(
            cfg.supported_schemas().unwrap(),
            vec![AssetSchema::Nia, AssetSchema::Ifa]
        );
        cfg.network = "mainnet".into();
        assert_eq!(cfg.supported_schemas().unwrap(), vec![AssetSchema::Nia]);
    }
}

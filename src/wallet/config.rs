use rgb_lib::{AssetSchema, BitcoinNetwork};
use serde::{Deserialize, Serialize};
use serde_valid::toml::FromTomlStr;
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
    /// EVM RPC used for BFA consignment validation. Optional for NIA wallets.
    #[serde(default)]
    pub eth_rpc_url: Option<String>,
    /// Opt in to BFA validation; existing NIA deployments remain unchanged.
    #[serde(default)]
    pub bfa_enabled: bool,
}

impl Config {
    pub fn supported_schemas(&self) -> Result<Vec<AssetSchema>, rgb_lib::Error> {
        let mut schemas = vec![AssetSchema::Nia];
        if self.bfa_enabled {
            if self
                .eth_rpc_url
                .as_deref()
                .is_none_or(|url| url.trim().is_empty())
            {
                return Err(rgb_lib::Error::InvalidEthRpcUrl {
                    details: "BFA requires an explicit EVM RPC".into(),
                });
            }
            schemas.push(AssetSchema::Bfa);
        }
        Ok(schemas)
    }

    /// The EVM RPC passed to `OnlineOptions`, only when BFA is enabled.
    pub fn eth_rpc(&self) -> Option<String> {
        if self.bfa_enabled {
            self.eth_rpc_url.clone()
        } else {
            None
        }
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
        assert!(Config::default().eth_rpc().is_none());
        let mut cfg = Config {
            bfa_enabled: true,
            ..Default::default()
        };
        assert!(cfg.supported_schemas().is_err());
        cfg.eth_rpc_url = Some("http://127.0.0.1:31014".into());
        assert_eq!(
            cfg.supported_schemas().unwrap(),
            vec![AssetSchema::Nia, AssetSchema::Bfa]
        );
        assert_eq!(cfg.eth_rpc().as_deref(), Some("http://127.0.0.1:31014"));
        // An RPC configured without the opt-in stays unused.
        let unused = Config {
            eth_rpc_url: Some("http://127.0.0.1:31014".into()),
            ..Default::default()
        };
        assert!(unused.eth_rpc().is_none());
    }
}

use rgb_lib::{AssetSchema, BitcoinNetwork};
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
    /// EVM RPC used for BFA mint validation. Optional for NIA wallets.
    #[serde(default)]
    pub eth_rpc_url: Option<String>,
    /// EIP-155 chain ID of `eth_rpc_url`; rgb-lib checks the RPC is on it.
    #[serde(default)]
    pub eth_chain_id: Option<u64>,
    /// Opt in to BFA validation; existing NIA deployments remain unchanged.
    #[serde(default)]
    pub bfa_enabled: bool,
}

impl Config {
    pub fn supported_schemas(&self) -> Result<Vec<AssetSchema>, rgb_lib::Error> {
        let mut schemas = vec![AssetSchema::Nia];
        if self.bfa_enabled {
            // rgb-lib rejects the BFA schema on mainnet.
            if self.net() == BitcoinNetwork::Mainnet {
                return Err(rgb_lib::Error::CannotUseSchemaOnMainnet {
                    asset_schema: AssetSchema::Bfa,
                });
            }
            if self
                .eth_rpc_url
                .as_deref()
                .is_none_or(|url| url.trim().is_empty())
            {
                return Err(rgb_lib::Error::InvalidEvmRpcUrl {
                    details: "BFA requires an explicit EVM RPC".into(),
                });
            }
            if self.eth_chain_id.is_none() {
                return Err(rgb_lib::Error::InvalidEvmRpcUrl {
                    details: "BFA requires the chain ID of the EVM RPC".into(),
                });
            }
            schemas.push(AssetSchema::Bfa);
        }
        // rgb-lib rejects the IFA schema on mainnet.
        if self.net() != BitcoinNetwork::Mainnet {
            schemas.push(AssetSchema::Ifa);
        }
        Ok(schemas)
    }

    /// The EVM RPCs passed to `OnlineOptions`, only when BFA is enabled.
    pub fn evm_rpc_urls(&self) -> HashMap<u64, String> {
        match (self.bfa_enabled, self.eth_chain_id, &self.eth_rpc_url) {
            (true, Some(chain_id), Some(url)) => HashMap::from([(chain_id, url.clone())]),
            _ => HashMap::new(),
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
        assert!(Config::default().evm_rpc_urls().is_empty());
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
        assert_eq!(
            cfg.supported_schemas().unwrap(),
            vec![AssetSchema::Nia, AssetSchema::Bfa, AssetSchema::Ifa]
        );
        assert_eq!(
            cfg.evm_rpc_urls(),
            HashMap::from([(11155111, "http://127.0.0.1:31014".to_string())])
        );
        // An RPC configured without the opt-in stays unused.
        let unused = Config {
            eth_rpc_url: Some("http://127.0.0.1:31014".into()),
            eth_chain_id: Some(11155111),
            ..Default::default()
        };
        assert!(unused.evm_rpc_urls().is_empty());
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

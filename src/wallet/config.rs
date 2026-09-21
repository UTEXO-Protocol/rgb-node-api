use rgb_lib::BitcoinNetwork;
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
}

impl Config {
    pub fn datadir(&self) -> String {
        let mut data_dir = Path::new(&self.data_dir).to_path_buf();

        if let Some(a) = self.alias.as_ref() {
            data_dir = data_dir.join(a);
        };

        data_dir.to_str().unwrap().to_owned()
    }

    pub fn net(&self) -> Result<BitcoinNetwork, rgb_lib::Error> {
        Ok(match self.network.to_ascii_lowercase().as_str() {
            "mainnet" => BitcoinNetwork::Mainnet,
            "regtest" => BitcoinNetwork::Regtest,
            "signet" => BitcoinNetwork::Signet,
            "testnet" => BitcoinNetwork::Testnet,
            "testnet4" => BitcoinNetwork::Testnet4,
            _ => {
                return Err(rgb_lib::Error::InvalidBitcoinNetwork {
                    network: self.network.clone(),
                });
            }
        })
    }

    pub fn read(path: &str) -> anyhow::Result<Config> {
        let contents = std::fs::read_to_string(path)?;
        let config: Config = Config::from_toml_str(&contents)?;
        config.net()?;

        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_or_missing_network_never_selects_mainnet() {
        for network in ["", "testent", "bitcoin", " regtest"] {
            let cfg = Config {
                network: network.into(),
                ..Default::default()
            };
            assert!(matches!(
                cfg.net(),
                Err(rgb_lib::Error::InvalidBitcoinNetwork { .. })
            ));
        }
        for (network, expected) in [
            ("mainnet", BitcoinNetwork::Mainnet),
            ("regtest", BitcoinNetwork::Regtest),
            ("signet", BitcoinNetwork::Signet),
            ("testnet", BitcoinNetwork::Testnet),
            ("testnet4", BitcoinNetwork::Testnet4),
        ] {
            let cfg = Config {
                network: network.into(),
                ..Default::default()
            };
            assert_eq!(cfg.net().unwrap(), expected);
        }
    }
}

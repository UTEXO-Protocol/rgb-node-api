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
}

impl Config {
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

        Ok(config)
    }
}

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
    #[serde(default)]
    pub mpc_send: MpcSendPolicy,
}

/// Immutable preparation policy; saved transactions retain their original policy.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct MpcSendPolicy {
    /// Retained only to validate previously saved split-change operations.
    pub carrier_sat: u64,
    pub max_amount: u64,
    pub fee_rate_sat_vb: u64,
    pub max_fee_sat: u64,
    pub max_inputs: usize,
    pub min_confirmations: u8,
    pub min_invoice_validity_secs: u64,
}

impl Default for MpcSendPolicy {
    fn default() -> Self {
        // Conservative limits for the two-role P2TR test profile.
        Self {
            carrier_sat: 1000,
            max_amount: 25,
            fee_rate_sat_vb: 2,
            max_fee_sat: 2000,
            max_inputs: 10,
            min_confirmations: 1,
            min_invoice_validity_secs: 120,
        }
    }
}

impl MpcSendPolicy {
    pub(crate) fn validate(&self) -> Result<(), &'static str> {
        // The library's bounded size estimator requires fewer than 253 inputs.
        if !(330..=100_000).contains(&self.carrier_sat)
            || self.max_amount == 0
            || self.fee_rate_sat_vb == 0
            || self.max_fee_sat == 0
            || !(1..253).contains(&self.max_inputs)
            || self.min_confirmations == 0
            || self.min_invoice_validity_secs == 0
        {
            return Err("Invalid MPC send policy");
        }
        Ok(())
    }
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

    pub fn net(&self) -> Result<BitcoinNetwork, rgb_lib::Error> {
        Self::parse_network(&self.network)
    }

    pub(crate) fn parse_network(network: &str) -> Result<BitcoinNetwork, rgb_lib::Error> {
        Ok(match network.to_ascii_lowercase().as_str() {
            "mainnet" => BitcoinNetwork::Mainnet,
            "regtest" => BitcoinNetwork::Regtest,
            "signet" => BitcoinNetwork::Signet,
            "testnet" | "testnet3" => BitcoinNetwork::Testnet,
            "testnet4" => BitcoinNetwork::Testnet4,
            _ => {
                return Err(rgb_lib::Error::InvalidBitcoinNetwork {
                    network: network.to_owned(),
                });
            }
        })
    }

    pub fn read(path: &str) -> anyhow::Result<Config> {
        let contents = std::fs::read_to_string(path)?;
        let config: Config = Config::from_toml_str(&contents)?;
        config.net()?;
        config.supported_schemas()?;

        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn send_policy_is_configurable_and_legacy_defaults_are_preserved() {
        let policy: MpcSendPolicy = serde_json::from_str("{}").unwrap();
        assert_eq!(policy, MpcSendPolicy::default());
        let mut policy: MpcSendPolicy =
            serde_json::from_str(r#"{"max_amount":1000,"max_fee_sat":3000,"fee_rate_sat_vb":3}"#)
                .unwrap();
        assert!(policy.validate().is_ok());
        assert_eq!(policy.max_amount, 1000);
        policy.max_inputs = 253;
        assert!(policy.validate().is_err());
        assert!(serde_json::from_str::<MpcSendPolicy>(r#"{"max_amout":1000}"#).is_err());
    }

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
            ("testnet3", BitcoinNetwork::Testnet),
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

#[cfg(test)]
mod schema_wire_tests {
    use rgb_lib::AssetSchema;

    #[test]
    fn schema_serialises_as_its_variant_name() {
        assert_eq!(serde_json::to_string(&AssetSchema::Nia).unwrap(), "\"Nia\"");
        assert_eq!(serde_json::to_string(&AssetSchema::Bfa).unwrap(), "\"Bfa\"");
    }
}

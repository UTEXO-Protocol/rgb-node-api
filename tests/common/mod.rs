#![allow(dead_code)]
use rgb_lib::{
    BitcoinNetwork,
    bitcoin::{
        Address, CompressedPublicKey, Network,
        blockdata::constants::genesis_block,
        secp256k1::{Secp256k1, SecretKey},
    },
};
use rgb_node_api::{
    mpc::{Owner, Provider, RegisteredAddress, Registration, Role, ScriptType, WitnessRequest},
    wallet::Config,
};
use uuid::Uuid;

pub const TOKEN: &str = "local-test-service-token-not-for-production";
pub fn owner() -> Owner {
    Owner {
        tenant_id: "poc".into(),
        user_id: "alice".into(),
    }
}
pub fn config(directory: &std::path::Path) -> Config {
    Config {
        data_dir: directory.to_string_lossy().into(),
        network: "regtest".into(),
        indexer_address: "127.0.0.1:51011".into(),
        proxy_address: vec!["rpc://127.0.0.1:31010/json-rpc".into()],
        ..Default::default()
    }
}
pub fn registration(seed: u8) -> Registration {
    registration_for_network(seed, BitcoinNetwork::Regtest)
}
pub fn registration_for_network(seed: u8, network: BitcoinNetwork) -> Registration {
    let addresses = [Role::Rgb, Role::Fee]
        .into_iter()
        .enumerate()
        .map(|(i, role)| {
            // Public deterministic keys used only by tests; no live provider signer.
            let secret = SecretKey::from_slice(&[seed + i as u8; 32]).unwrap();
            let public = CompressedPublicKey(secret.public_key(&Secp256k1::new()));
            RegisteredAddress {
                role,
                script_type: ScriptType::P2wpkh,
                address: Address::p2wpkh(&public, Network::from(network)).to_string(),
                public_key: public.to_string(),
                signing_key_id: format!("fixture-{seed}-{i}"),
            }
        })
        .collect();
    Registration {
        wallet_id: Uuid::new_v4(),
        provider: Provider::FireblocksVault,
        provider_environment: "fixture-environment".into(),
        provider_wallet_ref: format!("fixture-wallet-{seed}"),
        bitcoin_network: network.to_string().to_ascii_lowercase(),
        genesis_hash: genesis_block(Network::from(network))
            .block_hash()
            .to_string(),
        addresses,
    }
}
pub fn witness() -> WitnessRequest {
    WitnessRequest {
        request_id: Uuid::new_v4(),
        asset_id: None,
        amount: Some("25".into()),
        expiration_timestamp: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 3600,
    }
}

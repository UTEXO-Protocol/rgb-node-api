#![allow(dead_code)]
use rgb_lib::{
    BitcoinNetwork,
    bitcoin::{
        Address, Network,
        blockdata::constants::genesis_block,
        secp256k1::{Secp256k1, SecretKey},
    },
};
use rgb_node_api::{
    mpc::{BlindRequest, Owner, Provider, RegisteredAddress, Registration, Role, ScriptType},
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
        indexer_address: "127.0.0.1:51111".into(),
        proxy_address: vec!["rpc://127.0.0.1:31110/json-rpc".into()],
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
            let public = secret.x_only_public_key(&Secp256k1::new()).0;
            let (output, _) =
                rgb_lib::bitcoin::key::TapTweak::tap_tweak(public, &Secp256k1::new(), None);
            RegisteredAddress {
                role,
                script_type: ScriptType::P2tr,
                address: Address::p2tr(&Secp256k1::new(), public, None, Network::from(network))
                    .to_string(),
                public_key: output.to_string(),
                internal_key: public.to_string(),
                provider_wallet_id: format!("wallet-{seed}-{i}"),
                signing_key_id: format!("fixture-{seed}-{i}"),
            }
        })
        .collect();
    Registration {
        wallet_id: Uuid::new_v4(),
        provider: Provider::try_from("dfns".to_owned()).unwrap(),
        provider_environment: "fixture-environment".into(),
        provider_wallet_ref: format!("fixture-wallet-{seed}"),
        bitcoin_network: network.to_string().to_ascii_lowercase(),
        genesis_hash: genesis_block(Network::from(network))
            .block_hash()
            .to_string(),
        addresses,
    }
}
pub fn blind() -> BlindRequest {
    BlindRequest {
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

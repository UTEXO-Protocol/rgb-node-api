//! Prints unfunded public registration fixtures for the selected Bitcoin network.
//! These are not real Dynamic or Fireblocks wallets.
#[path = "../tests/common/mod.rs"]
mod common;
use rgb_lib::bitcoin::{
    Address, Network,
    key::TweakedPublicKey,
    secp256k1::{Secp256k1, SecretKey},
};
use rgb_node_api::mpc::{Provider, ScriptType};

fn main() -> anyhow::Result<()> {
    let model = std::env::args().nth(1).unwrap_or_else(|| "vault".into());
    anyhow::ensure!(
        model == "vault" || model == "embedded",
        "Use vault or embedded"
    );
    let seed = if model == "vault" { 42 } else { 52 };
    let network = rgb_node_api::wallet::Config {
        network: std::env::args().nth(2).unwrap_or_else(|| "regtest".into()),
        ..Default::default()
    }
    .net()?;
    let mut registration = common::registration_for_network(seed, network);
    registration.wallet_id = uuid::Uuid::from_u128(if model == "vault" { 1 } else { 2 });
    if model == "embedded" {
        registration.provider = Provider::DynamicEmbedded;
        for (index, address) in registration.addresses.iter_mut().enumerate() {
            let secret = SecretKey::from_slice(&[seed + index as u8; 32])?;
            let (output_key, _) = secret.public_key(&Secp256k1::new()).x_only_public_key();
            address.script_type = ScriptType::P2tr;
            address.public_key = output_key.to_string();
            address.address = Address::p2tr_tweaked(
                TweakedPublicKey::dangerous_assume_tweaked(output_key),
                Network::from(network),
            )
            .to_string();
        }
    }
    println!("{}", serde_json::to_string_pretty(&registration)?);
    Ok(())
}

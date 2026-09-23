//! Operator-only BTC preparation/submission using an existing watch-only source wallet.
//! Caller must hold and inherit operator.lock. JSON stdin/stdout; never accepts keys.
use std::{io::Read, str::FromStr};

use anyhow::{Context, Result, ensure};
use rgb_lib::{
    BitcoinNetwork, Wallet,
    bitcoin::{Address, Network},
    wallet::{OnlineOptions, RgbWalletOpsOffline},
};
use serde_json::{Value, json};

fn string<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key].as_str().with_context(|| format!("Missing {key}"))
}

fn execute(v: &Value) -> Result<Value> {
    let command = string(v, "command")?;
    ensure!(
        ["prepare", "submit", "status"].contains(&command),
        "Unknown command"
    );
    let address =
        Address::from_str(string(v, "recipient_address")?)?.require_network(Network::Testnet)?;
    let headers = &v["headers"];
    // load refuses missing manifests; do not create or replace source wallet state.
    let mut wallet = Wallet::load(
        string(v, "data_dir")?,
        string(headers, "master-fingerprint")?,
        None,
    )?;
    ensure!(
        wallet.get_wallet_data().bitcoin_network == BitcoinNetwork::Testnet,
        "Testnet3 only"
    );
    let keys = wallet.get_keys();
    ensure!(
        keys.account_xpub_colored == string(headers, "xpub-col")?
            && keys.account_xpub_vanilla == string(headers, "xpub-van")?,
        "Source binding changed"
    );
    let online = wallet.go_online(OnlineOptions {
        indexer_url: "ssl://electrum.iriswallet.com:50013".into(),
        skip_consistency_check: false,
        vanilla_sync_lookback: 20,
        eth_rpc_url: if v["bfa_mock"] == true {
            Some("http://127.0.0.1:31014".into())
        } else {
            None
        },
    })?;
    Ok(match command {
        "prepare" => {
            let unspents = wallet.list_unspents(Some(online), false, false)?;
            let psbt =
                wallet.send_btc_begin(online, address.to_string(), 5000, 2, false, false, None)?;
            json!({"psbt": psbt, "unspents": unspents})
        }
        "submit" => json!({"txid": wallet.send_btc_end(online, string(v, "signed_psbt")?.into())?}),
        "status" => json!({"transactions": wallet.list_transactions(Some(online), false)?}),
        _ => unreachable!(),
    })
}

fn main() -> Result<()> {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    println!("{}", execute(&serde_json::from_str(&input)?)?);
    Ok(())
}

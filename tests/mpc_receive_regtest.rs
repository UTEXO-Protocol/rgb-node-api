//! Actual regtest receive using a local watch-only sender and a separate test
//! signer. No provider API or EVM bridge is mocked into the RGB validation path.
mod common;
use common::*;
use rgb_lib::{
    AssetSchema, Assignment, BitcoinNetwork,
    bitcoin::{
        Address, Network,
        key::TweakedPublicKey,
        secp256k1::{Secp256k1, SecretKey},
    },
    keys::{WitnessVersion, generate_keys},
    wallet::{
        DatabaseType, Invoice, OnlineOptions, Recipient, RgbWalletOpsOffline, RgbWalletOpsOnline,
        SinglesigKeys, Wallet, WalletData, WitnessData,
    },
};
use rgb_node_api::mpc::{MpcService, Provider, ScriptType};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

struct Bitcoin {
    client: reqwest::blocking::Client,
    wallet: String,
    mining_address: String,
}
impl Bitcoin {
    fn rpc(&self, method: &str, params: Value, wallet: bool) -> Value {
        let url = if wallet {
            format!("http://127.0.0.1:19443/wallet/{}", self.wallet)
        } else {
            "http://127.0.0.1:19443".into()
        };
        let response: Value = self
            .client
            .post(url)
            .basic_auth("user", Some("default_password"))
            .json(&json!({"jsonrpc":"2.0", "id":1, "method":method, "params":params}))
            .send()
            .unwrap()
            .json()
            .unwrap();
        assert!(
            response["error"].is_null(),
            "{method}: {}",
            response["error"]
        );
        response["result"].clone()
    }
    fn mine(&self, blocks: u64) {
        self.rpc(
            "generatetoaddress",
            json!([blocks, self.mining_address]),
            true,
        );
    }
    fn new() -> Self {
        let mut value = Self {
            client: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .unwrap(),
            wallet: format!("mpc-poc-{}", uuid::Uuid::new_v4()),
            mining_address: String::new(),
        };
        assert_eq!(
            value.rpc("getblockchaininfo", json!([]), false)["chain"],
            "regtest"
        );
        assert_eq!(
            value.rpc("getblockhash", json!([0]), false),
            registration(1).genesis_hash
        );
        value.rpc("createwallet", json!([value.wallet]), false);
        value.mining_address = value
            .rpc("getnewaddress", json!([]), true)
            .as_str()
            .unwrap()
            .into();
        value.mine(101);
        value
    }
}

fn wallet_data(path: &std::path::Path) -> WalletData {
    WalletData {
        data_dir: path.to_string_lossy().into(),
        bitcoin_network: BitcoinNetwork::Regtest,
        database_type: DatabaseType::Sqlite,
        max_allocations_per_utxo: 5,
        supported_schemas: vec![AssetSchema::Nia],
        reuse_addresses: false,
    }
}

#[test]
#[ignore = "Starts real regtest transfers; requires tests/mpc/compose.yaml and RGB_MPC_REGTEST=1"]
fn nia_receive_p2wpkh_and_p2tr_survives_restart() {
    assert_eq!(std::env::var("RGB_MPC_REGTEST").as_deref(), Ok("1"));
    let bitcoin = Bitcoin::new();
    let source_dir = tempfile::tempdir().unwrap();
    let signer_dir = tempfile::tempdir().unwrap();
    let receiver_dir = tempfile::tempdir().unwrap();
    let keys = generate_keys(BitcoinNetwork::Regtest, WitnessVersion::Taproot);
    let mut watch_keys = SinglesigKeys::from_keys(&keys, None);
    watch_keys.mnemonic = None;
    let mut source = Wallet::new(wallet_data(source_dir.path()), watch_keys).unwrap();
    let signer = Wallet::new(
        wallet_data(signer_dir.path()),
        SinglesigKeys::from_keys(&keys, None),
    )
    .unwrap();
    let options = OnlineOptions {
        indexer_url: "127.0.0.1:51011".into(),
        skip_consistency_check: false,
        vanilla_sync_lookback: 20,
        eth_rpc_url: None,
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    let online = loop {
        match source.go_online(options.clone()) {
            Ok(online) => break online,
            Err(error) if Instant::now() < deadline => {
                eprintln!("Waiting for local indexer: {error}");
                std::thread::sleep(Duration::from_millis(500));
            }
            Err(error) => panic!("Indexer unavailable: {error}"),
        }
    };
    let funding_address = source.get_address().unwrap();
    bitcoin.rpc("sendtoaddress", json!([funding_address, 1.0]), true);
    bitcoin.mine(1);
    let deadline = Instant::now() + Duration::from_secs(30);
    while source
        .get_btc_balance(Some(online), false)
        .unwrap()
        .vanilla
        .spendable
        < 100_000_000
    {
        assert!(Instant::now() < deadline, "Funded source not indexed");
        std::thread::sleep(Duration::from_millis(300));
    }
    let psbt = source
        .create_utxos_begin(online, false, Some(5), Some(5000), 2, false, true)
        .unwrap();
    let signed = signer.sign_psbt(psbt, None).unwrap();
    source.create_utxos_end(online, signed).unwrap();
    bitcoin.mine(1);
    let asset = source
        .issue_asset_nia("POC".into(), "POC Test Asset".into(), 0, vec![1000])
        .unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let cfg = config(receiver_dir.path());
    let service = MpcService::new(cfg.clone(), Some(TOKEN.into())).unwrap();
    let mut recipients = vec![];
    let mut evidence = vec![];
    for (seed, taproot) in [(42, false), (52, true)] {
        let mut registration = registration(seed);
        if taproot {
            registration.provider = Provider::DynamicEmbedded;
            for (index, address) in registration.addresses.iter_mut().enumerate() {
                let secret = SecretKey::from_slice(&[seed + index as u8; 32]).unwrap();
                let (output_key, _) = secret.public_key(&Secp256k1::new()).x_only_public_key();
                address.script_type = ScriptType::P2tr;
                address.public_key = output_key.to_string();
                address.address = Address::p2tr_tweaked(
                    TweakedPublicKey::dangerous_assume_tweaked(output_key),
                    Network::Regtest,
                )
                .to_string();
            }
            // Embedded SDKs may initially expose only one Bitcoin address.
            registration
                .addresses
                .retain(|address| address.role == rgb_node_api::mpc::Role::Rgb);
        }
        let id = registration.wallet_id;
        runtime
            .block_on(service.register(owner(), registration))
            .unwrap();
        // Receive twice at the pinned RGB address. Each invoice carries its own nonce.
        for iteration in 1..=2 {
            let mut request = witness();
            if iteration == 2 {
                request.asset_id = Some(asset.asset_id.clone());
            }
            let invoice = runtime
                .block_on(service.witness(owner(), id, request))
                .unwrap();
            let data = Invoice::new(invoice.invoice.clone())
                .unwrap()
                .invoice_data();
            let recipient = Recipient {
                recipient_id: data.recipient_id,
                assignment: Assignment::Fungible(25),
                witness_data: Some(WitnessData {
                    amount_sat: 1000,
                    blinding: None,
                }),
                transport_endpoints: data.transport_endpoints,
            };
            let begin = source
                .send_begin(
                    online,
                    HashMap::from([(asset.asset_id.clone(), vec![recipient])]),
                    false,
                    2,
                    1,
                    invoice.expiration_timestamp,
                    false,
                    None,
                )
                .unwrap();
            let signed = signer.sign_psbt(begin.psbt, None).unwrap();
            let operation = source.send_end(online, signed).unwrap();
            let deadline = Instant::now() + Duration::from_secs(40);
            loop {
                assert!(runtime.block_on(service.refresh_registered()).unwrap() > 0);
                source.refresh(online, None, vec![], false).unwrap();
                bitcoin.mine(1);
                let assets = runtime.block_on(service.assets(owner(), id)).unwrap();
                let balance = assets
                    .nia
                    .unwrap_or_default()
                    .iter()
                    .find(|item| item.asset_id == asset.asset_id)
                    .map(|item| item.balance.settled)
                    .unwrap_or(0);
                if balance == 25 * iteration {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "NIA did not settle; balance={balance}"
                );
                std::thread::sleep(Duration::from_millis(300));
            }
            assert!(bitcoin.rpc("getrawtransaction", json!([operation.txid, true]), false)["confirmations"].as_u64().unwrap() >= 1);
            evidence.push(json!({"wallet_id":id, "script":if taproot {"p2tr"} else {"p2wpkh"}, "txid":operation.txid, "received":25}));
        }
        recipients.push(id);
    }
    drop(service);
    let reopened = MpcService::new(cfg, Some(TOKEN.into())).unwrap();
    assert_eq!(runtime.block_on(reopened.refresh_registered()).unwrap(), 2);
    for id in recipients {
        let assets = runtime.block_on(reopened.assets(owner(), id)).unwrap();
        let nia = assets.nia.unwrap();
        let received = nia
            .iter()
            .find(|item| item.asset_id == asset.asset_id)
            .unwrap();
        assert_eq!(received.balance.settled, 50);
        assert_eq!(received.balance.spendable, 50);
        assert_eq!(
            runtime
                .block_on(reopened.transfers(owner(), id))
                .unwrap()
                .len(),
            2
        );
    }
    source.refresh(online, None, vec![], false).unwrap();
    assert_eq!(
        source
            .get_asset_balance(asset.asset_id.clone())
            .unwrap()
            .settled,
        900
    );
    println!(
        "POC_RESULT={}",
        json!({"network":"regtest", "asset_id":asset.asset_id, "transfers":evidence,
        "provider_signing":false, "receiver_balance_each":50, "source_balance":900, "restart_verified":true})
    );
}

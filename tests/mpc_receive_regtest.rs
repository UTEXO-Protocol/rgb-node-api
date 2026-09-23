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
#[ignore = "Requires the isolated regtest indexer in tests/mpc/compose.yaml and RGB_MPC_REGTEST=1"]
fn configured_network_rejects_an_indexer_from_another_chain() {
    use rgb_node_api::mpc::MpcError;
    assert_eq!(std::env::var("RGB_MPC_REGTEST").as_deref(), Ok("1"));
    let _bitcoin = Bitcoin::new();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let control_dir = tempfile::tempdir().unwrap();
    let control = MpcService::new(config(control_dir.path()), Some(TOKEN.into())).unwrap();
    let request = registration(60);
    let control_id = request.wallet_id;
    runtime
        .block_on(control.register(owner(), request))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match runtime.block_on(control.refresh(owner(), control_id)) {
            Ok(()) => break,
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(200)),
            Err(error) => panic!("Regtest indexer control failed: {error}"),
        }
    }
    for network in [
        BitcoinNetwork::Mainnet,
        BitcoinNetwork::Testnet,
        BitcoinNetwork::Testnet4,
        BitcoinNetwork::Signet,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = config(dir.path());
        cfg.network = network.to_string().to_ascii_lowercase();
        let service = MpcService::new(cfg, Some(TOKEN.into())).unwrap();
        let registration = registration_for_network(62, network);
        let id = registration.wallet_id;
        runtime
            .block_on(service.register(owner(), registration))
            .unwrap();
        assert!(matches!(
            runtime.block_on(service.refresh(owner(), id)),
            Err(MpcError::Rgb(rgb_lib::Error::InvalidIndexer { .. }))
        ));
    }
    runtime
        .block_on(control.refresh(owner(), control_id))
        .unwrap();
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

// DYNAMIC_EMBEDDED_POC: shared P2WPKH signatures from isolated fixture keys.
// This exercises the SDK response boundary, not Dynamic authentication/MPC.
fn fixture_sign(psbt: &str, key: &SecretKey, finalized: bool) -> String {
    use rgb_lib::bitcoin::{
        CompressedPublicKey, EcdsaSighashType, Psbt, Witness, ecdsa, secp256k1::Message,
        sighash::SighashCache,
    };
    use std::str::FromStr;
    let secp = Secp256k1::new();
    let public = CompressedPublicKey(key.public_key(&secp));
    let mut signed = Psbt::from_str(psbt).unwrap();
    let mut cache = SighashCache::new(&signed.unsigned_tx);
    for (index, input) in signed.inputs.iter_mut().enumerate() {
        let prevout = input.witness_utxo.as_ref().unwrap();
        let digest = cache
            .p2wpkh_signature_hash(
                index,
                &prevout.script_pubkey,
                prevout.value,
                EcdsaSighashType::All,
            )
            .unwrap();
        let signature = ecdsa::Signature::sighash_all(secp.sign_ecdsa(&Message::from(digest), key));
        if finalized {
            input.final_script_witness = Some(Witness::p2wpkh(&signature, &public.0));
        } else {
            input.partial_sigs.insert(public.into(), signature);
        }
        input.proprietary.clear();
        input.unknown.clear();
    }
    signed.proprietary.clear();
    signed.unknown.clear();
    for output in &mut signed.outputs {
        output.proprietary.clear();
        output.unknown.clear();
    }
    signed.to_string()
}

#[test]
#[ignore = "Uses only temporary wallets; requires tests/mpc/compose.yaml and RGB_MPC_REGTEST=1"]
// DYNAMIC_EMBEDDED_POC: shared test across provider names and fresh wallet state.
fn new_provider_wallets_receive_cancel_recover_and_return_with_native_wallet() {
    use rgb_lib::bitcoin::CompressedPublicKey;
    use rgb_node_api::mpc::{MpcError, Owner, Role, SendRequest};
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
    let online = source.go_online(options.clone()).unwrap();
    bitcoin.rpc(
        "sendtoaddress",
        json!([source.get_address().unwrap(), 1.0]),
        true,
    );
    bitcoin.mine(1);
    let deadline = Instant::now() + Duration::from_secs(30);
    while source
        .get_btc_balance(Some(online), false)
        .unwrap()
        .vanilla
        .spendable
        < 100_000_000
    {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(200));
    }
    let psbt = source
        // Three recipients each need 10,000 sat plus source transaction fees.
        .create_utxos_begin(online, false, Some(5), Some(10_000), 2, false, true)
        .unwrap();
    source
        .create_utxos_end(online, signer.sign_psbt(psbt, None).unwrap())
        .unwrap();
    bitcoin.mine(1);
    let asset = source
        .issue_asset_nia("AUDIT".into(), "Fresh audit asset".into(), 0, vec![2000])
        .unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let mut cfg = config(receiver_dir.path());
    cfg.mpc_send.max_amount = 1000;
    cfg.mpc_send.fee_rate_sat_vb = 3;
    cfg.mpc_send.max_fee_sat = 3000;
    let service = MpcService::new(cfg.clone(), Some(TOKEN.into())).unwrap();
    let mut wallets = Vec::new();
    let providers = [
        ("new-alice", Provider::DynamicEmbedded),
        ("new-bob", Provider::FireblocksVault),
        (
            "new-carol",
            Provider::try_from(format!("adapter_{}", uuid::Uuid::new_v4())).unwrap(),
        ),
    ];
    for (user, provider) in providers {
        let owner = Owner {
            tenant_id: "fresh-regtest".into(),
            user_id: user.into(),
        };
        let mut registration = registration(70);
        registration.provider = provider;
        registration.provider_wallet_ref = uuid::Uuid::new_v4().to_string();
        registration.addresses.retain(|a| a.role == Role::Rgb);
        let mut entropy = [0u8; 32];
        entropy[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        entropy[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        let key = SecretKey::from_slice(&entropy).unwrap();
        let public = CompressedPublicKey(key.public_key(&Secp256k1::new()));
        registration.addresses[0].public_key = public.to_string();
        registration.addresses[0].address = Address::p2wpkh(&public, Network::Regtest).to_string();
        registration.addresses[0].signing_key_id = registration.provider_wallet_ref.clone();
        let id = registration.wallet_id;
        runtime
            .block_on(service.register(owner.clone(), registration))
            .unwrap();
        assert!(
            runtime
                .block_on(service.assets(owner.clone(), id))
                .unwrap()
                .nia
                .unwrap()
                .is_empty()
        );
        let mut request = witness();
        request.amount = Some("100".into());
        let receive = runtime
            .block_on(service.witness(owner.clone(), id, request.clone()))
            .unwrap();
        assert_eq!(
            runtime
                .block_on(service.witness(owner.clone(), id, request))
                .unwrap()
                .invoice,
            receive.invoice
        );
        let data = Invoice::new(receive.invoice).unwrap().invoice_data();
        let begin = source
            .send_begin(
                online,
                HashMap::from([(
                    asset.asset_id.clone(),
                    vec![Recipient {
                        recipient_id: data.recipient_id,
                        assignment: Assignment::Fungible(100),
                        witness_data: Some(WitnessData {
                            amount_sat: 10_000,
                            blinding: None,
                        }),
                        transport_endpoints: data.transport_endpoints,
                    }],
                )]),
                false,
                2,
                1,
                receive.expiration_timestamp,
                false,
                None,
            )
            .unwrap();
        source
            .send_end(online, signer.sign_psbt(begin.psbt, None).unwrap())
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(40);
        loop {
            runtime
                .block_on(service.refresh(owner.clone(), id))
                .unwrap();
            source.refresh(online, None, vec![], false).unwrap();
            bitcoin.mine(1);
            if runtime
                .block_on(service.assets(owner.clone(), id))
                .unwrap()
                .nia
                .unwrap()
                .iter()
                .any(|a| a.asset_id == asset.asset_id && a.balance.spendable == 100)
            {
                break;
            }
            assert!(Instant::now() < deadline, "Fresh receive did not settle");
            std::thread::sleep(Duration::from_millis(200));
        }
        wallets.push((owner, id, key));
    }
    let mut sends = Vec::new();
    for (owner, id, _) in &wallets {
        // A real funded cancellation must free inputs for the next explicit send.
        for cancel in [true, false] {
            let receive = source
                .blind_receive(
                    Some(asset.asset_id.clone()),
                    Assignment::Fungible(40),
                    witness().expiration_timestamp,
                    cfg.proxy_address.clone(),
                    1,
                )
                .unwrap();
            let request = SendRequest {
                request_id: uuid::Uuid::new_v4(),
                invoice: receive.invoice,
                amount: "40".into(),
            };
            let prepared = runtime
                .block_on(service.prepare_send(owner.clone(), *id, request.clone()))
                .unwrap();
            assert_eq!(prepared.state, "AWAITING_SIGNATURE");
            assert!(
                prepared.fee_sat.unwrap() > 308,
                "Configured fee rate was ignored"
            );
            let other = wallets
                .iter()
                .find(|(candidate, _, _)| candidate != owner)
                .unwrap()
                .0
                .clone();
            assert!(matches!(
                runtime.block_on(service.send_status(other.clone(), *id, request.request_id)),
                Err(MpcError::NotFound)
            ));
            assert!(matches!(
                runtime.block_on(service.cancel_send(other.clone(), *id, request.request_id)),
                Err(MpcError::NotFound)
            ));
            assert!(matches!(
                runtime.block_on(service.finish_send(
                    other,
                    *id,
                    request.request_id,
                    "untrusted".into()
                )),
                Err(MpcError::NotFound)
            ));
            assert_eq!(
                runtime
                    .block_on(service.prepare_send(owner.clone(), *id, request.clone()))
                    .unwrap()
                    .psbt,
                prepared.psbt
            );
            if cancel {
                assert_eq!(
                    runtime
                        .block_on(service.cancel_send(owner.clone(), *id, request.request_id))
                        .unwrap()
                        .state,
                    "CANCELLED"
                );
                // The fixture owns the counterparty too. Its now-unused blind
                // invoice must not reserve a source UTXO for the later native send.
                source
                    .fail_transfers(online, Some(receive.batch_transfer_idx), false, false)
                    .unwrap();
                assert_eq!(
                    runtime
                        .block_on(service.prepare_send(owner.clone(), *id, request))
                        .unwrap()
                        .state,
                    "CANCELLED"
                );
            } else {
                sends.push((request, prepared));
            }
        }
    }
    drop(service);
    // Simulate a crash after RGB committed prepare but before the API saved its response.
    let path = receiver_dir.path().join(format!(
        "mpc/sends/{}/{}.json",
        wallets[0].1, sends[0].0.request_id
    ));
    let mut journal: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    journal["view"]["state"] = "PREPARING".into();
    for field in ["psbt", "txid", "batch_transfer_idx"] {
        journal["view"][field] = Value::Null;
    }
    journal["original_psbt"] = Value::Null;
    std::fs::write(&path, serde_json::to_vec(&journal).unwrap()).unwrap();
    // The new policy must not invalidate an operation already reviewed under the old one.
    let mut changed = cfg.clone();
    changed.mpc_send.max_amount = 1;
    changed.mpc_send.max_fee_sat = 1;
    let service = MpcService::new(changed, Some(TOKEN.into())).unwrap();
    for (index, ((owner, id, key), (request, prepared))) in wallets.iter().zip(&sends).enumerate() {
        let recovered = runtime
            .block_on(service.send_status(owner.clone(), *id, request.request_id))
            .unwrap();
        assert_eq!(recovered.psbt, prepared.psbt);
        assert_eq!(recovered.txid, prepared.txid);
        let signed = fixture_sign(recovered.psbt.as_ref().unwrap(), key, index == 1);
        let wrong = fixture_sign(
            recovered.psbt.as_ref().unwrap(),
            &wallets[(index + 1) % wallets.len()].2,
            false,
        );
        assert!(
            runtime
                .block_on(service.finish_send(owner.clone(), *id, request.request_id, wrong))
                .is_err()
        );
        let submitted = runtime
            .block_on(service.finish_send(owner.clone(), *id, request.request_id, signed.clone()))
            .unwrap();
        assert_eq!(submitted.txid, prepared.txid);
        assert!(matches!(
            runtime.block_on(service.cancel_send(owner.clone(), *id, request.request_id)),
            Err(MpcError::Conflict(_))
        ));
        let deadline = Instant::now() + Duration::from_secs(40);
        loop {
            source.refresh(online, None, vec![], false).unwrap();
            let state = runtime
                .block_on(service.send_status(owner.clone(), *id, request.request_id))
                .unwrap();
            if state.state == "SETTLED" {
                break;
            }
            bitcoin.mine(1);
            assert!(
                Instant::now() < deadline,
                "Fresh return did not settle: {}",
                state.state
            );
            std::thread::sleep(Duration::from_millis(200));
        }
        assert_eq!(
            runtime
                .block_on(service.finish_send(owner.clone(), *id, request.request_id, signed))
                .unwrap()
                .state,
            "SETTLED"
        );
    }
    drop(service);
    let service = MpcService::new(cfg.clone(), Some(TOKEN.into())).unwrap();
    for (owner, id, _) in &wallets {
        let assets = runtime
            .block_on(service.assets(owner.clone(), *id))
            .unwrap();
        let balance = &assets.nia.unwrap()[0].balance;
        assert_eq!(
            (balance.settled, balance.future, balance.spendable),
            (60, 60, 60)
        );
    }
    source.refresh(online, None, vec![], false).unwrap();
    assert_eq!(
        source
            .get_asset_balance(asset.asset_id.clone())
            .unwrap()
            .settled,
        1820
    );

    // Standard native witness receive still works alongside the MPC path.
    let native_dir = tempfile::tempdir().unwrap();
    let native_keys = generate_keys(BitcoinNetwork::Regtest, WitnessVersion::Taproot);
    let mut public_keys = SinglesigKeys::from_keys(&native_keys, None);
    public_keys.mnemonic = None;
    let mut native = Wallet::new(wallet_data(native_dir.path()), public_keys).unwrap();
    let native_online = native.go_online(options).unwrap();
    let receive = native
        .witness_receive(
            None,
            Assignment::Fungible(25),
            witness().expiration_timestamp,
            cfg.proxy_address,
            1,
        )
        .unwrap();
    let data = Invoice::new(receive.invoice).unwrap().invoice_data();
    let begin = source
        .send_begin(
            online,
            HashMap::from([(
                asset.asset_id.clone(),
                vec![Recipient {
                    recipient_id: data.recipient_id,
                    assignment: Assignment::Fungible(25),
                    witness_data: Some(WitnessData {
                        amount_sat: 1000,
                        blinding: None,
                    }),
                    transport_endpoints: data.transport_endpoints,
                }],
            )]),
            false,
            2,
            1,
            receive.expiration_timestamp,
            false,
            None,
        )
        .unwrap();
    source
        .send_end(online, signer.sign_psbt(begin.psbt, None).unwrap())
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(40);
    loop {
        native.refresh(native_online, None, vec![], false).unwrap();
        source.refresh(online, None, vec![], false).unwrap();
        if native
            .get_asset_balance(asset.asset_id.clone())
            .is_ok_and(|b| b.spendable == 25)
        {
            break;
        }
        bitcoin.mine(1);
        assert!(
            Instant::now() < deadline,
            "Native witness receive did not settle"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
    println!(
        "AUDIT_RESULT={}",
        json!({"network":"regtest", "fresh_provider_wallets":3,
        "providers":["dynamic_embedded", "fireblocks_vault", "new_adapter"],
        "fresh_asset":asset.asset_id, "provider_balance_each":60, "native_received":25,
        "cancel_and_prepare_recovery":true, "saved_policy_after_restart":true,
        "external_signatures":"partial_and_final_p2wpkh", "provider_signing":false})
    );
}

//! Actual regtest receive using a local watch-only sender and a separate test
//! signer. No provider API or EVM bridge is mocked into the RGB validation path.
mod common;
use common::*;
use rgb_lib::{
    AssetSchema, Assignment, BitcoinNetwork,
    bitcoin::{
        Psbt, TapSighashType, Witness,
        hashes::Hash,
        key::TapTweak,
        secp256k1::{Keypair, Message, Secp256k1, SecretKey},
        sighash::{Prevouts, SighashCache},
        taproot,
    },
    keys::{WitnessVersion, generate_keys},
    wallet::{
        DatabaseType, Invoice, MpcWallet, OnlineOptions, Recipient, RgbWalletOpsOffline,
        RgbWalletOpsOnline, SinglesigKeys, Wallet, WalletData, WitnessData,
    },
};
use rgb_node_api::mpc::{MpcService, RegistryProvider, SendRequest};
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
            format!("http://127.0.0.1:19543/wallet/{}", self.wallet)
        } else {
            "http://127.0.0.1:19543".into()
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

use std::str::FromStr;
fn sign(psbt: &str, keys: &[Keypair], final_witness: bool) -> String {
    let mut signed = Psbt::from_str(psbt).unwrap();
    let prevouts: Vec<_> = signed
        .inputs
        .iter()
        .map(|i| i.witness_utxo.clone().unwrap())
        .collect();
    let secp = Secp256k1::new();
    let mut cache = SighashCache::new(&signed.unsigned_tx);
    for (index, input) in signed.inputs.iter_mut().enumerate() {
        let key = keys
            .iter()
            .copied()
            .find(|k| {
                rgb_lib::bitcoin::ScriptBuf::new_p2tr(&secp, k.x_only_public_key().0, None)
                    == prevouts[index].script_pubkey
            })
            .unwrap();
        let hash = input
            .sighash_type
            .map(|v| v.taproot_hash_ty().unwrap())
            .unwrap_or(TapSighashType::Default);
        let digest = cache
            .taproot_key_spend_signature_hash(index, &Prevouts::All(&prevouts), hash)
            .unwrap();
        let sig = taproot::Signature {
            signature: secp.sign_schnorr_no_aux_rand(
                &Message::from_digest(digest.to_byte_array()),
                &key.tap_tweak(&secp, None).to_keypair(),
            ),
            sighash_type: hash,
        };
        if final_witness {
            input.final_script_witness = Some(Witness::from_slice(&[sig.to_vec()]));
        } else {
            input.tap_key_sig = Some(sig);
        }
    }
    signed.to_string()
}
fn wait(mut work: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if work() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "Fixture did not settle before timeout"
        );
        std::thread::sleep(Duration::from_millis(250));
    }
}
#[test]
#[ignore = "Requires isolated tests/dfns/compose.yaml and RGB_DFNS_REGTEST=1; no provider calls"]
fn two_role_receive_send_twice_restart_and_spend_vanilla() {
    assert_eq!(std::env::var("RGB_DFNS_REGTEST").as_deref(), Ok("1"));
    let bitcoin = Bitcoin::new();
    let source_dir = tempfile::tempdir().unwrap();
    let receiver_dir = tempfile::tempdir().unwrap();
    let keys = generate_keys(BitcoinNetwork::Regtest, WitnessVersion::Taproot);
    let mut source = Wallet::new(
        wallet_data(source_dir.path()),
        SinglesigKeys::from_keys(&keys, None),
    )
    .unwrap();
    let options = OnlineOptions {
        indexer_url: "127.0.0.1:51111".into(),
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
    wait(|| {
        source
            .get_btc_balance(Some(online), false)
            .unwrap()
            .vanilla
            .spendable
            >= 100_000_000
    });
    let begun = source
        .create_utxos_begin(online, false, Some(5), Some(10_000), 2, false, true)
        .unwrap();
    source
        .create_utxos_end(online, source.sign_psbt(begun, None).unwrap())
        .unwrap();
    bitcoin.mine(1);
    let asset = source
        .issue_asset_nia("DFNSNIA".into(), "Dfns isolated NIA".into(), 0, vec![2000])
        .unwrap();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let cfg = config(receiver_dir.path());
    let mut registration = registration(71);
    let secp = Secp256k1::new();
    let signing_keys = registration
        .addresses
        .iter_mut()
        .map(|binding| {
            let mut entropy = [0u8; 32];
            entropy[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
            entropy[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
            let key = Keypair::from_secret_key(&secp, &SecretKey::from_slice(&entropy).unwrap());
            binding.internal_key = key.x_only_public_key().0.to_string();
            binding.public_key = key
                .x_only_public_key()
                .0
                .tap_tweak(&secp, None)
                .0
                .to_string();
            binding.address = rgb_lib::bitcoin::Address::p2tr(
                &secp,
                key.x_only_public_key().0,
                None,
                rgb_lib::bitcoin::Network::Regtest,
            )
            .to_string();
            key
        })
        .collect::<Vec<_>>();
    let id = registration.wallet_id;
    let service = MpcService::new(cfg.clone(), Some(TOKEN.into())).unwrap();
    rt.block_on(service.register(owner(), registration.clone()))
        .unwrap();
    let mut req = witness();
    req.asset_id = Some(asset.asset_id.clone());
    req.amount = Some("100".into());
    let receive = rt
        .block_on(service.witness(owner(), id, req.clone()))
        .unwrap();
    assert_eq!(
        receive.invoice,
        rt.block_on(service.witness(owner(), id, req))
            .unwrap()
            .invoice
    );
    let data = Invoice::new(receive.invoice).unwrap().invoice_data();
    // A 1000-sat RGB carrier forces both independent keys into the first send.
    let begun = source
        .send_begin(
            online,
            HashMap::from([(
                asset.asset_id.clone(),
                vec![Recipient {
                    recipient_id: data.recipient_id,
                    assignment: Assignment::Fungible(100),
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
        .send_end(online, source.sign_psbt(begun.psbt, None).unwrap())
        .unwrap();
    bitcoin.rpc(
        "sendtoaddress",
        json!([registration.addresses[1].address, 0.001]),
        true,
    );
    bitcoin.mine(1);
    wait(|| {
        rt.block_on(service.refresh(owner(), id)).unwrap();
        source.refresh(online, None, vec![], false).unwrap();
        bitcoin.mine(1);
        rt.block_on(service.assets(owner(), id))
            .unwrap()
            .nia
            .unwrap()
            .iter()
            .any(|a| a.balance.spendable == 100)
    });
    drop(service);
    let mut prior_txid = None;
    for round in 0..2 {
        let service = MpcService::new(cfg.clone(), Some(TOKEN.into())).unwrap();
        let invoice = source
            .blind_receive(
                Some(asset.asset_id.clone()),
                Assignment::Fungible(25),
                witness().expiration_timestamp,
                cfg.proxy_address.clone(),
                1,
            )
            .unwrap();
        let request = SendRequest {
            request_id: uuid::Uuid::new_v4(),
            invoice: invoice.invoice,
            amount: "25".into(),
            asset_id: asset.asset_id.clone(),
        };
        let prepared = rt
            .block_on(service.prepare_send(owner(), id, request.clone()))
            .unwrap();
        assert_eq!(prepared.key_groups.len(), 2);
        assert_eq!(prepared.change.len(), 2);
        assert_eq!(
            prepared
                .change
                .iter()
                .find(|c| c.role == rgb_node_api::mpc::Role::Rgb)
                .unwrap()
                .amount_sat,
            "1000"
        );
        let psbt = Psbt::from_str(prepared.psbt.as_ref().unwrap()).unwrap();
        if let Some(txid) = prior_txid {
            assert!(
                psbt.unsigned_tx
                    .input
                    .iter()
                    .any(|i| i.previous_output.txid == txid)
            );
        }
        assert_eq!(
            prepared.psbt,
            rt.block_on(service.prepare_send(owner(), id, request.clone()))
                .unwrap()
                .psbt
        );
        assert!(
            rt.block_on(service.send_status(
                rgb_node_api::mpc::Owner {
                    tenant_id: "poc".into(),
                    user_id: "bob".into()
                },
                id,
                request.request_id
            ))
            .is_err()
        );
        drop(service);
        let path = receiver_dir
            .path()
            .join(format!("mpc/sends/{id}/{}.json", request.request_id));
        let mut journal: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        // Crash after committing prepare, before returning the PSBT.
        journal["view"]["state"] = "PREPARING".into();
        journal["view"]["psbt"] = Value::Null;
        journal["view"]["txid"] = Value::Null;
        journal["view"]["batch_transfer_idx"] = Value::Null;
        journal["original_psbt"] = Value::Null;
        std::fs::write(&path, serde_json::to_vec(&journal).unwrap()).unwrap();
        let service = MpcService::new(cfg.clone(), Some(TOKEN.into())).unwrap();
        let recovered = rt
            .block_on(service.send_status(owner(), id, request.request_id))
            .unwrap();
        assert_eq!(recovered.psbt, prepared.psbt);
        let signed = sign(recovered.psbt.as_ref().unwrap(), &signing_keys, round == 1);
        let mut wrong = Psbt::from_str(&signed).unwrap();
        wrong.unsigned_tx.output[1].value += rgb_lib::bitcoin::Amount::from_sat(1);
        assert!(
            rt.block_on(service.finish_send(owner(), id, request.request_id, wrong.to_string()))
                .is_err()
        );
        if round == 0 {
            // Crash before send_end: the final verified PSBT is durably saved first.
            let original = Psbt::from_str(prepared.psbt.as_ref().unwrap()).unwrap();
            let inputs = original
                .unsigned_tx
                .input
                .iter()
                .zip(&original.inputs)
                .map(
                    |(i, p)| rgb_node_api::taproot_signing::VerifiedTaprootInput {
                        outpoint: i.previous_output,
                        prevout: p.witness_utxo.clone().unwrap(),
                        output_key: rgb_lib::bitcoin::secp256k1::XOnlyPublicKey::from_slice(
                            &p.witness_utxo.as_ref().unwrap().script_pubkey.as_bytes()[2..],
                        )
                        .unwrap(),
                    },
                )
                .collect::<Vec<_>>();
            let finalized =
                rgb_node_api::taproot_signing::PreparedTaproot::prepare(original, &inputs, 2000)
                    .unwrap()
                    .finalize_psbt(&Psbt::from_str(&signed).unwrap())
                    .unwrap();
            drop(service);
            let mut journal: Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            journal["view"]["state"] = "SUBMITTING".into();
            journal["submission_started"] = true.into();
            journal["signed_psbt"] = finalized.to_string().into();
            std::fs::write(&path, serde_json::to_vec(&journal).unwrap()).unwrap();
        } else {
            rt.block_on(service.finish_send(owner(), id, request.request_id, signed.clone()))
                .unwrap();
            drop(service);
        }
        let service = MpcService::new(cfg.clone(), Some(TOKEN.into())).unwrap();
        wait(|| {
            let status = rt
                .block_on(service.send_status(owner(), id, request.request_id))
                .unwrap();
            source.refresh(online, None, vec![], false).unwrap();
            bitcoin.mine(1);
            status.state == "SETTLED"
        });
        assert_eq!(
            rt.block_on(service.finish_send(owner(), id, request.request_id, signed))
                .unwrap()
                .state,
            "SETTLED"
        );
        assert_eq!(
            rt.block_on(service.assets(owner(), id))
                .unwrap()
                .nia
                .unwrap()[0]
                .balance
                .spendable,
            100 - (round + 1) * 25
        );
        prior_txid = Some(psbt.unsigned_tx.compute_txid());
        drop(service);
    }
    // Reopen only after the API releases its process lock, then spend vanilla
    // through the public library begin/sign/end API; colored allocations survive.
    let dir = receiver_dir.path().join(format!("mpc/wallets/{id}"));
    let mut data = wallet_data(&dir);
    data.reuse_addresses = true;
    let mut wallet = MpcWallet::new(
        data,
        id.to_string(),
        Box::new(
            RegistryProvider::new(BitcoinNetwork::Regtest, registration.addresses.clone()).unwrap(),
        ),
    )
    .unwrap();
    let source_online = online;
    let online = wallet.go_online(options).unwrap();
    let balances = wallet.get_btc_balance(Some(online), false).unwrap();
    assert_eq!(balances.colored.spendable, 1000);
    assert!(balances.vanilla.spendable > 90_000);
    let destination = bitcoin
        .rpc("getnewaddress", json!([]), true)
        .as_str()
        .unwrap()
        .to_owned();
    let begun = wallet
        .send_btc_begin(online, destination, 10_000, 2, false, false)
        .unwrap();
    let psbt = Psbt::from_str(&begun).unwrap();
    assert!(psbt.inputs.iter().all(|i| {
        i.witness_utxo.as_ref().unwrap().script_pubkey
            != rgb_lib::bitcoin::Address::from_str(&registration.addresses[0].address)
                .unwrap()
                .assume_checked()
                .script_pubkey()
    }));
    wallet
        .send_btc_end(online, sign(&begun, &signing_keys, true))
        .unwrap();
    bitcoin.mine(1);
    wallet.refresh(online, None, vec![], false).unwrap();
    assert_eq!(
        wallet
            .get_asset_balance(asset.asset_id.clone())
            .unwrap()
            .spendable,
        50
    );
    // The only colored UTXO now contains two contracts. Exhausting the first
    // must retain a carrier for the unrelated allocation on that same input.
    let other = wallet
        .issue_asset_nia("OTHER".into(), "Unrelated allocation".into(), 0, vec![7])
        .unwrap();
    let colored = wallet.list_unspents(Some(online), false, false).unwrap();
    assert!(colored.iter().any(|u| u.rgb_allocations.len() == 2));
    drop(wallet);
    let mut final_cfg = cfg.clone();
    final_cfg.mpc_send.max_amount = 50;
    let service = MpcService::new(final_cfg, Some(TOKEN.into())).unwrap();
    for (contract, amount, retains_other) in [
        (asset.asset_id.clone(), 50, true),
        (other.asset_id.clone(), 7, false),
    ] {
        let receive = source
            .blind_receive(
                None,
                Assignment::Fungible(amount),
                witness().expiration_timestamp,
                cfg.proxy_address.clone(),
                1,
            )
            .unwrap();
        let mut invoice = rgbinvoice::RgbInvoice::from_str(&receive.invoice).unwrap();
        invoice.contract = Some(contract.parse().unwrap());
        invoice.schema = Some(AssetSchema::Nia.into());
        let request = SendRequest {
            request_id: uuid::Uuid::new_v4(),
            invoice: invoice.to_string(),
            amount: amount.to_string(),
            asset_id: contract.clone(),
        };
        let prepared = rt
            .block_on(service.prepare_send(owner(), id, request.clone()))
            .unwrap();
        assert_eq!(prepared.state, "AWAITING_SIGNATURE");
        assert_eq!(
            prepared
                .change
                .iter()
                .any(|change| change.role == rgb_node_api::mpc::Role::Rgb),
            retains_other
        );
        let signed = sign(prepared.psbt.as_ref().unwrap(), &signing_keys, false);
        rt.block_on(service.finish_send(owner(), id, request.request_id, signed))
            .unwrap();
        wait(|| {
            let status = rt
                .block_on(service.send_status(owner(), id, request.request_id))
                .unwrap();
            source.refresh(source_online, None, vec![], false).unwrap();
            bitcoin.mine(1);
            status.state == "SETTLED"
        });
        let assets = rt
            .block_on(service.assets(owner(), id))
            .unwrap()
            .nia
            .unwrap();
        assert_eq!(
            assets
                .iter()
                .find(|a| a.asset_id == contract)
                .unwrap()
                .balance
                .spendable,
            0
        );
        if retains_other {
            assert_eq!(
                assets
                    .iter()
                    .find(|a| a.asset_id == other.asset_id)
                    .unwrap()
                    .balance
                    .spendable,
                7
            );
        }
    }
    println!(
        "DFNS_REGTEST_RESULT: witness receive=100, two two-key RGB sends=25+25, remaining RGB=50, vanilla BTC spend=10000 sat; unrelated allocation retained when first contract exhausted; final all-asset send omits colored carrier; prepare/submit restart recovered; no provider calls"
    );
}

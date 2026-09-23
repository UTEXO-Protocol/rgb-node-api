//! Gateway worker helper for the registered Testnet3 fungible Sandbox wallet. No secrets,
//! provider calls, or public HTTP spending endpoint. JSON stdin / JSON stdout.
use std::{collections::HashMap, fs, io::Read, path::Path, str::FromStr};

use anyhow::{Context, Result, ensure};
use rgb_lib::{
    AssetSchema, Assignment, BitcoinNetwork,
    bitcoin::{
        Address, CompressedPublicKey, Network, Psbt, Transaction, consensus::deserialize,
        secp256k1::PublicKey,
    },
    wallet::{
        AssetFilter, DatabaseType, Invoice, MpcWallet, OnlineOptions, Recipient,
        RgbWalletOpsOffline, WalletData,
    },
};
use rgb_node_api::{
    mpc::{Registration, RegistryProvider},
    signing::{InputSignature, PreparedP2wpkh, VerifiedInput},
};
use serde_json::{Value, json};

const ASSET: &str = "rgb:qXB4xkhB-3pmU6PB-rTy_qNd-ErrO4OQ-8GFLLQq-gzGoing";
fn asset(v: &Value) -> Result<&str> {
    let id = v["assetId"].as_str().unwrap_or(ASSET);
    ensure!(
        id == ASSET || v["bfaMock"] == true,
        "Non-legacy asset requires explicit BFA mock mode"
    );
    Ok(id)
}
fn schemas(v: &Value) -> Vec<AssetSchema> {
    if v["bfaMock"] == true {
        vec![AssetSchema::Nia, AssetSchema::Bfa]
    } else {
        vec![AssetSchema::Nia]
    }
}
fn amount(v: &Value) -> Result<u64> {
    let amount: u64 = string(v, "amount")?.parse()?;
    ensure!((1..=25).contains(&amount), "POC amount must be 1..25");
    Ok(amount)
}
fn string<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str().with_context(|| format!("Missing {k}"))
}

fn invoice(v: &Value) -> Result<rgb_lib::wallet::InvoiceData> {
    let data = Invoice::new(string(v, "invoice")?.into())?.invoice_data();
    ensure!(
        data.network == BitcoinNetwork::Testnet
            && data.assignment == Assignment::Fungible(amount(v)?)
            && (data.asset_id.as_deref() == Some(asset(v)?)
                || (data.asset_id.is_none() && v["allowGenericAsset"] == true)),
        "Wrong invoice network/asset/amount"
    );
    ensure!(
        data.transport_endpoints.len() == 1
            && data.transport_endpoints[0].split('?').next()
                == Some("rpcs://proxy.iriswallet.com/0.2/json-rpc"),
        "Wrong transport"
    );
    Ok(data)
}

fn prepare(v: &Value) -> Result<PreparedP2wpkh> {
    invoice(v)?;
    let binding = &v["binding"];
    ensure!(
        binding["environment"] == "sandbox"
            && binding["assetId"] == "BTC_TEST"
            && binding["vault"]["id"] == "2",
        "Wrong provider binding"
    );
    let public_key =
        CompressedPublicKey::from_str(string(&binding["publicKeyInfo"], "publicKey")?)?;
    let address = Address::from_str(string(&binding["wallet"], "address")?)?
        .require_network(Network::Testnet)?;
    ensure!(
        Address::p2wpkh(&public_key, Network::Testnet) == address,
        "Wrong key/address"
    );
    let psbt = Psbt::from_str(string(v, "psbt")?)?;
    ensure!(
        !psbt.inputs.is_empty() && psbt.inputs.len() <= 10,
        "Invalid input count"
    );
    let data = invoice(v)?;
    let recipient = rgb_lib::utils::script_buf_from_recipient_id(data.recipient_id)?;
    ensure!(
        psbt.unsigned_tx.output.len() == if recipient.is_some() { 3 } else { 2 },
        "Unexpected outputs"
    );
    let commitment = &psbt.unsigned_tx.output[0];
    ensure!(
        commitment.value.to_sat() == 0
            && commitment.script_pubkey.is_op_return()
            && commitment.script_pubkey.len() == 34,
        "Expected RGB commitment"
    );
    let change = psbt.unsigned_tx.output.last().context("No change")?;
    ensure!(
        change.script_pubkey == address.script_pubkey()
            && change.value >= change.script_pubkey.minimal_non_dust(),
        "Wrong change"
    );
    if let Some(script) = recipient {
        ensure!(
            psbt.unsigned_tx.output[1].script_pubkey == script
                && psbt.unsigned_tx.output[1].value.to_sat() == 1000,
            "Wrong invoice output"
        );
    }
    let mut verified = Vec::new();
    for input in &psbt.unsigned_tx.input {
        let txid = input.previous_output.txid.to_string();
        let previous: Transaction =
            deserialize(&hex::decode(string(&v["previousTransactions"], &txid)?)?)?;
        ensure!(
            previous.compute_txid() == input.previous_output.txid,
            "Wrong previous transaction"
        );
        let prevout = previous
            .output
            .get(input.previous_output.vout as usize)
            .context("Missing prevout")?
            .clone();
        ensure!(
            prevout.script_pubkey == address.script_pubkey(),
            "Unknown provider input"
        );
        verified.push(VerifiedInput {
            outpoint: input.previous_output,
            prevout,
            public_key,
            signing_key_id: "sandbox:2:BTC_TEST:0:0".into(),
        });
    }
    PreparedP2wpkh::prepare(psbt, &verified, 2000).map_err(Into::into)
}

fn finalize_raw(prepared: &PreparedP2wpkh, raw: &Value, binding: &Value) -> Result<Psbt> {
    let plan = prepared.signing_plan();
    ensure!(
        raw["status"] == "COMPLETED"
            && raw["operation"] == "RAW"
            && raw["assetId"] == "BTC_TEST"
            && raw["source"]["id"] == "2",
        "Wrong RAW transaction"
    );
    let messages = raw["signedMessages"]
        .as_array()
        .context("Missing signatures")?;
    ensure!(
        messages.len() == plan.len(),
        "One signature per input required"
    );
    let mut signatures = Vec::new();
    // Fireblocks may return signedMessages in a different order from the request.
    // Bind each response to its digest; finalize rejects duplicate/missing input
    // indexes and cryptographically verifies every signature on the original PSBT.
    for message in messages {
        let input = plan
            .iter()
            .find(|input| message["content"] == hex::encode(input.digest))
            .context("Unknown signed digest")?;
        ensure!(
            message["algorithm"] == "MPC_ECDSA_SECP256K1"
                && message["derivationPath"] == binding["publicKeyInfo"]["derivationPath"],
            "Wrong signing algorithm/path"
        );
        let mut key = hex::decode(string(message, "publicKey")?)?;
        if key.len() == 64 {
            key.insert(0, 4);
        }
        ensure!(
            PublicKey::from_slice(&key)? == input.public_key.0,
            "Wrong key"
        );
        let r = hex::decode(string(&message["signature"], "r")?)?;
        let s = hex::decode(string(&message["signature"], "s")?)?;
        ensure!(r.len() == 32 && s.len() == 32, "Wrong scalars");
        signatures.push(InputSignature {
            input_index: input.input_index,
            compact: [r, s].concat(),
        });
    }
    Ok(prepared.finalize(&signatures)?)
}

fn inspect(v: &Value) -> Result<Value> {
    let prepared = prepare(v)?;
    let plan = prepared.signing_plan();
    let psbt = Psbt::from_str(string(v, "psbt")?)?;
    let mut result = json!({"purpose":"testnet3-asset-ui", "rgbAssetId":asset(v)?, "messages":plan.iter().map(|p| json!({"content":hex::encode(p.digest),"inputIndex":p.input_index})).collect::<Vec<_>>(),
        "assetId":"BTC_TEST", "vaultId":"2", "publicKey":plan[0].public_key.to_string(),
        "feeSat":prepared.fee_sat(), "txid":psbt.unsigned_tx.compute_txid().to_string(),
        "inputs":psbt.unsigned_tx.input.iter().map(|i| json!({"txid":i.previous_output.txid.to_string(),"vout":i.previous_output.vout})).collect::<Vec<_>>()});
    if let Some(raw) = v.get("rawResult") {
        let signed = finalize_raw(&prepared, raw, &v["binding"])?;
        let tx = signed.clone().extract_tx()?;
        ensure!(
            prepared.fee_sat() >= tx.vsize() as u64 * 2,
            "Fee below selected 2 sat/vB"
        );
        result["signedPsbt"] = json!(signed.to_string());
        result["signatureVerified"] = json!(true);
        result["vsize"] = json!(tx.vsize());
    }
    Ok(result)
}

fn wallet(v: &Value) -> Result<Value> {
    let registration: Registration = serde_json::from_value(v["registration"].clone())?;
    ensure!(
        registration.bitcoin_network == "testnet"
            && registration.provider_wallet_ref == "vault/2/BTC_TEST",
        "Wrong registered wallet"
    );
    let root = Path::new(string(v, "stateDirectory")?).join("mpc");
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.join("service.lock"))?;
    lock.try_lock()
        .context("MPC API or another operator is running")?;
    let saved: Value = serde_json::from_slice(&fs::read(
        root.join("registrations")
            .join(format!("{}.json", registration.wallet_id)),
    )?)?;
    ensure!(
        saved["registration"] == v["registration"]
            && saved["owner"] == json!({"tenant_id":"sandbox-poc","user_id":"vault-2"}),
        "Persisted owner/binding mismatch"
    );
    let mut wallet = MpcWallet::new(
        WalletData {
            data_dir: root
                .join("wallets")
                .join(registration.wallet_id.to_string())
                .to_string_lossy()
                .into(),
            bitcoin_network: BitcoinNetwork::Testnet,
            database_type: DatabaseType::Sqlite,
            max_allocations_per_utxo: 5,
            supported_schemas: schemas(v),
            reuse_addresses: true,
        },
        registration.wallet_id.to_string(),
        Box::new(RegistryProvider::new(
            BitcoinNetwork::Testnet,
            registration.addresses,
        )?),
    )?;
    ensure!(
        v["indexerUrl"] == "ssl://electrum.iriswallet.com:50013",
        "Wrong indexer"
    );
    let online = wallet.go_online(OnlineOptions {
        indexer_url: string(v, "indexerUrl")?.into(),
        skip_consistency_check: false,
        vanilla_sync_lookback: 20,
        eth_rpc_url: v["ethRpcUrl"].as_str().map(str::to_owned),
    })?;
    let selected_asset = asset(v)?;
    let command = string(v, "command")?;
    if command == "status" {
        wallet.refresh(online, None, vec![], false)?;
    }
    let assets = wallet.list_assets(schemas(v))?;
    let known = assets
        .nia
        .as_ref()
        .is_some_and(|rows| rows.iter().any(|a| a.asset_id == selected_asset))
        || assets
            .bfa
            .as_ref()
            .is_some_and(|rows| rows.iter().any(|a| a.asset_id == selected_asset));
    match command {
        "status" => Ok(json!({"assets":assets,
                "transfers":if known { wallet.list_transfers(AssetFilter::Id(asset(v)?.into()),None)? } else { vec![] },
                "unspents":wallet.list_unspents(Some(online),false,false)?})),
        "witness" => Ok(serde_json::to_value(wallet.witness_receive(
            known.then(|| selected_asset.into()),
            Assignment::Fungible(amount(v)?),
            v["expiration"].as_u64().context("Missing expiry")?,
            vec!["rpcs://proxy.iriswallet.com/0.2/json-rpc".into()],
            1,
        )?)?),
        "begin" => {
            let data = invoice(v)?;
            let expiry = data
                .expiration_timestamp
                .context("Invoice expiry required")?;
            let recipient = Recipient {
                recipient_id: data.recipient_id.clone(),
                witness_data: rgb_lib::utils::script_buf_from_recipient_id(
                    data.recipient_id.clone(),
                )?
                .map(|_| rgb_lib::wallet::WitnessData {
                    amount_sat: 1000,
                    blinding: None,
                }),
                assignment: Assignment::Fungible(amount(v)?),
                transport_endpoints: data.transport_endpoints,
            };
            Ok(serde_json::to_value(wallet.send_begin(
                online,
                HashMap::from([(asset(v)?.into(), vec![recipient])]),
                false,
                2,
                1,
                expiry,
                false,
            )?)?)
        }
        "end" => {
            let checked = inspect(v)?;
            // Reconstruct signed PSBT from the original plus verified provider response.
            Ok(serde_json::to_value(wallet.send_end(
                online,
                string(&checked, "signedPsbt")?.into(),
            )?)?)
        }
        _ => anyhow::bail!("Unknown wallet command"),
    }
}

fn main() -> Result<()> {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let value: Value = serde_json::from_str(&input)?;
    let result = match string(&value, "command")? {
        "inspect" => inspect(&value)?,
        "psbt-inputs" => json!(Psbt::from_str(string(&value,"psbt")?)?.unsigned_tx.input.iter().map(|i| json!({"txid":i.previous_output.txid.to_string(),"vout":i.previous_output.vout})).collect::<Vec<_>>()),
        "invoice" => serde_json::to_value(invoice(&value)?)?,
        _ => wallet(&value)?,
    };
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_receipt_allows_generic_asset_only_with_explicit_internal_opt_in() {
        let recipient = "tb3:utxob:7TjnbTy5-H~OndMD-98vlg7F-Fp1VnAt-8J5Ju_P-F~X03DP-kVVUC";
        let make = |asset: &str| {
            format!(
                "{asset}/~/ae/{recipient}?assignment_name=assetOwner&expiry=2000000000&endpoints=rpcs://proxy.iriswallet.com/0.2/json-rpc"
            )
        };
        let mut value =
            json!({"invoice":make("rgb:~"), "amount":"1", "assetId":ASSET, "bfaMock":true});
        assert!(invoice(&value).is_err());
        value["allowGenericAsset"] = json!(true);
        assert!(invoice(&value).unwrap().asset_id.is_none());
        value["amount"] = json!("2");
        assert!(invoice(&value).is_err());
        value["amount"] = json!("1");
        value["invoice"] = json!(make(ASSET));
        value["assetId"] = json!("rgb:another-asset");
        assert!(invoice(&value).is_err());
    }
    use rgb_lib::bitcoin::{
        Amount, OutPoint, ScriptBuf, TxIn, TxOut, Txid, absolute,
        hashes::Hash,
        secp256k1::{Message, Secp256k1, SecretKey},
        transaction,
    };

    fn fixture(count: usize) -> (PreparedP2wpkh, Value, Value) {
        let secp = Secp256k1::new();
        // Deterministic offline fixture only; no provider or funded wallet.
        let secret = SecretKey::from_slice(&[7; 32]).unwrap();
        let public_key = CompressedPublicKey(secret.public_key(&secp));
        let script = Address::p2wpkh(&public_key, Network::Testnet).script_pubkey();
        let verified: Vec<_> = (0..count)
            .map(|index| VerifiedInput {
                outpoint: OutPoint::new(Txid::from_byte_array([index as u8 + 1; 32]), 0),
                prevout: TxOut {
                    value: Amount::from_sat(1000),
                    script_pubkey: script.clone(),
                },
                public_key,
                signing_key_id: "fixture".into(),
            })
            .collect();
        let mut psbt = Psbt::from_unsigned_tx(Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: verified
                .iter()
                .map(|input| TxIn {
                    previous_output: input.outpoint,
                    ..Default::default()
                })
                .collect(),
            output: vec![
                TxOut {
                    value: Amount::ZERO,
                    script_pubkey: ScriptBuf::new_op_return([1; 32]),
                },
                TxOut {
                    value: Amount::from_sat(count as u64 * 1000 - 446),
                    script_pubkey: script,
                },
            ],
        })
        .unwrap();
        for (input, verified) in psbt.inputs.iter_mut().zip(&verified) {
            input.witness_utxo = Some(verified.prevout.clone());
        }
        let prepared = PreparedP2wpkh::prepare(psbt, &verified, 2000).unwrap();
        let path = json!([2147483692u64, 2147483649u64, 2147483648u64, 0, 0]);
        let messages: Vec<_> = prepared
            .signing_plan()
            .iter()
            .map(|input| {
                let signature = secp
                    .sign_ecdsa(&Message::from_digest(input.digest), &secret)
                    .serialize_compact();
                json!({"content":hex::encode(input.digest), "algorithm":"MPC_ECDSA_SECP256K1",
                "derivationPath":path, "publicKey":public_key.to_string(),
                "signature":{"r":hex::encode(&signature[..32]), "s":hex::encode(&signature[32..])}})
            })
            .collect();
        (
            prepared,
            json!({"status":"COMPLETED", "operation":"RAW", "assetId":"BTC_TEST",
            "source":{"id":"2"}, "signedMessages":messages}),
            json!({"publicKeyInfo":{"derivationPath":path}}),
        )
    }

    #[test]
    fn accepts_single_input_and_reordered_multi_input_raw_responses() {
        for count in [1, 2] {
            let (prepared, mut raw, binding) = fixture(count);
            let original = finalize_raw(&prepared, &raw, &binding).unwrap();
            raw["signedMessages"].as_array_mut().unwrap().reverse();
            let reordered = finalize_raw(&prepared, &raw, &binding).unwrap();
            assert_eq!(reordered, original);
            assert!(
                reordered
                    .inputs
                    .iter()
                    .all(|input| input.final_script_witness.is_some())
            );
        }
    }

    #[test]
    fn rejects_missing_duplicate_and_unrequested_digests() {
        let (prepared, raw, binding) = fixture(2);
        let mut missing = raw.clone();
        missing["signedMessages"].as_array_mut().unwrap().pop();
        assert!(finalize_raw(&prepared, &missing, &binding).is_err());
        let mut duplicate = raw.clone();
        duplicate["signedMessages"][1] = duplicate["signedMessages"][0].clone();
        assert!(finalize_raw(&prepared, &duplicate, &binding).is_err());
        let mut unknown = raw;
        unknown["signedMessages"][1]["content"] = json!("00".repeat(32));
        assert!(finalize_raw(&prepared, &unknown, &binding).is_err());
    }

    #[test]
    fn reordering_cannot_bypass_signature_key_path_or_provider_checks() {
        let (prepared, mut raw, binding) = fixture(2);
        raw["signedMessages"].as_array_mut().unwrap().reverse();
        let mut altered = raw.clone();
        altered["signedMessages"][0]["signature"] =
            altered["signedMessages"][1]["signature"].clone();
        assert!(finalize_raw(&prepared, &altered, &binding).is_err());
        for (field, value) in [
            ("algorithm", json!("MPC_EDDSA_ED25519")),
            ("derivationPath", json!([0, 1])),
            ("publicKey", json!("03".repeat(33))),
        ] {
            let mut altered = raw.clone();
            altered["signedMessages"][0][field] = value;
            assert!(finalize_raw(&prepared, &altered, &binding).is_err());
        }
        raw["source"]["id"] = json!("3");
        assert!(finalize_raw(&prepared, &raw, &binding).is_err());
    }
}

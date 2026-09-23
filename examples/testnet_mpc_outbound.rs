//! Local operator tool for one real 5-NIA Sandbox return transfer. No secrets,
//! provider calls, or public HTTP spending endpoint. JSON stdin / JSON stdout.
use std::{collections::HashMap, fs, io::Read, path::Path, str::FromStr};

use anyhow::{Context, Result, ensure};
use rgb_lib::{
    AssetSchema, Assignment, BitcoinNetwork,
    bitcoin::{
        Address, CompressedPublicKey, Network, OutPoint, Psbt, Transaction, consensus::deserialize,
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
const PREV_TX: &str = "9aba3328ca709009ae6633cb13ef8e9bf1e5074ec3163735a6bfbe3557226590";
fn string<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str().with_context(|| format!("Missing {k}"))
}

fn invoice(v: &Value) -> Result<rgb_lib::wallet::InvoiceData> {
    let data = Invoice::new(string(v, "invoice")?.into())?.invoice_data();
    ensure!(
        data.network == BitcoinNetwork::Testnet
            && data.assignment == Assignment::Fungible(5)
            && data.asset_id.as_deref() == Some(ASSET),
        "Wrong invoice network/asset/amount"
    );
    ensure!(
        rgb_lib::utils::script_buf_from_recipient_id(data.recipient_id.clone())?.is_none(),
        "Blinded invoice required"
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
    let previous: Transaction = deserialize(&hex::decode(string(v, "previousTransactionHex")?)?)?;
    ensure!(
        previous.compute_txid().to_string() == PREV_TX,
        "Wrong previous transaction"
    );
    let prevout = previous
        .output
        .get(1)
        .context("Missing previous output")?
        .clone();
    ensure!(
        prevout.value.to_sat() == 1000 && prevout.script_pubkey == address.script_pubkey(),
        "Wrong prevout"
    );
    let psbt = Psbt::from_str(string(v, "psbt")?)?;
    ensure!(psbt.unsigned_tx.output.len() == 2, "Unexpected outputs");
    let commitment = &psbt.unsigned_tx.output[0];
    let change = &psbt.unsigned_tx.output[1];
    ensure!(
        commitment.value.to_sat() == 0
            && commitment.script_pubkey.is_op_return()
            && commitment.script_pubkey.len() == 34,
        "Expected RGB OP_RETURN commitment"
    );
    ensure!(
        change.script_pubkey == address.script_pubkey() && change.value.to_sat() >= 546,
        "Wrong colored change"
    );
    ensure!(
        !psbt.proprietary.is_empty() || psbt.inputs.iter().any(|i| !i.proprietary.is_empty()),
        "Missing RGB metadata"
    );
    PreparedP2wpkh::prepare(
        psbt,
        &[VerifiedInput {
            outpoint: OutPoint::from_str(&format!("{PREV_TX}:1"))?,
            prevout,
            public_key,
            signing_key_id: "sandbox:2:BTC_TEST:0:0".into(),
        }],
        400,
    )
    .map_err(Into::into)
}

fn inspect(v: &Value) -> Result<Value> {
    let prepared = prepare(v)?;
    let plan = &prepared.signing_plan()[0];
    let mut result = json!({"purpose":"testnet3-5-nia-return", "content":hex::encode(plan.digest),
        "assetId":"BTC_TEST", "vaultId":"2", "publicKey":plan.public_key.to_string(),
        "feeSat":prepared.fee_sat(), "txid":Psbt::from_str(string(v,"psbt")?)?.unsigned_tx.compute_txid().to_string()});
    if let Some(raw) = v.get("rawResult") {
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
        ensure!(messages.len() == 1, "One signature required");
        let message = &messages[0];
        ensure!(
            message["algorithm"] == "MPC_ECDSA_SECP256K1"
                && message["content"] == result["content"]
                && message["derivationPath"] == v["binding"]["publicKeyInfo"]["derivationPath"],
            "Wrong signed message/path"
        );
        let mut key = hex::decode(string(message, "publicKey")?)?;
        if key.len() == 64 {
            key.insert(0, 4);
        }
        ensure!(
            PublicKey::from_slice(&key)? == plan.public_key.0,
            "Wrong signing key"
        );
        let r = hex::decode(string(&message["signature"], "r")?)?;
        let s = hex::decode(string(&message["signature"], "s")?)?;
        ensure!(r.len() == 32 && s.len() == 32, "Invalid signature scalars");
        let signed = prepared.finalize(&[InputSignature {
            input_index: 0,
            compact: [r, s].concat(),
        }])?;
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
            supported_schemas: vec![AssetSchema::Nia],
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
        eth_rpc_url: None,
    })?;
    match string(v, "command")? {
        "status" => {
            wallet.refresh(online, None, vec![], false)?;
            Ok(json!({"assets":wallet.list_assets(vec![AssetSchema::Nia])?,
                "transfers":wallet.list_transfers(AssetFilter::AnyOrNone,None)?,
                "unspents":wallet.list_unspents(Some(online),false,false)?}))
        }
        "begin" => {
            let data = invoice(v)?;
            let expiry = data
                .expiration_timestamp
                .context("Invoice expiry required")?;
            let recipient = Recipient {
                recipient_id: data.recipient_id,
                witness_data: None,
                assignment: Assignment::Fungible(5),
                transport_endpoints: data.transport_endpoints,
            };
            Ok(serde_json::to_value(wallet.send_begin(
                online,
                HashMap::from([(ASSET.into(), vec![recipient])]),
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
    fn invoice_requires_exact_iris_endpoint_and_preserves_nonce() {
        let recipient = "tb3:utxob:7TjnbTy5-H~OndMD-98vlg7F-Fp1VnAt-8J5Ju_P-F~X03DP-kVVUC";
        let prefix = format!(
            "{ASSET}/~/aM/{recipient}?assignment_name=assetOwner&expiry=2000000000&endpoints="
        );
        let endpoint = "rpcs://proxy.iriswallet.com/0.2/json-rpc";
        for transport in [
            endpoint.to_string(),
            format!("{endpoint}?rid_nonce%3D7072af5a8d855853cc825408692bb869"),
        ] {
            let data = invoice(&json!({"invoice": format!("{prefix}{transport}")})).unwrap();
            assert_eq!(
                data.transport_endpoints,
                vec![transport.replace("%3D", "=")]
            );
        }
        let error =
            invoice(&json!({"invoice": format!("{prefix}{endpoint}/unexpected")})).unwrap_err();
        assert_eq!(error.to_string(), "Wrong transport");
    }
}

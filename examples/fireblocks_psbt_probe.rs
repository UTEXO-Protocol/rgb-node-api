//! Operator-only cryptographic probe. The previous output is synthetic and does
//! not exist on a chain. Never use these fixture prevouts for a real spend.
//! Usage: prepare BINDING | finalize BINDING RAW_RESULT | registration BINDING UUID
use std::{env, fs, str::FromStr};

use anyhow::{Context, Result, ensure};
use rgb_lib::bitcoin::blockdata::constants::genesis_block;
use rgb_lib::bitcoin::{
    Address, Amount, CompressedPublicKey, Network, OutPoint, Psbt, ScriptBuf, Sequence,
    Transaction, TxIn, TxOut, Txid, Witness, absolute, hashes::Hash, psbt::raw::ProprietaryKey,
    secp256k1::PublicKey, transaction,
};
use rgb_node_api::signing::{InputSignature, PreparedP2wpkh, VerifiedInput};
use serde_json::{Value, json};

fn string<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    value[field]
        .as_str()
        .with_context(|| format!("Missing {field}"))
}

fn network(binding: &Value) -> Result<Network> {
    match string(binding, "assetId")? {
        "BTC_TEST" => Ok(Network::Testnet),
        "BTC_TEST4" => Ok(Network::Testnet4),
        _ => anyhow::bail!("Unsupported Bitcoin test asset"),
    }
}

fn fixture(binding: &Value) -> Result<(PreparedP2wpkh, CompressedPublicKey)> {
    ensure!(
        binding["environment"] == "sandbox",
        "Sandbox binding required"
    );
    let asset = string(binding, "assetId")?;
    let network = network(binding)?;
    let key_info = &binding["publicKeyInfo"];
    ensure!(
        key_info["algorithm"] == "MPC_ECDSA_SECP256K1",
        "Expected ECDSA key"
    );
    let public_key = CompressedPublicKey::from_str(string(key_info, "publicKey")?)?;
    let address =
        Address::from_str(string(&binding["wallet"], "address")?)?.require_network(network)?;
    ensure!(
        address.script_pubkey() == ScriptBuf::new_p2wpkh(&public_key.wpubkey_hash()),
        "Provider address/key mismatch"
    );
    let previous = VerifiedInput {
        outpoint: OutPoint {
            txid: Txid::all_zeros(),
            vout: 0,
        },
        prevout: TxOut {
            value: Amount::from_sat(50_000),
            script_pubkey: address.script_pubkey(),
        },
        public_key,
        signing_key_id: format!("sandbox:{}:{asset}:0:0", string(&binding["vault"], "id")?),
    };
    let transaction = Transaction {
        version: transaction::Version::TWO,
        lock_time: absolute::LockTime::ZERO,
        input: vec![TxIn {
            previous_output: previous.outpoint,
            script_sig: ScriptBuf::new(),
            sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
            witness: Witness::new(),
        }],
        output: vec![TxOut {
            value: Amount::from_sat(49_000),
            script_pubkey: address.script_pubkey(),
        }],
    };
    let mut psbt = Psbt::from_unsigned_tx(transaction)?;
    psbt.inputs[0].witness_utxo = Some(previous.prevout.clone());
    // A preservation check, not a real RGB consignment or commitment.
    psbt.proprietary.insert(
        ProprietaryKey {
            prefix: b"UTEXO-POC".to_vec(),
            subtype: 1,
            key: Vec::new(),
        },
        b"synthetic-metadata".to_vec(),
    );
    Ok((
        PreparedP2wpkh::prepare(psbt, &[previous], 1000)?,
        public_key,
    ))
}

fn finalize(binding: &Value, response: &Value) -> Result<Value> {
    let (prepared, public_key) = fixture(binding)?;
    ensure!(
        response["status"] == "COMPLETED",
        "RAW signature not completed"
    );
    ensure!(
        response["operation"] == "RAW" && response["assetId"] == binding["assetId"],
        "Unexpected RAW operation/asset"
    );
    ensure!(
        response["source"]["id"] == binding["vault"]["id"],
        "Wrong source vault"
    );
    let messages = response["signedMessages"]
        .as_array()
        .context("Missing signed messages")?;
    ensure!(messages.len() == 1, "Expected one signature");
    let message = &messages[0];
    ensure!(
        message["algorithm"] == "MPC_ECDSA_SECP256K1",
        "Wrong signature algorithm"
    );
    ensure!(
        message["derivationPath"] == binding["publicKeyInfo"]["derivationPath"],
        "Wrong signing derivation path"
    );
    ensure!(
        message["content"] == hex::encode(prepared.signing_plan()[0].digest),
        "Wrong signed digest"
    );
    let mut key_bytes = hex::decode(string(message, "publicKey")?)?;
    if key_bytes.len() == 64 {
        key_bytes.insert(0, 4);
    }
    ensure!(
        PublicKey::from_slice(&key_bytes)? == public_key.0,
        "Wrong signing public key"
    );
    let r = hex::decode(string(&message["signature"], "r")?)?;
    let s = hex::decode(string(&message["signature"], "s")?)?;
    ensure!(
        r.len() == 32 && s.len() == 32,
        "Invalid signature scalar length"
    );
    let signature = InputSignature {
        input_index: 0,
        compact: [r, s].concat(),
    };
    let signed = prepared.finalize(&[signature])?;
    ensure!(!signed.proprietary.is_empty(), "Fixture metadata was lost");
    Ok(json!({
        "signatureVerified": true, "providerAddressMatchesPublicKey": true,
        "syntheticPrevout": true, "broadcast": false, "nodeAcceptanceTested": false,
        "independentApprovalTested": false, "feeSat": prepared.fee_sat(),
        "psbtHex": hex::encode(signed.serialize()),
        "transactionId": signed.unsigned_tx.compute_txid().to_string(),
        "fireblocksTransactionId": response["id"],
    }))
}

fn main() -> Result<()> {
    let args: Vec<_> = env::args().collect();
    ensure!(
        args.len() >= 3,
        "Usage: prepare BINDING | finalize BINDING RAW_RESULT"
    );
    let binding: Value = serde_json::from_str(&fs::read_to_string(&args[2])?)?;
    let output = match args[1].as_str() {
        "registration" => {
            fixture(&binding)?;
            let wallet_id = uuid::Uuid::parse_str(args.get(3).context("Missing wallet UUID")?)?;
            let network = network(&binding)?;
            let asset = string(&binding, "assetId")?;
            let vault = string(&binding["vault"], "id")?;
            let api_user = string(&binding, "apiUserId")?;
            json!({
                "wallet_id": wallet_id,
                "provider": "fireblocks_vault",
                "provider_environment": format!("sandbox-api.fireblocks.io/api-user/{api_user}"),
                "provider_wallet_ref": format!("vault/{vault}/{asset}"),
                "bitcoin_network": network.to_string(),
                "genesis_hash": genesis_block(network).block_hash().to_string(),
                "addresses": [{
                    "role": "rgb", "script_type": "p2wpkh",
                    "address": binding["wallet"]["address"],
                    "public_key": binding["publicKeyInfo"]["publicKey"],
                    "signing_key_id": format!("sandbox:{vault}:{asset}:0:0"),
                }],
            })
        }
        "prepare" => {
            let (prepared, _) = fixture(&binding)?;
            json!({
                "purpose": "synthetic-unspendable-p2wpkh-probe",
                "assetId": binding["assetId"], "vaultId": binding["vault"]["id"],
                "publicKey": binding["publicKeyInfo"]["publicKey"],
                "content": hex::encode(prepared.signing_plan()[0].digest),
                "feeSat": prepared.fee_sat(), "broadcast": false,
            })
        }
        "finalize" => {
            let response: Value = serde_json::from_str(&fs::read_to_string(
                args.get(3).context("Missing RAW result file")?,
            )?)?;
            finalize(&binding, &response)?
        }
        _ => anyhow::bail!("Unknown command"),
    };
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

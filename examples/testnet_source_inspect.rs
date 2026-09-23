//! Offline, public-key-only checks for Testnet3 source funding, payouts and BFA mint.
//! JSON stdin: headers, unspents, psbt, optionally original_psbt. No network I/O.
use std::{collections::HashMap, io::Read, str::FromStr};

use anyhow::{Context, Result, ensure};
use rgb_lib::bitcoin::{
    Address, Network, NetworkKind, Psbt, ScriptBuf,
    bip32::{ChildNumber, Xpub},
    secp256k1::{Message, Secp256k1, XOnlyPublicKey, schnorr::Signature},
    sighash::{Prevouts, SighashCache, TapSighashType},
};
use rgb_lib::{Assignment, BitcoinNetwork, wallet::Invoice};
use serde_json::{Value, json};

fn invoice(value: &Value) -> Result<Value> {
    if value["poc"] == true {
        ensure!(
            (1..=25).contains(
                &value["amount"]
                    .as_str()
                    .context("Missing amount")?
                    .parse::<u64>()?
            ),
            "POC amount limit"
        );
    }
    let data =
        Invoice::new(value["invoice"].as_str().context("Missing invoice")?.into())?.invoice_data();
    ensure!(
        data.network == BitcoinNetwork::Testnet,
        "Wrong invoice network"
    );
    ensure!(
        data.assignment
            == Assignment::Fungible(if value["poc"] == true {
                value["amount"]
                    .as_str()
                    .context("Missing amount")?
                    .parse::<u64>()?
            } else {
                25
            }),
        "Invoice amount does not match the requested amount"
    );
    if let Some(asset) = &data.asset_id {
        ensure!(value["asset_id"] == *asset, "Wrong invoice asset");
    }
    ensure!(
        !data.transport_endpoints.is_empty(),
        "Missing invoice transport"
    );
    let recipient_address =
        rgb_lib::utils::script_buf_from_recipient_id(data.recipient_id.clone())?
            .map(|script| {
                Address::from_script(&script, Network::Testnet).map(|address| address.to_string())
            })
            .transpose()?;
    let mut decoded = serde_json::to_value(data)?;
    decoded["recipient_address"] = json!(recipient_address);
    Ok(decoded)
}

// DYNAMIC_EMBEDDED_POC: optional empty, source-owned RGB fee inputs. Vault callers
// retain their existing strict allocation check unless explicitly opted in.
fn poc_allocations_match(allocations: &[Value], asset: &Value, allow_empty: bool) -> bool {
    (allow_empty || !allocations.is_empty())
        && allocations.iter().all(|allocation| {
            allocation["asset_id"] == *asset
                && allocation["assignment"]["Fungible"]
                    .as_u64()
                    .is_some_and(|n| n > 0)
                && allocation["settled"] == true
        })
}

fn inspect(value: &Value) -> Result<Value> {
    let payout = value.get("transfer");
    let mint = value.get("mint");
    let funding = value.get("btc_funding");
    ensure!(
        [payout, mint, funding]
            .iter()
            .filter(|v| v.is_some())
            .count()
            <= 1,
        "Ambiguous source operation"
    );
    let rgb = payout.is_some() || mint.is_some();
    // Direct mock mint uses the same pinned invoice/address check as a payout.
    // The historical self-mint has no external recipient and stays strict.
    let destination = payout.or_else(|| mint.filter(|mint| mint.get("invoice").is_some()));
    let recipient_script = if let Some(payout) = destination {
        let decoded = invoice(payout)?;
        let expected = Address::from_str(
            payout["recipient_address"]
                .as_str()
                .context("Missing recipient")?,
        )?
        .require_network(Network::Testnet)?
        .script_pubkey();
        ensure!(
            rgb_lib::utils::script_buf_from_recipient_id(
                decoded["recipient_id"]
                    .as_str()
                    .context("Missing beneficiary")?
                    .into()
            )? == Some(expected.clone()),
            "Invoice and registered address disagree"
        );
        Some(expected)
    } else if let Some(funding) = funding {
        ensure!(
            funding["amount_sat"] == 5000,
            "Funding amount must be 5000 sat"
        );
        Some(
            Address::from_str(
                funding["recipient_address"]
                    .as_str()
                    .context("Missing recipient")?,
            )?
            .require_network(Network::Testnet)?
            .script_pubkey(),
        )
    } else {
        None
    };
    let secp = Secp256k1::verification_only();
    let mut scripts: HashMap<ScriptBuf, &str> = HashMap::new();
    for role in ["van", "col"] {
        let xpub = Xpub::from_str(
            value["headers"][format!("xpub-{role}")]
                .as_str()
                .context("Missing public registration")?,
        )?;
        ensure!(xpub.network == NetworkKind::Test, "Test keys required");
        // Same /0/index Taproot derivation as rgb-lib's default descriptors.
        // Bounded to this fresh POC wallet; refuse later unknown derivations.
        for index in 0..100 {
            let key = xpub.derive_pub(
                &secp,
                &[
                    ChildNumber::from_normal_idx(0)?,
                    ChildNumber::from_normal_idx(index)?,
                ],
            )?;
            let script = Address::p2tr(
                &secp,
                key.public_key.x_only_public_key().0,
                None,
                Network::Testnet,
            )
            .script_pubkey();
            ensure!(scripts.insert(script, role).is_none(), "Overlapping keys");
        }
    }
    let psbt = Psbt::from_str(value["psbt"].as_str().context("Missing PSBT")?)?;
    if let Some(script) = &recipient_script {
        ensure!(
            funding.is_none() || !scripts.contains_key(script),
            "Funding recipient must be external"
        );
    }
    let mut input_total = 0u64;
    let mut prevouts = Vec::new();
    let mut rgb_input_count = 0;
    let unspents = value["unspents"].as_array().context("Missing unspents")?;
    for (input, metadata) in psbt.unsigned_tx.input.iter().zip(&psbt.inputs) {
        let previous = metadata.witness_utxo.as_ref().context("Missing prevout")?;
        let role = scripts.get(&previous.script_pubkey);
        ensure!(
            role == Some(&"van") || (rgb && role == Some(&"col")),
            "Input is not source BTC"
        );
        ensure!(
            unspents.iter().any(|u| {
                ((mint.is_none() && funding.is_none())
                    || (u["utxo"]["exists"] == true && u["pending_blinded"] == 0))
                    && u["utxo"]["outpoint"]["txid"] == input.previous_output.txid.to_string()
                    && u["utxo"]["outpoint"]["vout"] == input.previous_output.vout
                    && u["utxo"]["btc_amount"] == previous.value.to_sat()
                    && if role == Some(&"col") {
                        u["utxo"]["colorable"] == true
                            && u["rgb_allocations"].as_array().is_some_and(|a| {
                                if let Some(mint) = mint {
                                    a.iter().all(|allocation| {
                                        allocation["asset_id"] == mint["asset_id"]
                                            && allocation["settled"] == true
                                            && allocation["assignment"] == "BridgeRight"
                                    })
                                } else if value["transfer"]["poc"] == true {
                                    poc_allocations_match(
                                        a,
                                        &value["transfer"]["asset_id"],
                                        value["transfer"]["allow_empty_rgb_fee_inputs"] == true,
                                    )
                                } else {
                                    a.len() == 1
                                        && a[0]["asset_id"] == value["transfer"]["asset_id"]
                                        && a[0]["assignment"] == json!({"Fungible":200})
                                        && a[0]["settled"] == true
                                }
                            })
                    } else {
                        u["utxo"]["colorable"] == false
                            && u["rgb_allocations"].as_array().is_some_and(Vec::is_empty)
                    }
            }),
            "Input disagrees with synced unspent snapshot"
        );
        // Empty fee inputs must never satisfy the requirement for an RGB input.
        if role == Some(&"col")
            && unspents.iter().any(|u| {
                u["utxo"]["outpoint"]["txid"] == input.previous_output.txid.to_string()
                    && u["utxo"]["outpoint"]["vout"] == input.previous_output.vout
                    && u["rgb_allocations"]
                        .as_array()
                        .is_some_and(|a| !a.is_empty())
            })
        {
            rgb_input_count += 1;
        }
        input_total = input_total
            .checked_add(previous.value.to_sat())
            .context("Input overflow")?;
        prevouts.push(previous.clone());
    }
    let mut colored = Vec::new();
    let mut output_total = 0u64;
    let mut vanilla_count = 0;
    let mut recipient_count = 0;
    let mut commitment_count = 0;
    for (vout, output) in psbt.unsigned_tx.output.iter().enumerate() {
        if recipient_script.as_ref() == Some(&output.script_pubkey) {
            ensure!(
                output.value.to_sat() == if funding.is_some() { 5000 } else { 1000 },
                "Wrong recipient BTC amount"
            );
            recipient_count += 1;
        } else if rgb && output.script_pubkey.is_op_return() {
            ensure!(
                output.value.to_sat() == 0 && output.script_pubkey.len() <= 83,
                "Unexpected commitment output"
            );
            commitment_count += 1;
        } else {
            match scripts.get(&output.script_pubkey) {
                Some(&"col") => {
                    ensure!(
                        rgb || output.value.to_sat() == 5000,
                        "Wrong allocation UTXO size"
                    );
                    colored.push(json!({"vout":vout, "btcAmountSat":output.value.to_sat()}));
                }
                Some(&"van") => vanilla_count += 1,
                _ => anyhow::bail!("Output does not belong to source"),
            }
        }
        output_total = output_total
            .checked_add(output.value.to_sat())
            .context("Output overflow")?;
    }
    if mint.is_some() {
        ensure!(
            rgb_input_count >= 1
                && recipient_count == usize::from(recipient_script.is_some())
                && commitment_count == 1
                && !colored.is_empty(),
            "Mint must spend its own bridge right, retain a source output and fund only its pinned recipient"
        );
    } else if payout.is_some() {
        ensure!(
            (if value["transfer"]["poc"] == true {
                rgb_input_count >= 1
            } else {
                rgb_input_count == 1
            }) && recipient_count == 1
                && commitment_count == 1,
            "Expected one 200-NIA input, one recipient and one RGB commitment"
        );
    } else if funding.is_some() {
        ensure!(
            recipient_count == 1
                && vanilla_count == 1
                && colored.is_empty()
                && commitment_count == 0
                && rgb_input_count == 0,
            "Funding requires one recipient, own vanilla change and no RGB inputs/outputs"
        );
    } else {
        let allocation_count = match value.get("allocation_count") {
            None => 5,
            Some(count) => count.as_u64().context("Invalid allocation output count")?,
        };
        ensure!(
            (1..=5).contains(&allocation_count)
                && colored.len() as u64 == allocation_count
                && vanilla_count == 1,
            "Expected the saved allocation output count and own change"
        );
    }
    let fee = input_total
        .checked_sub(output_total)
        .context("Outputs exceed inputs")?;
    ensure!(
        (1..=if funding.is_some() { 1000 } else { 2000 }).contains(&fee),
        "Fee exceeds the operation's POC limit"
    );
    let mut verified_signatures = 0;
    if let Some(original) = value["original_psbt"].as_str() {
        let original = Psbt::from_str(original)?;
        ensure!(
            original.unsigned_tx == psbt.unsigned_tx,
            "Signer changed transaction"
        );
        ensure!(
            original.proprietary == psbt.proprietary && original.unknown == psbt.unknown,
            "Signer changed global PSBT metadata"
        );
        for (before, after) in original.outputs.iter().zip(&psbt.outputs) {
            ensure!(
                before.proprietary == after.proprietary && before.unknown == after.unknown,
                "Signer changed output PSBT metadata"
            );
        }
        let mut cache = SighashCache::new(&psbt.unsigned_tx);
        for (index, input) in psbt.inputs.iter().enumerate() {
            let witness = input
                .final_script_witness
                .as_ref()
                .context("Unfinalized input")?;
            ensure!(witness.len() == 1, "Expected Taproot key spend");
            let bytes = witness.iter().next().context("Missing signature")?;
            ensure!(bytes.len() == 64, "Expected default sighash");
            let signature = Signature::from_slice(bytes)?;
            let digest = cache.taproot_key_spend_signature_hash(
                index,
                &Prevouts::All(&prevouts),
                TapSighashType::Default,
            )?;
            let public_key =
                XOnlyPublicKey::from_slice(&prevouts[index].script_pubkey.as_bytes()[2..])?;
            secp.verify_schnorr(&signature, &Message::from(digest), &public_key)?;
            verified_signatures += 1;
        }
    }
    Ok(
        json!({"txid":psbt.unsigned_tx.compute_txid().to_string(), "feeSat":fee,
        "inputSat":input_total,"outputSat":output_total,"allocationOutputs":colored,
        "verifiedSignatures":verified_signatures,"recipientOutputs":recipient_count,"rgbInputs":rgb_input_count}),
    )
}

fn main() -> Result<()> {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let value: Value = serde_json::from_str(&input)?;
    println!(
        "{}",
        if value["mode"] == "invoice" {
            invoice(&value)?
        } else {
            inspect(&value)?
        }
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rgb_lib::bitcoin::{
        Amount, OutPoint, Transaction, TxIn, TxOut, absolute, bip32::Xpriv, transaction,
    };

    fn fixture() -> Value {
        let secp = Secp256k1::new();
        let vanilla = Xpub::from_priv(
            &secp,
            &Xpriv::new_master(Network::Testnet, &[7; 32]).unwrap(),
        );
        let colored = Xpub::from_priv(
            &secp,
            &Xpriv::new_master(Network::Testnet, &[8; 32]).unwrap(),
        );
        let script = |xpub: Xpub, index| {
            let key = xpub
                .derive_pub(
                    &secp,
                    &[
                        ChildNumber::from_normal_idx(0).unwrap(),
                        ChildNumber::from_normal_idx(index).unwrap(),
                    ],
                )
                .unwrap();
            Address::p2tr(
                &secp,
                key.public_key.x_only_public_key().0,
                None,
                Network::Testnet,
            )
            .script_pubkey()
        };
        let mut outputs: Vec<_> = (0..5)
            .map(|i| TxOut {
                value: Amount::from_sat(5000),
                script_pubkey: script(colored, i),
            })
            .collect();
        outputs.push(TxOut {
            value: Amount::from_sat(74_000),
            script_pubkey: script(vanilla, 1),
        });
        let mut psbt = Psbt::from_unsigned_tx(Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: vec![TxIn {
                previous_output: OutPoint::null(),
                ..Default::default()
            }],
            output: outputs,
        })
        .unwrap();
        psbt.inputs[0].witness_utxo = Some(TxOut {
            value: Amount::from_sat(100_000),
            script_pubkey: script(vanilla, 0),
        });
        json!({"headers":{"xpub-van":vanilla.to_string(), "xpub-col":colored.to_string()},
            "psbt":psbt.to_string(), "unspents":[{"utxo":{"outpoint":{"txid":OutPoint::null().txid.to_string(),"vout":u32::MAX},"btc_amount":100000,"colorable":false},"rgb_allocations":[]}]})
    }

    #[test]
    fn btc_funding_pins_recipient_amount_and_preserves_rgb_inputs() {
        let mut value = fixture();
        let mut psbt = Psbt::from_str(value["psbt"].as_str().unwrap()).unwrap();
        let secp = Secp256k1::new();
        let external = Xpub::from_priv(
            &secp,
            &Xpriv::new_master(Network::Testnet, &[9; 32]).unwrap(),
        );
        let address = Address::p2tr(
            &secp,
            external.public_key.x_only_public_key().0,
            None,
            Network::Testnet,
        );
        let mut change = psbt.unsigned_tx.output.last().unwrap().clone();
        change.value = Amount::from_sat(94_000);
        psbt.unsigned_tx.output = vec![
            TxOut {
                value: Amount::from_sat(5000),
                script_pubkey: address.script_pubkey(),
            },
            change,
        ];
        psbt.outputs = vec![Default::default(); 2];
        value["psbt"] = json!(psbt.to_string());
        value["btc_funding"] =
            json!({"recipient_address": address.to_string(), "amount_sat": 5000});
        value["unspents"][0]["utxo"]["exists"] = json!(true);
        value["unspents"][0]["pending_blinded"] = json!(0);
        let result = inspect(&value).unwrap();
        assert_eq!(result["feeSat"], 1000);
        assert_eq!(result["recipientOutputs"], 1);
        assert_eq!(result["rgbInputs"], 0);
        for (field, replacement) in [
            ("amount_sat", json!(5001)),
            ("recipient_address", json!("bc1invalid")),
        ] {
            let mut wrong = value.clone();
            wrong["btc_funding"][field] = replacement;
            assert!(inspect(&wrong).is_err());
        }
        let mut wrong = value.clone();
        wrong["unspents"][0]["rgb_allocations"] = json!([{"assignment":"BridgeRight"}]);
        assert!(inspect(&wrong).is_err());
        let mut wrong = value.clone();
        wrong["mint"] = json!({});
        assert!(inspect(&wrong).is_err());
        for excess_fee in [false, true] {
            let mut wrong_psbt = psbt.clone();
            if excess_fee {
                wrong_psbt.unsigned_tx.output[1].value -= Amount::ONE_SAT;
            } else {
                wrong_psbt.unsigned_tx.output[0].script_pubkey =
                    wrong_psbt.unsigned_tx.output[1].script_pubkey.clone();
            }
            let mut wrong = value.clone();
            wrong["psbt"] = json!(wrong_psbt.to_string());
            assert!(inspect(&wrong).is_err());
        }
    }

    #[test]
    fn source_preparation_pins_a_bounded_output_count_without_changing_legacy_defaults() {
        let mut value = fixture();
        let mut psbt = Psbt::from_str(value["psbt"].as_str().unwrap()).unwrap();
        psbt.unsigned_tx.output.drain(1..5);
        psbt.outputs.drain(1..5);
        psbt.unsigned_tx.output.last_mut().unwrap().value += Amount::from_sat(20000);
        value["psbt"] = json!(psbt.to_string());
        assert!(inspect(&value).is_err());
        value["allocation_count"] = json!(1);
        assert!(inspect(&value).is_ok());
        for count in [0, 2, 6] {
            value["allocation_count"] = json!(count);
            assert!(inspect(&value).is_err());
        }
    }

    #[test]
    fn mock_mint_spends_only_its_bridge_right_and_keeps_all_btc_at_the_source() {
        let mut value = fixture();
        let mut psbt = Psbt::from_str(value["psbt"].as_str().unwrap()).unwrap();
        let mut change = psbt.unsigned_tx.output[0].clone();
        psbt.inputs[0].witness_utxo = Some(change.clone());
        change.value = Amount::from_sat(4500);
        psbt.unsigned_tx.output = vec![
            TxOut {
                value: Amount::ZERO,
                script_pubkey: ScriptBuf::new_op_return([1u8; 32]),
            },
            change,
        ];
        psbt.outputs = vec![Default::default(); 2];
        value["psbt"] = json!(psbt.to_string());
        value["mint"] = json!({"asset_id": "bfa-fixture"});
        value["unspents"][0]["utxo"]["colorable"] = json!(true);
        value["unspents"][0]["utxo"]["exists"] = json!(true);
        value["unspents"][0]["pending_blinded"] = json!(0);
        value["unspents"][0]["utxo"]["btc_amount"] = json!(5000);
        value["unspents"][0]["rgb_allocations"] =
            json!([{"asset_id":"bfa-fixture", "assignment":"BridgeRight", "settled":true}]);
        assert_eq!(inspect(&value).unwrap()["feeSat"], 500);
        let mut altered = value.clone();
        altered["unspents"][0]["pending_blinded"] = json!(1);
        assert!(inspect(&altered).is_err());
        let mut altered = value.clone();
        altered["unspents"][0]["rgb_allocations"][0]["asset_id"] = json!("nia-fixture");
        assert!(inspect(&altered).is_err());
        let mut altered = value.clone();
        altered["unspents"][0]["rgb_allocations"][0]["assignment"] = json!({"Fungible": 1000});
        assert!(inspect(&altered).is_err());
        psbt.unsigned_tx.output[1].script_pubkey = ScriptBuf::new();
        value["psbt"] = json!(psbt.to_string());
        assert!(inspect(&value).is_err());
    }

    #[test]
    fn direct_mint_allows_only_the_pinned_witness_and_keeps_the_bridge_right_input() {
        let mut value = fixture();
        let mut psbt = Psbt::from_str(value["psbt"].as_str().unwrap()).unwrap();
        let secp = Secp256k1::new();
        let key = Xpriv::new_master(Network::Testnet, &[9; 32])
            .unwrap()
            .to_priv();
        let address = Address::p2tr(
            &secp,
            key.public_key(&secp).inner.x_only_public_key().0,
            None,
            Network::Testnet,
        );
        let recipient = rgb_lib::utils::recipient_id_from_script_buf(
            address.script_pubkey(),
            BitcoinNetwork::Testnet,
        );
        let invoice = format!(
            "rgb:~/~/de/{recipient}?assignment_name=assetOwner&expiry=2000000000&endpoints=rpcs://example.org/0.2/json-rpc?rid_nonce%3D7072af5a8d855853cc825408692bb869"
        );
        let mut change = psbt.unsigned_tx.output[0].clone();
        psbt.inputs[0].witness_utxo = Some(change.clone());
        change.value = Amount::from_sat(3500);
        psbt.unsigned_tx.output = vec![
            TxOut {
                value: Amount::ZERO,
                script_pubkey: ScriptBuf::new_op_return([1u8; 32]),
            },
            TxOut {
                value: Amount::from_sat(1000),
                script_pubkey: address.script_pubkey(),
            },
            change,
        ];
        psbt.outputs = vec![Default::default(); 3];
        value["psbt"] = json!(psbt.to_string());
        value["mint"] = json!({"poc":true, "asset_id":"bfa-fixture", "amount":"25", "invoice":invoice, "recipient_address":address.to_string()});
        value["unspents"][0]["utxo"]["colorable"] = json!(true);
        value["unspents"][0]["utxo"]["exists"] = json!(true);
        value["unspents"][0]["pending_blinded"] = json!(0);
        value["unspents"][0]["utxo"]["btc_amount"] = json!(5000);
        value["unspents"][0]["rgb_allocations"] =
            json!([{"asset_id":"bfa-fixture", "assignment":"BridgeRight", "settled":true}]);
        let checked = inspect(&value).unwrap();
        assert_eq!(checked["feeSat"], 500);
        assert_eq!(checked["recipientOutputs"], 1);
        let mut altered = value.clone();
        altered["mint"]["amount"] = json!("24");
        assert!(inspect(&altered).is_err());
        let mut altered = value.clone();
        altered["mint"]["recipient_address"] = json!(
            Address::from_script(&psbt.unsigned_tx.output[2].script_pubkey, Network::Testnet)
                .unwrap()
                .to_string()
        );
        assert!(inspect(&altered).is_err());
        let mut altered = value.clone();
        altered["unspents"][0]["rgb_allocations"][0]["assignment"] = json!({"Fungible":25});
        assert!(inspect(&altered).is_err());
        for amount in [999, 1001] {
            let mut altered = psbt.clone();
            altered.unsigned_tx.output[1].value = Amount::from_sat(amount);
            let mut input = value.clone();
            input["psbt"] = json!(altered.to_string());
            assert!(inspect(&input).is_err());
        }
        let mut altered = psbt.clone();
        altered.unsigned_tx.output[1].script_pubkey = ScriptBuf::new();
        let mut input = value.clone();
        input["psbt"] = json!(altered.to_string());
        assert!(inspect(&input).is_err());
        let mut altered = psbt.clone();
        altered.unsigned_tx.output[2] = altered.unsigned_tx.output[1].clone();
        let mut input = value.clone();
        input["psbt"] = json!(altered.to_string());
        assert!(inspect(&input).is_err());
    }

    #[test]
    fn rejects_external_destination_and_high_fee() {
        let mut value = fixture();
        let valid = inspect(&value).unwrap();
        assert_eq!(valid["allocationOutputs"].as_array().unwrap().len(), 5);
        let mut psbt = Psbt::from_str(value["psbt"].as_str().unwrap()).unwrap();
        psbt.unsigned_tx.output[5].value = Amount::from_sat(70_000);
        value["psbt"] = json!(psbt.to_string());
        assert!(inspect(&value).is_err());
        psbt.unsigned_tx.output[5].value = Amount::from_sat(74_000);
        psbt.unsigned_tx.output[5].script_pubkey = ScriptBuf::new();
        value["psbt"] = json!(psbt.to_string());
        assert!(inspect(&value).is_err());
    }

    #[test]
    fn rejects_wrong_prevout_value_and_missing_signature() {
        let mut value = fixture();
        value["unspents"][0]["utxo"]["btc_amount"] = json!(99_000);
        assert!(inspect(&value).is_err());
        let mut value = fixture();
        value["original_psbt"] = value["psbt"].clone();
        assert!(inspect(&value).is_err());
    }

    #[test]
    fn payout_checks_invoice_recipient_and_rgb_input() {
        let mut value = fixture();
        let mut psbt = Psbt::from_str(value["psbt"].as_str().unwrap()).unwrap();
        let secp = Secp256k1::new();
        let key = Xpriv::new_master(Network::Testnet, &[9; 32])
            .unwrap()
            .to_priv();
        let address = Address::p2tr(
            &secp,
            key.public_key(&secp).inner.x_only_public_key().0,
            None,
            Network::Testnet,
        );
        let recipient = rgb_lib::utils::recipient_id_from_script_buf(
            address.script_pubkey(),
            BitcoinNetwork::Testnet,
        );
        let invoice = format!(
            "rgb:~/~/de/{recipient}?assignment_name=assetOwner&expiry=2000000000&endpoints=rpcs://example.org/0.2/json-rpc?rid_nonce%3D7072af5a8d855853cc825408692bb869"
        );
        value["transfer"] = json!({"invoice":invoice, "asset_id":"fixture", "recipient_address":address.to_string()});
        let decoded = super::invoice(&value["transfer"]).unwrap();
        assert_eq!(decoded["recipient_address"], address.to_string());
        assert_ne!(decoded["proxy_recipient_id"], decoded["recipient_id"]);
        assert!(
            decoded["transport_endpoints"][0]
                .as_str()
                .unwrap()
                .contains("rid_nonce")
        );
        psbt.inputs[0].witness_utxo = Some(psbt.unsigned_tx.output[0].clone());
        let mut change = psbt.unsigned_tx.output[5].clone();
        change.value = Amount::from_sat(3500);
        psbt.unsigned_tx.output = vec![
            TxOut {
                value: Amount::ZERO,
                script_pubkey: ScriptBuf::new_op_return([0u8; 32]),
            },
            TxOut {
                value: Amount::from_sat(1000),
                script_pubkey: address.script_pubkey(),
            },
            change,
        ];
        psbt.outputs = vec![Default::default(); 3];
        value["unspents"][0]["utxo"]["colorable"] = json!(true);
        value["unspents"][0]["utxo"]["btc_amount"] = json!(5000);
        value["unspents"][0]["rgb_allocations"] =
            json!([{"asset_id":"fixture","assignment":{"Fungible":200},"settled":true}]);
        value["psbt"] = json!(psbt.to_string());
        assert_eq!(inspect(&value).unwrap()["feeSat"], 500);
        // DYNAMIC_EMBEDDED_POC: a source-owned empty RGB output can fund fees,
        // but only with opt-in and alongside an actual RGB asset input.
        let mut funded = value.clone();
        funded["transfer"]["poc"] = json!(true);
        funded["transfer"]["amount"] = json!("25");
        let mut funded_psbt = psbt.clone();
        let mut fee_input = funded_psbt.unsigned_tx.input[0].clone();
        fee_input.previous_output.vout -= 1;
        funded_psbt.unsigned_tx.input.push(fee_input);
        funded_psbt.inputs.push(funded_psbt.inputs[0].clone());
        funded_psbt.unsigned_tx.output[2].value = Amount::from_sat(8500);
        let mut fee_utxo = funded["unspents"][0].clone();
        fee_utxo["utxo"]["outpoint"]["vout"] = json!(u32::MAX - 1);
        fee_utxo["rgb_allocations"] = json!([]);
        funded["unspents"].as_array_mut().unwrap().push(fee_utxo);
        funded["psbt"] = json!(funded_psbt.to_string());
        assert!(
            inspect(&funded).is_err(),
            "Vault/default check stays strict"
        );
        funded["transfer"]["allow_empty_rgb_fee_inputs"] = json!(true);
        assert_eq!(inspect(&funded).unwrap()["rgbInputs"], 1);
        funded["unspents"][0]["rgb_allocations"] = json!([]);
        assert!(
            inspect(&funded).is_err(),
            "Fee inputs cannot replace the RGB input"
        );
        value["transfer"]["asset_id"] = json!("other");
        assert!(inspect(&value).is_err());
        value["transfer"]["asset_id"] = json!("fixture");
        psbt.unsigned_tx.output[1].value = Amount::from_sat(1001);
        value["psbt"] = json!(psbt.to_string());
        assert!(inspect(&value).is_err());
        psbt.unsigned_tx.output[1].value = Amount::from_sat(1000);
        psbt.unsigned_tx.output[1].script_pubkey = psbt.unsigned_tx.output[2].script_pubkey.clone();
        value["psbt"] = json!(psbt.to_string());
        assert!(inspect(&value).is_err());
        value["transfer"]["recipient_address"] = json!(
            Address::from_script(&psbt.unsigned_tx.output[2].script_pubkey, Network::Testnet)
                .unwrap()
                .to_string()
        );
        assert!(inspect(&value).is_err());
    }
}

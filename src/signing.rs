//! Offline P2WPKH boundary for Vault RAW and Embedded signed-PSBT responses.
//!
//! No key storage, provider API, authorization or broadcast lives here. The
//! caller must authenticate the operation and obtain prevouts from its trusted
//! Bitcoin node and keys from its verified provider registry. Browser-supplied
//! prevouts or key references are not trusted inputs to this module.
use std::collections::HashSet;

use rgb_lib::bitcoin::{
    CompressedPublicKey, EcdsaSighashType, OutPoint, Psbt, ScriptBuf, TxOut, Witness, ecdsa,
    hashes::Hash,
    secp256k1::{Message, Secp256k1, ecdsa::Signature},
    sighash::SighashCache,
};

/// A node-verified prevout and its independently verified provider key binding.
#[derive(Clone)]
pub struct VerifiedInput {
    pub outpoint: OutPoint,
    pub prevout: TxOut,
    pub public_key: CompressedPublicKey,
    pub signing_key_id: String,
}

/// One digest for RAW signing. Provider adapters resolve the opaque key reference.
#[derive(Debug, Clone)]
pub struct SigningInput {
    pub input_index: usize,
    pub digest: [u8; 32],
    pub public_key: CompressedPublicKey,
    pub signing_key_id: String,
}

/// RAW ECDSA r||s bytes, explicitly mapped to an input. No recovery byte.
pub struct InputSignature {
    pub input_index: usize,
    pub compact: Vec<u8>,
}

/// Immutable transaction and signing plan retained across external signing.
pub struct PreparedP2wpkh {
    original: Psbt,
    plan: Vec<SigningInput>,
    fee_sat: u64,
}

fn invalid(details: impl Into<String>) -> rgb_lib::Error {
    rgb_lib::Error::InvalidPsbt {
        details: details.into(),
    }
}

impl PreparedP2wpkh {
    /// Validate all inputs as native P2WPKH with SIGHASH_ALL and a bounded fee.
    /// Mixed, Taproot and externally finalized inputs are rejected in this spike.
    pub fn prepare(
        original: Psbt,
        verified_inputs: &[VerifiedInput],
        max_fee_sat: u64,
    ) -> Result<Self, rgb_lib::Error> {
        if original.inputs.is_empty()
            || original.inputs.len() != original.unsigned_tx.input.len()
            || original.inputs.len() != verified_inputs.len()
            || original.outputs.len() != original.unsigned_tx.output.len()
            || original.outputs.is_empty()
        {
            return Err(invalid(
                "PSBT input/output count mismatch or empty transaction",
            ));
        }
        let mut seen = HashSet::new();
        let mut input_sum = 0u64;
        let mut plan = Vec::with_capacity(verified_inputs.len());
        let mut cache = SighashCache::new(&original.unsigned_tx);
        for (index, verified) in verified_inputs.iter().enumerate() {
            let tx_input = &original.unsigned_tx.input[index];
            let psbt_input = &original.inputs[index];
            if tx_input.previous_output != verified.outpoint || !seen.insert(verified.outpoint) {
                return Err(invalid("Unknown, reordered or duplicate prevout"));
            }
            if !tx_input.script_sig.is_empty()
                || !tx_input.witness.is_empty()
                || psbt_input.final_script_sig.is_some()
                || psbt_input.final_script_witness.is_some()
                || !psbt_input.partial_sigs.is_empty()
                || psbt_input.tap_key_sig.is_some()
                || !psbt_input.tap_script_sigs.is_empty()
            {
                return Err(invalid("Prepared transaction must not contain signatures"));
            }
            if verified.signing_key_id.is_empty()
                || verified.prevout.script_pubkey
                    != ScriptBuf::new_p2wpkh(&verified.public_key.wpubkey_hash())
                || psbt_input.witness_utxo.as_ref() != Some(&verified.prevout)
                || psbt_input.redeem_script.is_some()
                || psbt_input.witness_script.is_some()
            {
                return Err(invalid(
                    "P2WPKH script, key or node-verified prevout mismatch",
                ));
            }
            if let Some(previous) = &psbt_input.non_witness_utxo
                && (previous.compute_txid() != verified.outpoint.txid
                    || previous.output.get(verified.outpoint.vout as usize)
                        != Some(&verified.prevout))
            {
                return Err(invalid(
                    "Non-witness UTXO disagrees with the verified prevout",
                ));
            }
            if psbt_input
                .sighash_type
                .is_some_and(|value| value != EcdsaSighashType::All.into())
            {
                return Err(invalid("Only SIGHASH_ALL is supported"));
            }
            input_sum = input_sum
                .checked_add(verified.prevout.value.to_sat())
                .ok_or_else(|| invalid("Input value overflow"))?;
            let hash = cache
                .p2wpkh_signature_hash(
                    index,
                    &verified.prevout.script_pubkey,
                    verified.prevout.value,
                    EcdsaSighashType::All,
                )
                .map_err(|error| invalid(error.to_string()))?;
            plan.push(SigningInput {
                input_index: index,
                digest: hash.to_byte_array(),
                public_key: verified.public_key,
                signing_key_id: verified.signing_key_id.clone(),
            });
        }
        let output_sum = original
            .unsigned_tx
            .output
            .iter()
            .try_fold(0u64, |sum, output| {
                sum.checked_add(output.value.to_sat())
                    .ok_or_else(|| invalid("Output value overflow"))
            })?;
        let fee_sat = input_sum
            .checked_sub(output_sum)
            .ok_or_else(|| invalid("Outputs exceed inputs"))?;
        if fee_sat > max_fee_sat {
            return Err(invalid("Fee exceeds the approved absolute limit"));
        }
        Ok(Self {
            original,
            plan,
            fee_sat,
        })
    }

    pub fn signing_plan(&self) -> &[SigningInput] {
        &self.plan
    }
    pub fn fee_sat(&self) -> u64 {
        self.fee_sat
    }

    /// Verify a browser SDK's signed PSBT, retaining the original transaction
    /// and RGB metadata. Accept either partial ECDSA signatures or final native
    /// P2WPKH witnesses; a missing input never becomes a partial success.
    pub fn finalize_psbt(&self, response: &Psbt) -> Result<Psbt, rgb_lib::Error> {
        if response.unsigned_tx != self.original.unsigned_tx
            || response.version != self.original.version
            || response.inputs.len() != self.original.inputs.len()
            || response.outputs.len() != self.original.outputs.len()
        {
            return Err(invalid("Signer changed the prepared transaction"));
        }
        let mut signatures = Vec::with_capacity(self.plan.len());
        for (index, input) in response.inputs.iter().enumerate() {
            if input.final_script_sig.is_some()
                || input.tap_key_sig.is_some()
                || !input.tap_script_sigs.is_empty()
                || input.witness_utxo.as_ref().is_some_and(|value| {
                    Some(value) != self.original.inputs[index].witness_utxo.as_ref()
                })
            {
                return Err(invalid("Unexpected signer input data"));
            }
            let (signature, public_key) = match &input.final_script_witness {
                Some(witness) if witness.len() == 2 && input.partial_sigs.is_empty() => {
                    let mut items = witness.iter();
                    let signature = ecdsa::Signature::from_slice(items.next().unwrap())
                        .map_err(|_| invalid("Invalid P2WPKH witness signature"))?;
                    (signature, items.next().unwrap().to_vec())
                }
                None if input.partial_sigs.len() == 1 => {
                    let (key, signature) = input.partial_sigs.iter().next().unwrap();
                    (*signature, key.to_bytes())
                }
                _ => return Err(invalid("Expected one P2WPKH signature per input")),
            };
            if signature.sighash_type != EcdsaSighashType::All
                || public_key != self.plan[index].public_key.to_bytes()
            {
                return Err(invalid("Signer changed the key or approved sighash type"));
            }
            signatures.push(InputSignature {
                input_index: index,
                compact: signature.signature.serialize_compact().to_vec(),
            });
        }
        self.finalize(&signatures)
    }

    /// Verify every RAW response and assemble witnesses on the original PSBT.
    /// Cloning the original preserves RGB proprietary fields. An invalid or
    /// incomplete response cannot partially mutate the prepared transaction.
    pub fn finalize(&self, signatures: &[InputSignature]) -> Result<Psbt, rgb_lib::Error> {
        if signatures.len() != self.plan.len() {
            return Err(invalid("One signature per input is required"));
        }
        let secp = Secp256k1::verification_only();
        let mut seen = HashSet::new();
        let mut signed = self.original.clone();
        for response in signatures {
            let Some(input) = self.plan.get(response.input_index) else {
                return Err(invalid("Unknown signature input index"));
            };
            if !seen.insert(response.input_index) {
                return Err(invalid("Duplicate signature input index"));
            }
            let mut signature = Signature::from_compact(&response.compact)
                .map_err(|error| invalid(error.to_string()))?;
            signature.normalize_s();
            secp.verify_ecdsa(
                &Message::from_digest(input.digest),
                &signature,
                &input.public_key.0,
            )
            .map_err(|_| {
                invalid("Signature does not match the original transaction and bound key")
            })?;
            let bitcoin_signature = ecdsa::Signature::sighash_all(signature);
            signed.inputs[response.input_index].final_script_witness =
                Some(Witness::p2wpkh(&bitcoin_signature, &input.public_key.0));
        }
        Ok(signed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rgb_lib::bitcoin::{
        Amount, Sequence, Transaction, TxIn, Txid, absolute, psbt::raw::ProprietaryKey,
        secp256k1::SecretKey, transaction,
    };

    fn fixture() -> (Psbt, Vec<VerifiedInput>, Vec<SecretKey>) {
        let keys: Vec<_> = [1, 2]
            .map(|byte| SecretKey::from_slice(&[byte; 32]).unwrap())
            .into();
        let verified: Vec<_> = keys
            .iter()
            .enumerate()
            .map(|(index, key)| {
                let public_key = CompressedPublicKey(key.public_key(&Secp256k1::new()));
                VerifiedInput {
                    outpoint: OutPoint {
                        txid: Txid::from_byte_array([index as u8 + 1; 32]),
                        vout: 0,
                    },
                    prevout: TxOut {
                        value: Amount::from_sat(50_000),
                        script_pubkey: ScriptBuf::new_p2wpkh(&public_key.wpubkey_hash()),
                    },
                    public_key,
                    signing_key_id: format!("test-key-{index}"),
                }
            })
            .collect();
        let tx = Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: verified
                .iter()
                .map(|input| TxIn {
                    previous_output: input.outpoint,
                    script_sig: ScriptBuf::new(),
                    sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
                    witness: Witness::new(),
                })
                .collect(),
            output: vec![TxOut {
                value: Amount::from_sat(99_000),
                script_pubkey: verified[0].prevout.script_pubkey.clone(),
            }],
        };
        let mut psbt = Psbt::from_unsigned_tx(tx).unwrap();
        for (input, verified) in psbt.inputs.iter_mut().zip(&verified) {
            input.witness_utxo = Some(verified.prevout.clone());
        }
        psbt.proprietary.insert(
            ProprietaryKey {
                prefix: b"RGB".to_vec(),
                subtype: 1,
                key: vec![1],
            },
            b"fixture-metadata".to_vec(),
        );
        (psbt, verified, keys)
    }

    fn sign(prepared: &PreparedP2wpkh, keys: &[SecretKey]) -> Vec<InputSignature> {
        prepared
            .signing_plan()
            .iter()
            .map(|input| InputSignature {
                input_index: input.input_index,
                compact: Secp256k1::new()
                    .sign_ecdsa(
                        &Message::from_digest(input.digest),
                        &keys[input.input_index],
                    )
                    .serialize_compact()
                    .to_vec(),
            })
            .collect()
    }

    #[test]
    fn external_signatures_finalize_original_transaction_and_preserve_metadata() {
        let (original, verified, keys) = fixture();
        let prepared = PreparedP2wpkh::prepare(original.clone(), &verified, 1_000).unwrap();
        assert_eq!(prepared.fee_sat(), 1_000);
        let mut signatures = sign(&prepared, &keys);
        signatures.reverse(); // Mapping uses input index, not response order.
        let signed = prepared.finalize(&signatures).unwrap();
        assert_eq!(signed.unsigned_tx, original.unsigned_tx);
        assert_eq!(signed.proprietary, original.proprietary);
        let transaction = signed.extract_tx().unwrap();
        assert_eq!(
            transaction.compute_txid(),
            original.unsigned_tx.compute_txid()
        );
        for (index, input) in transaction.input.iter().enumerate() {
            let items: Vec<_> = input.witness.iter().collect();
            assert_eq!(items.len(), 2);
            let signature = ecdsa::Signature::from_slice(items[0]).unwrap();
            assert_eq!(signature.sighash_type, EcdsaSighashType::All);
            assert_eq!(items[1], verified[index].public_key.to_bytes());
            let hash = SighashCache::new(&transaction)
                .p2wpkh_signature_hash(
                    index,
                    &verified[index].prevout.script_pubkey,
                    verified[index].prevout.value,
                    EcdsaSighashType::All,
                )
                .unwrap();
            Secp256k1::verification_only()
                .verify_ecdsa(
                    &Message::from_digest(hash.to_byte_array()),
                    &signature.signature,
                    &verified[index].public_key.0,
                )
                .unwrap();
        }
    }

    #[test]
    fn wrong_keys_modified_transaction_and_partial_responses_are_rejected() {
        let (original, verified, keys) = fixture();
        let prepared = PreparedP2wpkh::prepare(original.clone(), &verified, 1_000).unwrap();
        let mut wrong_keys = keys.clone();
        wrong_keys.reverse();
        assert!(prepared.finalize(&sign(&prepared, &wrong_keys)).is_err());
        let signatures = sign(&prepared, &keys);
        assert!(prepared.finalize(&signatures[..1]).is_err());
        let mut duplicate = sign(&prepared, &keys);
        duplicate[1].input_index = 0;
        assert!(prepared.finalize(&duplicate).is_err());
        let mut modified = original;
        modified.unsigned_tx.output[0].script_pubkey = verified[1].prevout.script_pubkey.clone();
        let changed = PreparedP2wpkh::prepare(modified, &verified, 1_000).unwrap();
        assert!(changed.finalize(&signatures).is_err());
        assert!(prepared.finalize(&signatures).is_ok()); // Failures didn't mutate the plan.
    }

    #[test]
    fn forged_prevouts_keys_sighashes_duplicates_and_excess_fees_fail_preparation() {
        let (original, verified, _) = fixture();
        assert!(PreparedP2wpkh::prepare(original.clone(), &verified, 999).is_err());
        let mut forged = original.clone();
        forged.inputs[0].witness_utxo.as_mut().unwrap().value = Amount::from_sat(60_000);
        assert!(PreparedP2wpkh::prepare(forged, &verified, 20_000).is_err());
        let mut bad_binding = verified.clone();
        bad_binding[0].public_key = verified[1].public_key;
        assert!(PreparedP2wpkh::prepare(original.clone(), &bad_binding, 1_000).is_err());
        let mut bad_hash = original.clone();
        bad_hash.inputs[0].sighash_type = Some(EcdsaSighashType::None.into());
        assert!(PreparedP2wpkh::prepare(bad_hash, &verified, 1_000).is_err());
        let mut duplicate = original.clone();
        duplicate.unsigned_tx.input[1] = duplicate.unsigned_tx.input[0].clone();
        duplicate.inputs[1] = duplicate.inputs[0].clone();
        assert!(
            PreparedP2wpkh::prepare(
                duplicate,
                &[verified[0].clone(), verified[0].clone()],
                1_000
            )
            .is_err()
        );
        let mut excessive_output = original;
        excessive_output.unsigned_tx.output[0].value = Amount::from_sat(100_001);
        assert!(PreparedP2wpkh::prepare(excessive_output, &verified, u64::MAX).is_err());
    }

    #[test]
    fn embedded_psbt_response_is_verified_and_rgb_metadata_is_retained() {
        let (original, verified, keys) = fixture();
        let prepared = PreparedP2wpkh::prepare(original.clone(), &verified, 1_000).unwrap();
        let mut partial = original.clone();
        for signature in sign(&prepared, &keys) {
            partial.inputs[signature.input_index].partial_sigs.insert(
                verified[signature.input_index].public_key.into(),
                ecdsa::Signature::sighash_all(Signature::from_compact(&signature.compact).unwrap()),
            );
        }
        partial.proprietary.clear();
        let finalized = prepared.finalize_psbt(&partial).unwrap();
        assert_eq!(finalized.proprietary, original.proprietary);
        assert_eq!(finalized.unsigned_tx, original.unsigned_tx);
        assert!(finalized.clone().extract_tx().is_ok());
        assert!(prepared.finalize_psbt(&finalized).is_ok());

        let mut modified = partial.clone();
        modified.unsigned_tx.output[0].value -= Amount::from_sat(1);
        assert!(prepared.finalize_psbt(&modified).is_err());
        let mut missing = partial.clone();
        missing.inputs[1].partial_sigs.clear();
        assert!(prepared.finalize_psbt(&missing).is_err());
        let mut wrong_key = partial.clone();
        wrong_key.inputs.swap(0, 1);
        assert!(prepared.finalize_psbt(&wrong_key).is_err());
        let mut weak = partial.clone();
        weak.inputs[0]
            .partial_sigs
            .values_mut()
            .next()
            .unwrap()
            .sighash_type = EcdsaSighashType::None;
        assert!(prepared.finalize_psbt(&weak).is_err());
        let mut forged = partial;
        forged.inputs[0].witness_utxo.as_mut().unwrap().value += Amount::from_sat(1);
        assert!(prepared.finalize_psbt(&forged).is_err());
    }
}

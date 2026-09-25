//! Offline verification of externally signed Taproot key-path PSBTs.
//! The caller supplies authenticated provider bindings and node-verified prevouts,
//! and validates RGB intent and recipient/change outputs before preparation.
//! This module holds no secrets and cannot authorize or broadcast a transaction.
use std::collections::HashSet;

use rgb_lib::bitcoin::{
    OutPoint, Psbt, ScriptBuf, TapSighashType, TxOut, Witness,
    hashes::Hash,
    key::{TapTweak, TweakedPublicKey},
    secp256k1::{Message, Secp256k1, XOnlyPublicKey},
    sighash::{Prevouts, SighashCache},
    taproot,
};

#[derive(Clone)]
pub struct VerifiedTaprootInput {
    pub outpoint: OutPoint,
    pub prevout: TxOut,
    /// The tweaked output key, authenticated through the provider binding.
    pub output_key: XOnlyPublicKey,
}

pub struct PreparedTaproot {
    original: Psbt,
    inputs: Vec<VerifiedTaprootInput>,
    sighashes: Vec<TapSighashType>,
    fee_sat: u64,
}

fn invalid(details: &str) -> rgb_lib::Error {
    rgb_lib::Error::InvalidPsbt {
        details: details.into(),
    }
}

impl PreparedTaproot {
    pub fn prepare(
        original: Psbt,
        inputs: &[VerifiedTaprootInput],
        max_fee_sat: u64,
    ) -> Result<Self, rgb_lib::Error> {
        if inputs.is_empty()
            || original.inputs.len() != inputs.len()
            || original.unsigned_tx.input.len() != inputs.len()
            || original.outputs.is_empty()
            || original.outputs.len() != original.unsigned_tx.output.len()
        {
            return Err(invalid("Invalid Taproot PSBT input/output count"));
        }
        let mut seen = HashSet::new();
        let mut input_sum = 0u64;
        let mut sighashes = Vec::new();
        for ((txin, input), verified) in original
            .unsigned_tx
            .input
            .iter()
            .zip(&original.inputs)
            .zip(inputs)
        {
            if txin.previous_output != verified.outpoint || !seen.insert(verified.outpoint) {
                return Err(invalid("Unknown, reordered or duplicate Taproot prevout"));
            }
            let expected_script = ScriptBuf::new_p2tr_tweaked(
                TweakedPublicKey::dangerous_assume_tweaked(verified.output_key),
            );
            if verified.prevout.script_pubkey != expected_script
                || input.witness_utxo.as_ref() != Some(&verified.prevout)
            {
                return Err(invalid("Taproot key or prevout mismatch"));
            }
            if let Some(previous) = &input.non_witness_utxo
                && (previous.compute_txid() != verified.outpoint.txid
                    || previous.output.get(verified.outpoint.vout as usize)
                        != Some(&verified.prevout))
            {
                return Err(invalid("Taproot previous transaction mismatch"));
            }
            if !txin.script_sig.is_empty()
                || !txin.witness.is_empty()
                || input.final_script_sig.is_some()
                || input.final_script_witness.is_some()
                || !input.partial_sigs.is_empty()
                || input.tap_key_sig.is_some()
                || !input.tap_script_sigs.is_empty()
                || !input.tap_scripts.is_empty()
                || input.redeem_script.is_some()
                || input.witness_script.is_some()
            {
                return Err(invalid(
                    "Expected an unsigned native Taproot key-path input",
                ));
            }
            if let Some(internal) = input.tap_internal_key {
                let expected = internal
                    .tap_tweak(&Secp256k1::verification_only(), input.tap_merkle_root)
                    .0;
                if expected.serialize() != verified.output_key.serialize() {
                    return Err(invalid(
                        "Taproot internal key does not produce the registered output key",
                    ));
                }
            } else if input.tap_merkle_root.is_some() {
                return Err(invalid(
                    "Taproot merkle root requires a verified internal key",
                ));
            }
            let sighash = input
                .sighash_type
                .map(|value| value.taproot_hash_ty())
                .transpose()
                .map_err(|_| invalid("Invalid Taproot sighash"))?
                .unwrap_or(TapSighashType::Default);
            if !matches!(sighash, TapSighashType::Default | TapSighashType::All) {
                return Err(invalid(
                    "Taproot signing must commit to every input and output",
                ));
            }
            sighashes.push(sighash);
            input_sum = input_sum
                .checked_add(verified.prevout.value.to_sat())
                .ok_or_else(|| invalid("Input value overflow"))?;
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
            return Err(invalid("Fee exceeds the approved limit"));
        }
        Ok(Self {
            original,
            inputs: inputs.to_vec(),
            sighashes,
            fee_sat,
        })
    }

    pub fn fee_sat(&self) -> u64 {
        self.fee_sat
    }

    /// Accept signatures only; all transaction and RGB metadata comes from the
    /// retained original. Supports unfinalized tap_key_sig or a one-item witness.
    pub fn finalize_psbt(&self, response: &Psbt) -> Result<Psbt, rgb_lib::Error> {
        if response.unsigned_tx != self.original.unsigned_tx
            || response.version != self.original.version
            || response.inputs.len() != self.inputs.len()
            || response.outputs.len() != self.original.outputs.len()
        {
            return Err(invalid("Signer changed the prepared transaction"));
        }
        let prevouts: Vec<_> = self
            .inputs
            .iter()
            .map(|input| input.prevout.clone())
            .collect();
        let mut cache = SighashCache::new(&self.original.unsigned_tx);
        let mut signed = self.original.clone();
        for (index, input) in response.inputs.iter().enumerate() {
            if input.final_script_sig.is_some()
                || !input.partial_sigs.is_empty()
                || !input.tap_script_sigs.is_empty()
                || !input.tap_scripts.is_empty()
                || input
                    .witness_utxo
                    .as_ref()
                    .is_some_and(|value| value != &prevouts[index])
            {
                return Err(invalid("Unexpected signer input data"));
            }
            let signature = match (&input.tap_key_sig, &input.final_script_witness) {
                (Some(signature), None) => *signature,
                (None, Some(witness)) if witness.len() == 1 => {
                    taproot::Signature::from_slice(witness.iter().next().unwrap())
                        .map_err(|_| invalid("Invalid Taproot witness signature"))?
                }
                _ => {
                    return Err(invalid(
                        "Expected exactly one Taproot key-path signature per input",
                    ));
                }
            };
            if signature.sighash_type != self.sighashes[index] {
                return Err(invalid("Signer changed the approved sighash type"));
            }
            let digest = cache
                .taproot_key_spend_signature_hash(
                    index,
                    &Prevouts::All(&prevouts),
                    signature.sighash_type,
                )
                .map_err(|_| invalid("Cannot calculate Taproot signing digest"))?;
            Secp256k1::verification_only()
                .verify_schnorr(
                    &signature.signature,
                    &Message::from_digest(digest.to_byte_array()),
                    &self.inputs[index].output_key,
                )
                .map_err(|_| invalid("Invalid Taproot signature for the registered output key"))?;
            let target = &mut signed.inputs[index];
            target.final_script_witness = Some(Witness::from_slice(&[signature.to_vec()]));
            target.tap_internal_key = None;
            target.tap_merkle_root = None;
            target.tap_key_origins.clear();
            target.sighash_type = None;
        }
        Ok(signed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rgb_lib::bitcoin::{
        Amount, Sequence, Transaction, TxIn, Txid, absolute,
        psbt::raw::ProprietaryKey,
        secp256k1::{Keypair, SecretKey},
        transaction,
    };

    fn fixture() -> (Psbt, Vec<VerifiedTaprootInput>, Vec<Keypair>) {
        let secp = Secp256k1::new();
        let keys: Vec<_> = [3, 4]
            .map(|byte| {
                Keypair::from_secret_key(&secp, &SecretKey::from_slice(&[byte; 32]).unwrap())
            })
            .into();
        let inputs: Vec<_> = keys
            .iter()
            .enumerate()
            .map(|(index, key)| {
                let output_key = key.x_only_public_key().0.tap_tweak(&secp, None).0;
                VerifiedTaprootInput {
                    outpoint: OutPoint::new(Txid::from_byte_array([index as u8 + 1; 32]), 0),
                    prevout: TxOut {
                        value: Amount::from_sat(5_000),
                        script_pubkey: ScriptBuf::new_p2tr_tweaked(output_key),
                    },
                    output_key: XOnlyPublicKey::from_slice(&output_key.serialize()).unwrap(),
                }
            })
            .collect();
        let tx = Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: inputs
                .iter()
                .map(|input| TxIn {
                    previous_output: input.outpoint,
                    script_sig: ScriptBuf::new(),
                    sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
                    witness: Witness::new(),
                })
                .collect(),
            output: vec![TxOut {
                value: Amount::from_sat(9_500),
                script_pubkey: inputs[0].prevout.script_pubkey.clone(),
            }],
        };
        let mut psbt = Psbt::from_unsigned_tx(tx).unwrap();
        for (index, input) in psbt.inputs.iter_mut().enumerate() {
            input.witness_utxo = Some(inputs[index].prevout.clone());
            input.tap_internal_key = Some(keys[index].x_only_public_key().0);
        }
        psbt.proprietary.insert(
            ProprietaryKey {
                prefix: b"RGB".to_vec(),
                subtype: 1,
                key: vec![1],
            },
            b"preserve-rgb".to_vec(),
        );
        (psbt, inputs, keys)
    }

    fn sign(
        original: &Psbt,
        inputs: &[VerifiedTaprootInput],
        keys: &[Keypair],
        tweaked: bool,
    ) -> Psbt {
        let prevouts: Vec<_> = inputs.iter().map(|input| input.prevout.clone()).collect();
        let mut cache = SighashCache::new(&original.unsigned_tx);
        let mut signed = original.clone();
        for (index, input) in signed.inputs.iter_mut().enumerate() {
            let sighash_type = input
                .sighash_type
                .map(|s| s.taproot_hash_ty().unwrap())
                .unwrap_or(TapSighashType::Default);
            let digest = cache
                .taproot_key_spend_signature_hash(index, &Prevouts::All(&prevouts), sighash_type)
                .unwrap();
            let key = if tweaked {
                keys[index].tap_tweak(&Secp256k1::new(), None).to_keypair()
            } else {
                keys[index]
            };
            input.tap_key_sig = Some(taproot::Signature {
                signature: Secp256k1::new()
                    .sign_schnorr_no_aux_rand(&Message::from_digest(digest.to_byte_array()), &key),
                sighash_type,
            });
        }
        signed
    }

    #[test]
    fn external_psbt_signatures_preserve_original_rgb_and_finalize_every_input() {
        for sighash in [TapSighashType::Default, TapSighashType::All] {
            let (mut original, inputs, keys) = fixture();
            for input in &mut original.inputs {
                input.sighash_type = Some(sighash.into());
            }
            let prepared = PreparedTaproot::prepare(original.clone(), &inputs, 500).unwrap();
            assert_eq!(prepared.fee_sat(), 500);
            let mut response = sign(&original, &inputs, &keys, true);
            response.proprietary.clear(); // SDK may omit unknown metadata; never use its copy.
            let signed = prepared.finalize_psbt(&response).unwrap();
            assert_eq!(signed.proprietary, original.proprietary);
            assert_eq!(signed.unsigned_tx, original.unsigned_tx);
            assert!(
                signed.inputs.iter().all(|input| input
                    .final_script_witness
                    .as_ref()
                    .unwrap()
                    .len()
                    == 1)
            );
            assert!(signed.clone().extract_tx().is_ok());
            assert!(prepared.finalize_psbt(&signed).is_ok()); // Already finalized SDK response.
        }
    }

    #[test]
    fn rejects_internal_key_as_output_key_wrong_signatures_and_changed_transaction() {
        let (original, inputs, keys) = fixture();
        let prepared = PreparedTaproot::prepare(original.clone(), &inputs, 500).unwrap();
        assert!(
            prepared
                .finalize_psbt(&sign(&original, &inputs, &keys, false))
                .is_err()
        );
        let mut bad_key = original.clone();
        bad_key.inputs[0].tap_internal_key = Some(inputs[0].output_key);
        assert!(PreparedTaproot::prepare(bad_key, &inputs, 500).is_err());
        let response = sign(&original, &inputs, &keys, true);
        let mut changed = response.clone();
        changed.unsigned_tx.output[0].value = Amount::from_sat(9_000);
        assert!(prepared.finalize_psbt(&changed).is_err());
        let mut missing = response.clone();
        missing.inputs[1].tap_key_sig = None;
        assert!(prepared.finalize_psbt(&missing).is_err());
        let mut swapped = response.clone();
        swapped.inputs.swap(0, 1);
        assert!(prepared.finalize_psbt(&swapped).is_err());
        let mut witness = response.clone();
        witness.inputs[0].final_script_witness =
            Some(Witness::from_slice(&[vec![0; 64], vec![0x50]]));
        witness.inputs[0].tap_key_sig = None;
        assert!(prepared.finalize_psbt(&witness).is_err());
    }

    #[test]
    fn rejects_forged_prevouts_unsafe_sighashes_and_excess_fees() {
        let (original, inputs, _) = fixture();
        assert!(PreparedTaproot::prepare(original.clone(), &inputs, 499).is_err());
        let mut forged = original.clone();
        forged.inputs[0].witness_utxo.as_mut().unwrap().value = Amount::from_sat(6_000);
        assert!(PreparedTaproot::prepare(forged, &inputs, 2_000).is_err());
        let mut weak = original.clone();
        weak.inputs[0].sighash_type = Some(TapSighashType::AllPlusAnyoneCanPay.into());
        assert!(PreparedTaproot::prepare(weak, &inputs, 500).is_err());
        let mut duplicate = original;
        duplicate.unsigned_tx.input[1] = duplicate.unsigned_tx.input[0].clone();
        duplicate.inputs[1] = duplicate.inputs[0].clone();
        assert!(
            PreparedTaproot::prepare(duplicate, &[inputs[0].clone(), inputs[0].clone()], 500)
                .is_err()
        );
    }
}

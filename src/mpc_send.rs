//! DYNAMIC_EMBEDDED_POC: shared external PSBT sends on the configured Bitcoin network.
//! Provider identity does not select signing or transaction behavior. Retain this
//! module when removing a provider integration.
use super::*;
use crate::signing::{PreparedP2wpkh, VerifiedInput};
use crate::wallet::MpcSendPolicy;
use rgb_lib::{
    TransferStatus,
    wallet::{Invoice, Recipient, Unspent},
};
use std::collections::HashMap;

#[cfg(test)]
const ASSET: &str = "rgb:qXB4xkhB-3pmU6PB-rTy_qNd-ErrO4OQ-8GFLLQq-gzGoing";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SendProfile {
    P2wpkhBlind,
}

fn send_address(registration: &Registration) -> Result<&RegisteredAddress> {
    match registration.addresses.as_slice() {
        [address] if address.role == Role::Rgb && address.script_type == ScriptType::P2wpkh => {
            Ok(address)
        }
        _ => Err(MpcError::Invalid(
            "External send requires one RGB P2WPKH address without a separate fee address",
        )),
    }
}

pub(super) fn supported_profile(registration: &Registration) -> Option<SendProfile> {
    send_address(registration)
        .ok()
        .map(|_| SendProfile::P2wpkhBlind)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SendRequest {
    pub request_id: Uuid,
    pub invoice: String,
    pub amount: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct SendView {
    pub wallet_id: Uuid,
    pub request_id: Uuid,
    pub state: String,
    pub amount: String,
    pub invoice: String,
    pub psbt: Option<String>,
    pub txid: Option<String>,
    pub fee_sat: Option<u64>,
    pub input_indexes: Vec<usize>,
    pub batch_transfer_idx: Option<i32>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Journal {
    request: SendRequest,
    view: SendView,
    original_psbt: Option<String>,
    #[serde(default)]
    prior_batch_ids: Option<Vec<i32>>,
    #[serde(default)]
    input_snapshot: Option<Vec<Unspent>>,
    #[serde(default)]
    submission_started: bool,
    #[serde(default)]
    policy: MpcSendPolicy,
}

fn terminal(state: &str) -> bool {
    matches!(state, "SETTLED" | "FAILED" | "CANCELLED")
}

fn now() -> Result<u64> {
    Ok(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| MpcError::Internal)?
        .as_secs())
}

fn invoice(
    request: &SendRequest,
    endpoints: &[String],
    network: BitcoinNetwork,
    policy: &MpcSendPolicy,
) -> Result<rgb_lib::wallet::InvoiceData> {
    policy.validate().map_err(MpcError::Invalid)?;
    let amount = request
        .amount
        .parse::<u64>()
        .map_err(|_| MpcError::Invalid("Invalid asset amount"))?;
    if !(1..=policy.max_amount).contains(&amount)
        || amount.to_string() != request.amount
        || request.invoice.len() > 16_384
    {
        return Err(MpcError::Invalid(
            "Amount exceeds the configured send limit or is not canonical",
        ));
    }
    let data = Invoice::new(request.invoice.clone())?.invoice_data();
    let minimum_expiry = now()?.saturating_add(policy.min_invoice_validity_secs);
    if data.network != network
        || data.asset_id.is_none()
        || data.assignment != Assignment::Fungible(amount)
        || data
            .expiration_timestamp
            .is_none_or(|expiry| expiry <= minimum_expiry)
        || data.transport_endpoints.len() != 1
        || !endpoints.iter().any(|endpoint| {
            Some(endpoint.as_str()) == data.transport_endpoints[0].split('?').next()
        })
        || rgb_lib::utils::script_buf_from_recipient_id(data.recipient_id.clone())?.is_some()
    {
        return Err(MpcError::Invalid(
            "Expected an unexpired asset blind invoice on the configured network and transport",
        ));
    }
    Ok(data)
}

// The immutable invoice pins the asset, including legacy NIA journals. Never use
// the currently selected UI asset when recovering an existing operation.
fn request_asset(request: &SendRequest) -> Result<String> {
    Invoice::new(request.invoice.clone())?
        .invoice_data()
        .asset_id
        .ok_or(MpcError::Invalid("An asset-bound invoice is required"))
}

/// Validate the server-generated transaction and preserve its RGB metadata.
/// At preparation, input amounts are separately checked against synced UTXOs.
fn plan(
    record: &Record,
    psbt: Psbt,
    unspents: &[Unspent],
    require_settled: bool,
    asset_id: &str,
    network: BitcoinNetwork,
    policy: &MpcSendPolicy,
) -> Result<PreparedP2wpkh> {
    policy.validate().map_err(MpcError::Invalid)?;
    let address = send_address(&record.registration)?;
    let key = CompressedPublicKey::from_str(&address.public_key)
        .map_err(|_| MpcError::Invalid("Invalid public key"))?;
    let script = checked_address(address, network)?.script_pubkey();
    if psbt.inputs.is_empty()
        || psbt.inputs.len() > policy.max_inputs
        || psbt.unsigned_tx.output.len() != 2
        || !psbt.unsigned_tx.output[0].script_pubkey.is_op_return()
        || psbt.unsigned_tx.output[0].script_pubkey.len() != 34
        || psbt.unsigned_tx.output[0].value.to_sat() != 0
        || psbt.unsigned_tx.output[1].script_pubkey != script
        || psbt.unsigned_tx.output[1].value < script.minimal_non_dust()
    {
        return Err(MpcError::Invalid("Unexpected RGB return outputs"));
    }
    let verified = psbt
        .unsigned_tx
        .input
        .iter()
        .map(|input| {
            let unspent = unspents
                .iter()
                .find(|u| {
                    u.utxo.exists
                        && u.utxo.outpoint.txid == input.previous_output.txid.to_string()
                        && u.utxo.outpoint.vout == input.previous_output.vout
                })
                .ok_or(MpcError::Conflict(
                    "Input differs from synced RGB wallet state",
                ))?;
            if require_settled
                && (unspent.pending_blinded != 0
                    || unspent
                        .rgb_allocations
                        .iter()
                        .any(|a| !a.settled || a.asset_id.as_deref() != Some(asset_id)))
            {
                return Err(MpcError::Conflict(
                    "Input has pending or unrelated RGB allocations",
                ));
            }
            let prevout = rgb_lib::bitcoin::TxOut {
                value: rgb_lib::bitcoin::Amount::from_sat(unspent.utxo.btc_amount),
                script_pubkey: script.clone(),
            };
            Ok(VerifiedInput {
                outpoint: input.previous_output,
                prevout,
                public_key: key,
                signing_key_id: address.signing_key_id.clone(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(PreparedP2wpkh::prepare(
        psbt,
        &verified,
        policy.max_fee_sat,
    )?)
}

impl MpcService {
    pub async fn prepare_send(
        &self,
        owner: Owner,
        id: Uuid,
        request: SendRequest,
    ) -> Result<SendView> {
        self.call(id, move |manager| manager.prepare_send(&owner, id, request))
            .await
    }
    pub async fn finish_send(
        &self,
        owner: Owner,
        id: Uuid,
        request_id: Uuid,
        signed_psbt: String,
    ) -> Result<SendView> {
        self.call(id, move |manager| {
            manager.finish_send(&owner, id, request_id, signed_psbt)
        })
        .await
    }
    pub async fn cancel_send(&self, owner: Owner, id: Uuid, request_id: Uuid) -> Result<SendView> {
        self.call(id, move |manager| {
            manager.cancel_send(&owner, id, request_id)
        })
        .await
    }
    pub async fn send_status(&self, owner: Owner, id: Uuid, request_id: Uuid) -> Result<SendView> {
        self.call(id, move |manager| {
            let record = manager.owned(&owner, id)?;
            let mut journal = manager.read_send(id, request_id)?;
            manager.reconcile_send(&record, &mut journal)?;
            Ok(journal.view)
        })
        .await
    }
}

impl Manager {
    fn send_path(&self, id: Uuid, request_id: Uuid) -> PathBuf {
        self.root
            .join("sends")
            .join(id.to_string())
            .join(format!("{request_id}.json"))
    }
    fn read_send(&self, id: Uuid, request_id: Uuid) -> Result<Journal> {
        let bytes = fs::read(self.send_path(id, request_id)).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                MpcError::NotFound
            } else {
                MpcError::Internal
            }
        })?;
        let journal: Journal = serde_json::from_slice(&bytes)?;
        if journal.view.wallet_id != id
            || journal.request.request_id != request_id
            || journal.view.request_id != request_id
        {
            return Err(MpcError::Internal);
        }
        Ok(journal)
    }
    fn save_send(&self, journal: &Journal) -> Result<()> {
        let path = self.send_path(journal.view.wallet_id, journal.request.request_id);
        let dir = path.parent().ok_or(MpcError::Internal)?;
        private_directory(dir)?;
        let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
        tmp.write_all(&serde_json::to_vec(journal)?)?;
        tmp.as_file().sync_all()?;
        tmp.persist(&path).map_err(|_| MpcError::Internal)?;
        fs::File::open(dir)?.sync_all()?;
        Ok(())
    }
    fn send_online(&mut self, record: &Record) -> Result<Online> {
        let options = OnlineOptions {
            indexer_url: self.config.indexer_address.clone(),
            skip_consistency_check: false,
            vanilla_sync_lookback: 20,
            eth_rpc_url: self.config.eth_rpc_url.clone(),
        };
        let entry = self.open(record)?;
        if let Some(online) = entry.online {
            return Ok(online);
        }
        let online = entry.wallet.go_online(options)?;
        entry.online = Some(online);
        Ok(online)
    }
    fn recover_preparation(&mut self, record: &Record, journal: &mut Journal) -> Result<()> {
        let Some(before) = &journal.prior_batch_ids else {
            return Ok(());
        };
        let transfers = self
            .open(record)?
            .wallet
            .list_transfers(AssetFilter::Id(request_asset(&journal.request)?), None)?;
        let added: Vec<_> = transfers
            .iter()
            .filter(|t| !before.contains(&t.batch_transfer_idx))
            .collect();
        if added.is_empty() {
            // No committed send and no PSBT was ever returned to a signer.
            journal.view.state = "FAILED".into();
            return self.save_send(journal);
        }
        if added.len() != 1 {
            return Err(MpcError::Conflict(
                "Ambiguous send preparation; operator reconciliation required",
            ));
        }
        let transfer = added[0];
        let invoice = Invoice::new(journal.request.invoice.clone())?.invoice_data();
        let endpoints = invoice
            .transport_endpoints
            .into_iter()
            .map(rgb_lib::wallet::TransportEndpoint::new)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if transfer.kind != rgb_lib::wallet::TransferKind::Send
            || transfer.status != TransferStatus::Initiated
            || transfer.recipient_id.as_deref() != Some(invoice.recipient_id.as_str())
            || transfer.requested_assignment != Some(invoice.assignment)
            || transfer.expiration_timestamp != invoice.expiration_timestamp
            || transfer.transport_endpoints.len() != endpoints.len()
            || !transfer
                .transport_endpoints
                .iter()
                .zip(&endpoints)
                .all(|(a, b)| a.endpoint == b.endpoint && a.transport_type == b.transport_type)
        {
            return Err(MpcError::Conflict(
                "Prepared transfer differs from the saved invoice",
            ));
        }
        let path =
            Path::new(transfer.psbt_path.as_deref().ok_or(MpcError::Internal)?).canonicalize()?;
        let wallet_dir = self
            .root
            .join("wallets")
            .join(record.registration.wallet_id.to_string())
            .canonicalize()?;
        if !path.starts_with(wallet_dir) {
            return Err(MpcError::Internal);
        }
        let original =
            Psbt::from_str(&fs::read_to_string(path)?).map_err(|_| MpcError::Internal)?;
        if Some(original.unsigned_tx.compute_txid().to_string()) != transfer.txid {
            return Err(MpcError::Internal);
        }
        let snapshot = journal.input_snapshot.as_ref().ok_or(MpcError::Internal)?;
        let prepared = plan(
            record,
            original.clone(),
            snapshot,
            true,
            &request_asset(&journal.request)?,
            self.network,
            &journal.policy,
        )?;
        journal.original_psbt = Some(original.to_string());
        journal.view.psbt = journal.original_psbt.clone();
        journal.view.txid = transfer.txid.clone();
        journal.view.batch_transfer_idx = Some(transfer.batch_transfer_idx);
        journal.view.fee_sat = Some(prepared.fee_sat());
        journal.view.input_indexes = prepared
            .signing_plan()
            .iter()
            .map(|i| i.input_index)
            .collect();
        journal.view.state = "AWAITING_SIGNATURE".into();
        self.save_send(journal)
    }

    fn reconcile_send(&mut self, record: &Record, journal: &mut Journal) -> Result<()> {
        if journal.view.state == "PREPARING" {
            return self.recover_preparation(record, journal);
        }
        if terminal(&journal.view.state) {
            return Ok(());
        }
        if journal.view.batch_transfer_idx.is_none() || journal.view.txid.is_none() {
            return Ok(());
        }
        if [
            "SUBMITTING",
            "WAITING_COUNTERPARTY",
            "WAITING_CONFIRMATIONS",
            "NEEDS_REVIEW",
        ]
        .contains(&journal.view.state.as_str())
        {
            let online = self.send_online(record)?;
            self.open(record)?
                .wallet
                .refresh(online, None, vec![], false)?;
        }
        let transfers = self
            .open(record)?
            .wallet
            .list_transfers(AssetFilter::Id(request_asset(&journal.request)?), None)?;
        if let Some(transfer) = transfers.iter().find(|t| {
            Some(t.batch_transfer_idx) == journal.view.batch_transfer_idx
                && t.txid == journal.view.txid
        }) {
            journal.view.state = match transfer.status {
                TransferStatus::Settled => "SETTLED",
                TransferStatus::WaitingConfirmations => "WAITING_CONFIRMATIONS",
                TransferStatus::WaitingCounterparty => "WAITING_COUNTERPARTY",
                TransferStatus::Failed if journal.view.state == "CANCELLING" => "CANCELLED",
                TransferStatus::Failed => "FAILED",
                _ => return Ok(()),
            }
            .into();
            if terminal(&journal.view.state) {
                journal.view.psbt = None;
            }
            self.save_send(journal)?;
        }
        Ok(())
    }

    fn cancel_send(&mut self, owner: &Owner, id: Uuid, request_id: Uuid) -> Result<SendView> {
        let record = self.owned(owner, id)?;
        let mut journal = self.read_send(id, request_id)?;
        if terminal(&journal.view.state) {
            return Ok(journal.view);
        }
        if journal.submission_started
            || matches!(
                journal.view.state.as_str(),
                "SUBMITTING" | "WAITING_COUNTERPARTY" | "WAITING_CONFIRMATIONS" | "NEEDS_REVIEW"
            )
        {
            return Err(MpcError::Conflict(
                "Submission may have started; reconcile the saved send instead of cancelling",
            ));
        }
        self.reconcile_send(&record, &mut journal)?;
        if terminal(&journal.view.state) {
            return Ok(journal.view);
        }
        if journal.submission_started
            || !["AWAITING_SIGNATURE", "CANCELLING"].contains(&journal.view.state.as_str())
        {
            return Err(MpcError::Conflict(
                "Submission may have started; reconcile the saved send instead of cancelling",
            ));
        }
        let online = self.send_online(&record)?;
        let wallet = &mut self.open(&record)?.wallet;
        let transfers =
            wallet.list_transfers(AssetFilter::Id(request_asset(&journal.request)?), None)?;
        if !transfers.iter().any(|t| {
            Some(t.batch_transfer_idx) == journal.view.batch_transfer_idx
                && t.txid == journal.view.txid
                && t.status == TransferStatus::Initiated
        }) {
            return Err(MpcError::Conflict(
                "Only a verified unsubmitted send can be cancelled",
            ));
        }
        journal.view.state = "CANCELLING".into();
        journal.view.psbt = None;
        self.save_send(&journal)?;
        self.open(&record)?.wallet.fail_transfers(
            online,
            journal.view.batch_transfer_idx,
            false,
            false,
        )?;
        self.reconcile_send(&record, &mut journal)?;
        Ok(journal.view)
    }

    fn prepare_send(&mut self, owner: &Owner, id: Uuid, request: SendRequest) -> Result<SendView> {
        let record = self.owned(owner, id)?;
        send_address(&record.registration)?;
        if self.send_path(id, request.request_id).exists() {
            let mut previous = self.read_send(id, request.request_id)?;
            if previous.request != request {
                return Err(MpcError::Conflict("Send request ID already used"));
            }
            self.reconcile_send(&record, &mut previous)?;
            if previous.view.state == "PREPARING" {
                return Err(MpcError::Conflict(
                    "Unknown send preparation outcome; reconcile the saved request",
                ));
            }
            return Ok(previous.view);
        }
        let policy = self.config.mpc_send.clone();
        let network = self.network;
        let data = invoice(&request, &self.config.proxy_address, network, &policy)?;
        let asset_id = request_asset(&request)?;
        let dir = self.root.join("sends").join(id.to_string());
        private_directory(&dir)?;
        for file in fs::read_dir(dir)? {
            let path = file?.path();
            if path.extension().is_some_and(|ext| ext == "json") {
                let request_id = registration_file_id(&path).ok_or(MpcError::Internal)?;
                let mut journal = self.read_send(id, request_id)?;
                self.reconcile_send(&record, &mut journal)?;
                if !terminal(&journal.view.state) {
                    return Err(MpcError::Conflict(
                        "Finish the active return before another send",
                    ));
                }
            }
        }
        let online = self.send_online(&record)?;
        let wallet = &mut self.open(&record)?.wallet;
        wallet.refresh(online, None, vec![], false)?;
        let unspents = wallet.list_unspents(Some(online), false, false)?;
        let recipients = HashMap::from([(
            asset_id.clone(),
            vec![Recipient {
                recipient_id: data.recipient_id,
                witness_data: None,
                assignment: data.assignment,
                transport_endpoints: data.transport_endpoints,
            }],
        )]);
        let expiry = data
            .expiration_timestamp
            .ok_or(MpcError::Invalid("Invoice expiry required"))?;
        // Check funding before writing a durable mutation intent.
        let dry = wallet.send_begin(
            online,
            recipients.clone(),
            false,
            policy.fee_rate_sat_vb,
            policy.min_confirmations,
            expiry,
            true,
        )?;
        plan(
            &record,
            Psbt::from_str(&dry.psbt).map_err(|_| MpcError::Internal)?,
            &unspents,
            true,
            &asset_id,
            network,
            &policy,
        )?;
        let prior_batch_ids = wallet
            .list_transfers(AssetFilter::Id(asset_id.clone()), None)?
            .iter()
            .map(|transfer| transfer.batch_transfer_idx)
            .collect();
        let mut journal = Journal {
            view: SendView {
                wallet_id: id,
                request_id: request.request_id,
                state: "PREPARING".into(),
                amount: request.amount.clone(),
                invoice: request.invoice.clone(),
                psbt: None,
                txid: None,
                fee_sat: None,
                input_indexes: vec![],
                batch_transfer_idx: None,
            },
            request,
            original_psbt: None,
            prior_batch_ids: Some(prior_batch_ids),
            input_snapshot: Some(unspents.clone()),
            submission_started: false,
            policy: policy.clone(),
        };
        self.save_send(&journal)?;
        let begun = self.open(&record)?.wallet.send_begin(
            online,
            recipients,
            false,
            policy.fee_rate_sat_vb,
            policy.min_confirmations,
            expiry,
            false,
        )?;
        let original = Psbt::from_str(&begun.psbt).map_err(|_| MpcError::Internal)?;
        let prepared = plan(
            &record,
            original.clone(),
            &unspents,
            true,
            &asset_id,
            network,
            &policy,
        )?;
        journal.original_psbt = Some(begun.psbt.clone());
        journal.view.psbt = Some(begun.psbt);
        journal.view.txid = Some(original.unsigned_tx.compute_txid().to_string());
        journal.view.fee_sat = Some(prepared.fee_sat());
        journal.view.input_indexes = prepared
            .signing_plan()
            .iter()
            .map(|input| input.input_index)
            .collect();
        journal.view.batch_transfer_idx = begun.batch_transfer_idx;
        journal.view.state = "AWAITING_SIGNATURE".into();
        self.save_send(&journal)?;
        Ok(journal.view)
    }
    fn finish_send(
        &mut self,
        owner: &Owner,
        id: Uuid,
        request_id: Uuid,
        signed_psbt: String,
    ) -> Result<SendView> {
        let record = self.owned(owner, id)?;
        let mut journal = self.read_send(id, request_id)?;
        self.reconcile_send(&record, &mut journal)?;
        if [
            "WAITING_COUNTERPARTY",
            "WAITING_CONFIRMATIONS",
            "SETTLED",
            "FAILED",
            "CANCELLED",
        ]
        .contains(&journal.view.state.as_str())
        {
            return Ok(journal.view);
        }
        if journal.view.state != "AWAITING_SIGNATURE" {
            return Err(MpcError::Conflict(
                "Send needs reconciliation; no new submission attempted",
            ));
        }
        invoice(
            &journal.request,
            &self.config.proxy_address,
            self.network,
            &journal.policy,
        )?;
        if signed_psbt.len() > 128_000 {
            return Err(MpcError::Invalid("Signed PSBT too large"));
        }
        let original = Psbt::from_str(journal.original_psbt.as_deref().ok_or(MpcError::Internal)?)
            .map_err(|_| MpcError::Internal)?;
        let signed =
            Psbt::from_str(&signed_psbt).map_err(|_| MpcError::Invalid("Invalid signed PSBT"))?;
        // Legacy journals are checked against the saved wallet DB, never the
        // signer-supplied witness_utxo. New journals retain the pre-send snapshot.
        let snapshot = match &journal.input_snapshot {
            Some(snapshot) => snapshot.clone(),
            None => self
                .open(&record)?
                .wallet
                .list_unspents(None, false, true)?,
        };
        let signed = plan(
            &record,
            original,
            &snapshot,
            journal.input_snapshot.is_some(),
            &request_asset(&journal.request)?,
            self.network,
            &journal.policy,
        )?
        .finalize_psbt(&signed)?;
        let online = self.send_online(&record)?;
        journal.submission_started = true;
        journal.view.state = "SUBMITTING".into();
        journal.view.psbt = None;
        self.save_send(&journal)?;
        let result = self
            .open(&record)?
            .wallet
            .send_end(online, signed.to_string())?;
        if Some(result.txid) != journal.view.txid
            || Some(result.batch_transfer_idx) != journal.view.batch_transfer_idx
        {
            return Err(MpcError::Conflict("Send result differs from approved PSBT"));
        }
        journal.view.state = "WAITING_COUNTERPARTY".into();
        self.save_send(&journal)?;
        Ok(journal.view)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rgb_lib::bitcoin::{
        Amount, Network, OutPoint, ScriptBuf, Transaction, TxIn, TxOut, Txid, absolute,
        hashes::Hash, transaction,
    };

    fn fixture() -> (Record, Psbt) {
        let key = CompressedPublicKey::from_str(
            "0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798",
        )
        .unwrap();
        let address = rgb_lib::bitcoin::Address::p2wpkh(&key, Network::Testnet);
        let record = Record {
            version: 1,
            owner: Owner {
                tenant_id: "test".into(),
                user_id: "alice".into(),
            },
            registration: Registration {
                wallet_id: Uuid::new_v4(),
                provider: Provider::DynamicEmbedded,
                provider_environment: "fixture".into(),
                provider_wallet_ref: "fixture".into(),
                bitcoin_network: "testnet".into(),
                genesis_hash: "fixture".into(),
                addresses: vec![RegisteredAddress {
                    role: Role::Rgb,
                    script_type: ScriptType::P2wpkh,
                    address: address.to_string(),
                    public_key: key.to_string(),
                    signing_key_id: "fixture".into(),
                }],
            },
            invoices: BTreeMap::new(),
        };
        let mut psbt = Psbt::from_unsigned_tx(Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: vec![TxIn {
                previous_output: OutPoint {
                    txid: Txid::from_byte_array([1; 32]),
                    vout: 0,
                },
                ..Default::default()
            }],
            output: vec![
                TxOut {
                    value: Amount::ZERO,
                    script_pubkey: ScriptBuf::new_op_return([2; 32]),
                },
                TxOut {
                    value: Amount::from_sat(692),
                    script_pubkey: address.script_pubkey(),
                },
            ],
        })
        .unwrap();
        psbt.inputs[0].witness_utxo = Some(TxOut {
            value: Amount::from_sat(1000),
            script_pubkey: address.script_pubkey(),
        });
        (record, psbt)
    }

    fn unspents() -> Vec<Unspent> {
        vec![Unspent {
            utxo: rgb_lib::wallet::Utxo {
                outpoint: rgb_lib::wallet::Outpoint {
                    txid: Txid::from_byte_array([1; 32]).to_string(),
                    vout: 0,
                },
                btc_amount: 1000,
                colorable: true,
                exists: true,
                derivation_index: None,
            },
            rgb_allocations: vec![],
            pending_blinded: 0,
        }]
    }

    fn plan(record: &Record, psbt: Psbt) -> Result<PreparedP2wpkh> {
        super::plan(
            record,
            psbt,
            &unspents(),
            true,
            ASSET,
            BitcoinNetwork::Testnet,
            &MpcSendPolicy::default(),
        )
    }

    fn invoice(
        request: &SendRequest,
        endpoints: &[String],
    ) -> Result<rgb_lib::wallet::InvoiceData> {
        super::invoice(
            request,
            endpoints,
            BitcoinNetwork::Testnet,
            &MpcSendPolicy::default(),
        )
    }

    #[test]
    fn send_invoice_and_psbt_validation_use_the_selected_network() {
        let networks = [
            BitcoinNetwork::Mainnet,
            BitcoinNetwork::Testnet,
            BitcoinNetwork::Testnet4,
            BitcoinNetwork::Signet,
            BitcoinNetwork::Regtest,
        ];
        let endpoint = "rpc://proxy.example/json-rpc";
        for network in networks {
            let chain = rgb_lib::ChainNet::from(network);
            let prefix = chain.prefix();
            let request = SendRequest {
                request_id: Uuid::new_v4(),
                invoice: format!(
                    "{ASSET}/~/ae/{prefix}:utxob:7TjnbTy5-H~OndMD-98vlg7F-Fp1VnAt-8J5Ju_P-F~X03DP-kVVUC?assignment_name=assetOwner&expiry=2000000000&endpoints={endpoint}"
                ),
                amount: "1".into(),
            };
            for selected in networks {
                assert_eq!(
                    super::invoice(
                        &request,
                        &[endpoint.into()],
                        selected,
                        &MpcSendPolicy::default()
                    )
                    .is_ok(),
                    selected == network
                );
            }
            let (mut record, psbt) = fixture();
            let address = &mut record.registration.addresses[0];
            let key = CompressedPublicKey::from_str(&address.public_key).unwrap();
            address.address =
                rgb_lib::bitcoin::Address::p2wpkh(&key, Network::from(network)).to_string();
            record.registration.bitcoin_network = network.to_string().to_ascii_lowercase();
            assert_eq!(
                super::plan(
                    &record,
                    psbt,
                    &unspents(),
                    true,
                    ASSET,
                    network,
                    &MpcSendPolicy::default()
                )
                .unwrap()
                .fee_sat(),
                308
            );
        }
    }

    #[test]
    fn return_inputs_are_checked_against_the_saved_asset_including_bfa() {
        let (record, psbt) = fixture();
        let mut inputs = unspents();
        inputs[0]
            .rgb_allocations
            .push(rgb_lib::wallet::RgbAllocation {
                asset_id: Some("bfa-fixture".into()),
                assignment: Assignment::Fungible(5),
                settled: true,
            });
        assert!(
            super::plan(
                &record,
                psbt.clone(),
                &inputs,
                true,
                "bfa-fixture",
                BitcoinNetwork::Testnet,
                &MpcSendPolicy::default()
            )
            .is_ok()
        );
        assert!(
            super::plan(
                &record,
                psbt.clone(),
                &inputs,
                true,
                ASSET,
                BitcoinNetwork::Testnet,
                &MpcSendPolicy::default()
            )
            .is_err()
        );
        inputs[0].rgb_allocations[0].settled = false;
        assert!(
            super::plan(
                &record,
                psbt,
                &inputs,
                true,
                "bfa-fixture",
                BitcoinNetwork::Testnet,
                &MpcSendPolicy::default()
            )
            .is_err()
        );
    }

    #[test]
    fn saved_return_invoice_pins_asset_and_still_checks_amount_expiry_and_transport() {
        let recipient = "tb3:utxob:7TjnbTy5-H~OndMD-98vlg7F-Fp1VnAt-8J5Ju_P-F~X03DP-kVVUC";
        let endpoint = "rpcs://proxy.iriswallet.com/0.2/json-rpc";
        let make = |asset: &str| {
            format!(
                "{asset}/~/ae/{recipient}?assignment_name=assetOwner&expiry=2000000000&endpoints={endpoint}"
            )
        };
        let mut request = SendRequest {
            request_id: Uuid::new_v4(),
            invoice: make(ASSET),
            amount: "1".into(),
        };
        assert_eq!(request_asset(&request).unwrap(), ASSET);
        assert!(invoice(&request, &[endpoint.into()]).is_ok());
        assert!(invoice(&request, &["rpc://wrong.invalid".into()]).is_err());
        request.amount = "2".into();
        assert!(invoice(&request, &[endpoint.into()]).is_err());
        request.amount = "1".into();
        request.invoice = make("rgb:~");
        assert!(request_asset(&request).is_err());
        assert!(invoice(&request, &[endpoint.into()]).is_err());
    }

    #[test]
    fn return_only_accepts_own_change_and_a_bounded_rgb_commitment() {
        let (record, psbt) = fixture();
        assert_eq!(plan(&record, psbt.clone()).unwrap().fee_sat(), 308);
        let mut altered = psbt.clone();
        altered.unsigned_tx.output[1].script_pubkey = ScriptBuf::new();
        assert!(plan(&record, altered).is_err());
        let mut altered = psbt.clone();
        altered.unsigned_tx.output.swap(0, 1);
        assert!(plan(&record, altered).is_err());
        let mut altered = psbt.clone();
        altered.unsigned_tx.output[1].value = Amount::from_sat(1);
        assert!(plan(&record, altered).is_err());
        let mut altered = psbt.clone();
        altered.inputs[0].witness_utxo.as_mut().unwrap().value = Amount::from_sat(10_000);
        assert!(plan(&record, altered).is_err());
        let mut altered = psbt;
        altered.inputs[0].sighash_type = Some(rgb_lib::bitcoin::EcdsaSighashType::None.into());
        assert!(plan(&record, altered).is_err());
    }

    #[test]
    fn amounts_must_be_canonical_and_bounded_before_parsing_the_invoice() {
        for amount in ["0", "01", "26", "-1", "1.5", "9999999999999999999999999"] {
            let request = SendRequest {
                request_id: Uuid::new_v4(),
                invoice: "not-an-invoice".into(),
                amount: amount.into(),
            };
            assert!(matches!(invoice(&request, &[]), Err(MpcError::Invalid(_))));
        }
    }
}

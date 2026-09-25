//! Provider identity does not select signing or transaction behavior. Retain this
//! module when removing a provider integration.
use super::*;
use crate::taproot_signing::{PreparedTaproot, VerifiedTaprootInput};
use crate::wallet::MpcSendPolicy;
use rgb_lib::{
    TransferStatus,
    wallet::{Invoice, Recipient, Unspent},
};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SendProfile {
    P2trTwoRoleBlindV1,
}
fn send_address(registration: &Registration) -> Result<&RegisteredAddress> {
    if registration.addresses.len() != 2
        || registration
            .addresses
            .iter()
            .filter(|a| a.role == Role::Fee)
            .count()
            != 1
    {
        return Err(MpcError::Invalid("Two Taproot role addresses required"));
    }
    registration
        .addresses
        .iter()
        .find(|a| a.role == Role::Rgb)
        .ok_or(MpcError::Invalid("RGB role required"))
}
pub(super) fn supported_profile(registration: &Registration) -> Option<SendProfile> {
    send_address(registration)
        .ok()
        .map(|_| SendProfile::P2trTwoRoleBlindV1)
}
#[derive(Clone, Serialize, Deserialize)]
pub struct KeyGroup {
    pub signing_key_id: String,
    pub input_indexes: Vec<usize>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct ChangeOutput {
    pub role: Role,
    pub vout: usize,
    pub amount_sat: String,
}
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct InputSnapshot {
    pub(super) rgb: Vec<Unspent>,
    pub(super) prevouts: Vec<(rgb_lib::bitcoin::OutPoint, rgb_lib::bitcoin::TxOut)>,
}
pub(super) fn groups(
    record: &Record,
    psbt: &Psbt,
    network: BitcoinNetwork,
) -> Result<Vec<KeyGroup>> {
    let mut result = BTreeMap::<String, Vec<usize>>::new();
    for (index, input) in psbt.inputs.iter().enumerate() {
        let prevout = input.witness_utxo.as_ref().ok_or(MpcError::Internal)?;
        let address = record
            .registration
            .addresses
            .iter()
            .find(|a| {
                checked_address(a, network)
                    .is_ok_and(|a| a.script_pubkey() == prevout.script_pubkey)
            })
            .ok_or(MpcError::Invalid("Unregistered input key"))?;
        result
            .entry(address.signing_key_id.clone())
            .or_default()
            .push(index);
    }
    Ok(result
        .into_iter()
        .map(|(signing_key_id, input_indexes)| KeyGroup {
            signing_key_id,
            input_indexes,
        })
        .collect())
}
fn change_outputs(
    record: &Record,
    psbt: &Psbt,
    network: BitcoinNetwork,
) -> Result<Vec<ChangeOutput>> {
    psbt.unsigned_tx
        .output
        .iter()
        .enumerate()
        .skip(1)
        .map(|(vout, output)| {
            let address = record
                .registration
                .addresses
                .iter()
                .find(|a| {
                    checked_address(a, network)
                        .is_ok_and(|a| a.script_pubkey() == output.script_pubkey)
                })
                .ok_or(MpcError::Invalid("Unregistered change"))?;
            Ok(ChangeOutput {
                role: address.role,
                vout,
                amount_sat: output.value.to_sat().to_string(),
            })
        })
        .collect()
}
// Apply the dev output policy only to new preparation. Reconciliation still
// validates the exact saved transaction, including older split-change journals.
fn check_new_change(record: &Record, psbt: &Psbt, network: BitcoinNetwork) -> Result<()> {
    let changes = change_outputs(record, psbt, network)?;
    if changes.len() > 1 || changes.iter().any(|change| change.role != Role::Fee) {
        return Err(MpcError::Invalid(
            "Expected a single Internal change output",
        ));
    }
    Ok(())
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SendRequest {
    pub request_id: Uuid,
    pub invoice: String,
    pub amount: String,
    pub asset_id: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct SendView {
    pub version: u32,
    pub asset_id: String,
    pub key_groups: Vec<KeyGroup>,
    pub change: Vec<ChangeOutput>,
    pub wallet_id: Uuid,
    pub request_id: Uuid,
    pub state: String,
    pub message: Option<String>,
    pub expires_at: Option<u64>,
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
    signed_psbt: Option<String>,
    #[serde(default)]
    prior_batch_ids: Option<Vec<i32>>,
    #[serde(default)]
    input_snapshot: Option<InputSnapshot>,
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
    preparing: bool,
    supported: &[rgb_lib::AssetSchema],
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
    let minimum_expiry = now()?.saturating_add(if preparing {
        policy.min_invoice_validity_secs
    } else {
        0
    });
    // The schema is checked while preparing. Finishing re-reads the journal's own
    // invoice, so re-checking it against current configuration would only strand
    // a saved operation if the supported schemas later changed.
    let schema_allowed = !preparing
        || data
            .asset_schema
            .is_some_and(|schema| supported.contains(&schema));
    if data.asset_id.as_deref() != Some(request.asset_id.as_str())
        || !schema_allowed
        || data.network != network
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
    mut psbt: Psbt,
    snapshot: &InputSnapshot,
    require_settled: bool,
    _asset_id: &str,
    network: BitcoinNetwork,
    policy: &MpcSendPolicy,
) -> Result<PreparedTaproot> {
    policy.validate().map_err(MpcError::Invalid)?;
    send_address(&record.registration)?;
    if psbt.inputs.is_empty()
        || psbt.inputs.len() > policy.max_inputs
        || !(1..=3).contains(&psbt.unsigned_tx.output.len())
        || !psbt.unsigned_tx.output[0].script_pubkey.is_op_return()
        || psbt.unsigned_tx.output[0].script_pubkey.len() != 34
        || psbt.unsigned_tx.output[0].value.to_sat() != 0
    {
        return Err(MpcError::Invalid("Unexpected RGB transaction shape"));
    }
    let changes = change_outputs(record, &psbt, network)?;
    let mut roles = HashSet::new();
    for change in &changes {
        let output = &psbt.unsigned_tx.output[change.vout];
        if !roles.insert(change.role)
            || output.value < output.script_pubkey.minimal_non_dust()
            || (change.role == Role::Rgb && output.value.to_sat() != policy.carrier_sat)
        {
            return Err(MpcError::Invalid("Change differs from saved policy"));
        }
    }
    let mut verified = Vec::new();
    for (index, input) in psbt.unsigned_tx.input.iter().enumerate() {
        let (_, prevout) = snapshot
            .prevouts
            .iter()
            .find(|(op, _)| *op == input.previous_output)
            .ok_or(MpcError::Conflict("Input differs from indexed prevouts"))?;
        let binding = record
            .registration
            .addresses
            .iter()
            .find(|a| {
                checked_address(a, network)
                    .is_ok_and(|a| a.script_pubkey() == prevout.script_pubkey)
            })
            .ok_or(MpcError::Invalid("Unregistered input"))?;
        let rgb = snapshot.rgb.iter().find(|u| {
            u.utxo.exists
                && u.utxo.outpoint.txid == input.previous_output.txid.to_string()
                && u.utxo.outpoint.vout == input.previous_output.vout
        });
        if binding.role == Role::Rgb && rgb.is_none() {
            return Err(MpcError::Conflict("Colored input absent from RGB state"));
        }
        // Internal change can carry RGB too. Its address role is not proof
        // that it is an asset-free BTC input.
        if let Some(unspent) = rgb
            && (unspent.utxo.btc_amount != prevout.value.to_sat()
                || (require_settled
                    && (unspent.pending_blinded != 0
                        || unspent.rgb_allocations.iter().any(|a| !a.settled))))
        {
            return Err(MpcError::Conflict(
                "Colored input is pending or differs from node",
            ));
        }
        psbt.inputs[index].tap_internal_key =
            Some(XOnlyPublicKey::from_str(&binding.internal_key).map_err(|_| MpcError::Internal)?);
        verified.push(VerifiedTaprootInput {
            outpoint: input.previous_output,
            prevout: prevout.clone(),
            output_key: XOnlyPublicKey::from_str(&binding.public_key)
                .map_err(|_| MpcError::Internal)?,
        });
    }
    Ok(PreparedTaproot::prepare(
        psbt,
        &verified,
        policy.max_fee_sat,
    )?)
}
pub(super) fn add_internal_keys(
    record: &Record,
    psbt: &mut Psbt,
    network: BitcoinNetwork,
) -> Result<()> {
    for input in &mut psbt.inputs {
        let output = input.witness_utxo.as_ref().ok_or(MpcError::Internal)?;
        let binding = record
            .registration
            .addresses
            .iter()
            .find(|a| {
                checked_address(a, network).is_ok_and(|a| a.script_pubkey() == output.script_pubkey)
            })
            .ok_or(MpcError::Internal)?;
        input.tap_internal_key =
            Some(XOnlyPublicKey::from_str(&binding.internal_key).map_err(|_| MpcError::Internal)?);
    }
    Ok(())
}

impl MpcService {
    pub async fn prepare_send(
        &self,
        owner: Owner,
        id: Uuid,
        request: SendRequest,
    ) -> Result<SendView> {
        self.call(id, move |manager| {
            manager.owned(&owner, id)?;
            let result = manager.prepare_send(&owner, id, request.clone());
            let message = match &result {
                Err(MpcError::Invalid(message)) => Some((*message).to_owned()),
                Err(MpcError::Rgb(error)) if crate::wallet::classify(error).is_client_error() => Some(match crate::wallet::classify(error) {
                    crate::wallet::ErrorClass::NotEnoughBalance => "Insufficient eligible Bitcoin for this transfer",
                    crate::wallet::ErrorClass::NotEnoughAssets => "Insufficient spendable RGB assets",
                    _ => "Invoice, amount, asset or wallet state does not permit this transfer",
                }.to_owned()),
                _ => None,
            };
            // Under the wallet lock, an error with no saved mutation intent is
            // definitively unprepared. Persist a tombstone before reporting it.
            if let Some(message) = message
                && !manager.send_path(id, request.request_id).try_exists()? {
                let journal = Journal {
                    view: SendView { version: 1, asset_id: request.asset_id.clone(), wallet_id: id,
                        request_id: request.request_id, state: "FAILED".into(), message: Some(message), expires_at: None,
                        amount: request.amount.clone(), invoice: request.invoice.clone(), psbt: None,
                        txid: None, fee_sat: None, input_indexes: vec![], batch_transfer_idx: None,
                        key_groups: vec![], change: vec![] },
                    request, original_psbt: None, signed_psbt: None, prior_batch_ids: None,
                    input_snapshot: None, submission_started: false, policy: manager.config.mpc_send.clone(),
                };
                manager.save_send(&journal)?;
                return Ok(journal.view);
            }
            result
        }).await
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
    pub(super) fn send_online(&mut self, record: &Record) -> Result<Online> {
        let options = OnlineOptions {
            indexer_url: self.config.indexer_address.clone(),
            skip_consistency_check: false,
            vanilla_sync_lookback: 20,
            eth_rpc_url: self.config.eth_rpc(),
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
        let mut original =
            Psbt::from_str(&fs::read_to_string(path)?).map_err(|_| MpcError::Internal)?;
        add_internal_keys(record, &mut original, self.network)?;
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
        journal.view.input_indexes = (0..original.inputs.len()).collect();
        journal.view.key_groups = groups(record, &original, self.network)?;
        journal.view.change = change_outputs(record, &original, self.network)?;
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
        // Replay only the previously verified, persisted transaction. rgb-lib handles
        // an identical already-posted consignment; no new prepare or txid is created.
        if journal.view.state == "SUBMITTING" {
            let transfers = self
                .open(record)?
                .wallet
                .list_transfers(AssetFilter::Id(request_asset(&journal.request)?), None)?;
            if transfers.iter().any(|t| {
                Some(t.batch_transfer_idx) == journal.view.batch_transfer_idx
                    && t.txid == journal.view.txid
                    && t.status == TransferStatus::Initiated
            }) {
                let signed = journal.signed_psbt.clone().ok_or(MpcError::Conflict(
                    "Missing saved signature; submission needs review",
                ))?;
                let online = self.send_online(record)?;
                let result = self.open(record)?.wallet.send_end(online, signed)?;
                if Some(result.txid) != journal.view.txid
                    || Some(result.batch_transfer_idx) != journal.view.batch_transfer_idx
                {
                    return Err(MpcError::Conflict(
                        "Recovered submission differs from saved transaction",
                    ));
                }
            }
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
        let data = invoice(
            &request,
            &self.config.proxy_address,
            network,
            &policy,
            true,
            &self.config.supported_schemas()?,
        )?;
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
        let unspents = InputSnapshot {
            rgb: wallet.list_unspents(Some(online), false, false)?,
            prevouts: wallet.list_mpc_unspents(online)?,
        };
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
        let dry = Psbt::from_str(&dry.psbt).map_err(|_| MpcError::Internal)?;
        check_new_change(&record, &dry, network)?;
        plan(&record, dry, &unspents, true, &asset_id, network, &policy)?;
        let prior_batch_ids = wallet
            .list_transfers(AssetFilter::Id(asset_id.clone()), None)?
            .iter()
            .map(|transfer| transfer.batch_transfer_idx)
            .collect();
        let mut journal = Journal {
            view: SendView {
                version: 1,
                asset_id: asset_id.clone(),
                key_groups: vec![],
                change: vec![],
                wallet_id: id,
                request_id: request.request_id,
                state: "PREPARING".into(),
                message: None,
                expires_at: Some(expiry),
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
            signed_psbt: None,
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
        let mut original = Psbt::from_str(&begun.psbt).map_err(|_| MpcError::Internal)?;
        add_internal_keys(&record, &mut original, network)?;
        check_new_change(&record, &original, network)?;
        let prepared = plan(
            &record,
            original.clone(),
            &unspents,
            true,
            &asset_id,
            network,
            &policy,
        )?;
        journal.original_psbt = Some(original.to_string());
        journal.view.psbt = journal.original_psbt.clone();
        journal.view.txid = Some(original.unsigned_tx.compute_txid().to_string());
        journal.view.fee_sat = Some(prepared.fee_sat());
        journal.view.input_indexes = (0..original.inputs.len()).collect();
        journal.view.key_groups = groups(&record, &original, self.network)?;
        journal.view.change = change_outputs(&record, &original, self.network)?;
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
            false,
            &self.config.supported_schemas()?,
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
            None => return Err(MpcError::Conflict("Saved input snapshot required")),
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
        journal.signed_psbt = Some(signed.to_string());
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

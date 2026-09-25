//! Delegated MPC pool preparation using the library's existing begin/end flow.
use super::send::{ChangeOutput, InputSnapshot, KeyGroup, add_internal_keys, groups};
use super::*;
use crate::{
    taproot_signing::{PreparedTaproot, VerifiedTaprootInput},
    wallet::MpcSendPolicy,
};

fn parse_psbt(value: &str) -> Result<Psbt> {
    Psbt::from_str(value).map_err(|_| MpcError::Invalid("Invalid PSBT"))
}

fn default_num() -> u8 {
    5
}
fn default_size() -> u32 {
    1000
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateUtxosRequest {
    pub request_id: Uuid,
    #[serde(default = "default_num")]
    pub num: u8,
    #[serde(default = "default_size")]
    pub size: u32,
    #[serde(default)]
    pub up_to: bool,
}
impl CreateUtxosRequest {
    fn validate(&self) -> Result<()> {
        // Keep the existing estimator below the output-count varint boundary.
        if self.num == 0 || self.num > 251 || self.size < 330 {
            return Err(MpcError::Invalid(
                "Expected 1–251 UTXOs of at least 330 sat",
            ));
        }
        Ok(())
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct CreateUtxosView {
    pub version: u32,
    pub wallet_id: Uuid,
    pub request_id: Uuid,
    pub state: String,
    pub message: Option<String>,
    pub num: u8,
    pub size: u32,
    pub up_to: bool,
    pub created: Option<u8>,
    pub psbt: Option<String>,
    pub txid: Option<String>,
    pub fee_sat: Option<u64>,
    pub key_groups: Vec<KeyGroup>,
    /// All outputs, including each individual External UTXO.
    pub outputs: Vec<ChangeOutput>,
}
#[derive(Serialize, Deserialize)]
struct Journal {
    request: CreateUtxosRequest,
    view: CreateUtxosView,
    original_psbt: Option<String>,
    signed_psbt: Option<String>,
    snapshot: Option<InputSnapshot>,
    prior_pending: Vec<String>,
    policy: MpcSendPolicy,
}
fn terminal(state: &str) -> bool {
    matches!(state, "COMPLETED" | "CANCELLED" | "FAILED")
}
fn outputs(record: &Record, psbt: &Psbt, network: BitcoinNetwork) -> Result<Vec<ChangeOutput>> {
    psbt.unsigned_tx
        .output
        .iter()
        .enumerate()
        .map(|(vout, output)| {
            let binding = record
                .registration
                .addresses
                .iter()
                .find(|a| {
                    checked_address(a, network)
                        .is_ok_and(|a| a.script_pubkey() == output.script_pubkey)
                })
                .ok_or(MpcError::Invalid("Unregistered pool output"))?;
            Ok(ChangeOutput {
                role: binding.role,
                vout,
                amount_sat: output.value.to_sat().to_string(),
            })
        })
        .collect()
}
fn plan(
    record: &Record,
    mut psbt: Psbt,
    snapshot: &InputSnapshot,
    request: &CreateUtxosRequest,
    network: BitcoinNetwork,
    policy: &MpcSendPolicy,
) -> Result<PreparedTaproot> {
    request.validate()?;
    policy.validate().map_err(MpcError::Invalid)?;
    let outputs = outputs(record, &psbt, network)?;
    let pool_count = outputs.iter().take_while(|o| o.role == Role::Rgb).count();
    if psbt.inputs.is_empty()
        || psbt.inputs.len() > policy.max_inputs
        || pool_count == 0
        || pool_count > request.num as usize
        || outputs.len() > pool_count + 1
        || outputs[..pool_count]
            .iter()
            .any(|o| o.amount_sat != request.size.to_string())
        || outputs[pool_count..]
            .iter()
            .any(|o| o.role != Role::Fee || o.amount_sat.parse::<u64>().unwrap_or(0) <= 330)
    {
        return Err(MpcError::Invalid("Unexpected MPC pool transaction shape"));
    }
    let mut verified = Vec::new();
    for input in &psbt.unsigned_tx.input {
        let (_, prevout) = snapshot
            .prevouts
            .iter()
            .find(|(op, _)| *op == input.previous_output)
            .ok_or(MpcError::Conflict(
                "Pool input absent from indexed prevouts",
            ))?;
        if snapshot.rgb.iter().any(|u| {
            u.utxo.outpoint.txid == input.previous_output.txid.to_string()
                && u.utxo.outpoint.vout == input.previous_output.vout
        }) {
            return Err(MpcError::Invalid("RGB inputs cannot fund UTXO preparation"));
        }
        let binding = record
            .registration
            .addresses
            .iter()
            .find(|a| {
                a.role == Role::Fee
                    && checked_address(a, network)
                        .is_ok_and(|a| a.script_pubkey() == prevout.script_pubkey)
            })
            .ok_or(MpcError::Invalid(
                "Pool requires registered Internal funding inputs",
            ))?;
        verified.push(VerifiedTaprootInput {
            outpoint: input.previous_output,
            prevout: prevout.clone(),
            output_key: XOnlyPublicKey::from_str(&binding.public_key)
                .map_err(|_| MpcError::Internal)?,
        });
    }
    add_internal_keys(record, &mut psbt, network)?;
    Ok(PreparedTaproot::prepare(
        psbt,
        &verified,
        policy.max_fee_sat,
    )?)
}
impl MpcService {
    /// UI hint; blind_receive remains authoritative for allocation and reservation checks.
    pub async fn has_receive_utxo(&self, owner: Owner, id: Uuid) -> Result<bool> {
        self.call(id, move |manager| {
            let record = manager.owned(&owner, id)?;
            let online = manager.send_online(&record)?;
            let unspents =
                manager
                    .open(&record)?
                    .wallet
                    .list_unspents(Some(online), false, false)?;
            Ok(unspents.iter().any(|u| {
                u.utxo.colorable
                    && u.utxo.exists
                    && u.rgb_allocations.len() as u32 + u.pending_blinded < 5
                    && !u
                        .rgb_allocations
                        .iter()
                        .any(|a| a.assignment == Assignment::LinkRight)
            }))
        })
        .await
    }
    pub async fn prepare_utxos(
        &self,
        owner: Owner,
        id: Uuid,
        request: CreateUtxosRequest,
    ) -> Result<CreateUtxosView> {
        self.call(id, move |manager| {
            manager.prepare_utxos(&owner, id, request)
        })
        .await
    }
    pub async fn utxos_status(
        &self,
        owner: Owner,
        id: Uuid,
        request_id: Uuid,
    ) -> Result<CreateUtxosView> {
        self.call(id, move |manager| {
            let record = manager.owned(&owner, id)?;
            let mut journal = manager.read_utxos(id, request_id)?;
            manager.reconcile_utxos(&record, &mut journal)?;
            Ok(journal.view)
        })
        .await
    }
    pub async fn finish_utxos(
        &self,
        owner: Owner,
        id: Uuid,
        request_id: Uuid,
        signed: String,
    ) -> Result<CreateUtxosView> {
        self.call(id, move |manager| {
            let record = manager.owned(&owner, id)?;
            let mut journal = manager.read_utxos(id, request_id)?;
            manager.reconcile_utxos(&record, &mut journal)?;
            if terminal(&journal.view.state) {
                return Ok(journal.view);
            }
            if journal.view.state != "AWAITING_SIGNATURE" {
                return Err(MpcError::Conflict("Reconcile the saved pool preparation"));
            }
            if signed.len() > 128_000 {
                return Err(MpcError::Invalid("Signed PSBT too large"));
            }
            let original = parse_psbt(journal.original_psbt.as_deref().ok_or(MpcError::Internal)?)?;
            let prepared = plan(
                &record,
                original,
                journal.snapshot.as_ref().ok_or(MpcError::Internal)?,
                &journal.request,
                manager.network,
                &journal.policy,
            )?;
            journal.signed_psbt = Some(prepared.finalize_psbt(&parse_psbt(&signed)?)?.to_string());
            journal.view.state = "SUBMITTING".into();
            journal.view.psbt = None;
            manager.save_utxos(&journal)?;
            manager.reconcile_utxos(&record, &mut journal)?;
            Ok(journal.view)
        })
        .await
    }
    pub async fn cancel_utxos(
        &self,
        owner: Owner,
        id: Uuid,
        request_id: Uuid,
    ) -> Result<CreateUtxosView> {
        self.call(id, move |manager| {
            let record = manager.owned(&owner, id)?;
            let mut journal = manager.read_utxos(id, request_id)?;
            if terminal(&journal.view.state) {
                return Ok(journal.view);
            }
            if journal.signed_psbt.is_some() {
                return Err(MpcError::Conflict(
                    "Broadcast may have started; resume the saved transaction",
                ));
            }
            manager.reconcile_utxos(&record, &mut journal)?;
            if terminal(&journal.view.state) {
                return Ok(journal.view);
            }
            if journal.view.state != "AWAITING_SIGNATURE" {
                return Err(MpcError::Conflict("Reconcile the saved pool preparation"));
            }
            journal.view.state = "CANCELLING".into();
            journal.view.psbt = None;
            manager.save_utxos(&journal)?;
            manager.reconcile_utxos(&record, &mut journal)?;
            Ok(journal.view)
        })
        .await
    }
}
impl Manager {
    fn utxos_path(&self, id: Uuid, request: Uuid) -> PathBuf {
        self.root
            .join("utxo-preparations")
            .join(id.to_string())
            .join(format!("{request}.json"))
    }
    fn read_utxos(&self, id: Uuid, request: Uuid) -> Result<Journal> {
        let bytes = fs::read(self.utxos_path(id, request)).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                MpcError::NotFound
            } else {
                MpcError::Internal
            }
        })?;
        let journal: Journal = serde_json::from_slice(&bytes)?;
        if journal.view.wallet_id != id
            || journal.view.request_id != request
            || journal.request.request_id != request
        {
            return Err(MpcError::Internal);
        }
        Ok(journal)
    }
    fn save_utxos(&self, journal: &Journal) -> Result<()> {
        let path = self.utxos_path(journal.view.wallet_id, journal.request.request_id);
        let dir = path.parent().ok_or(MpcError::Internal)?;
        private_directory(dir)?;
        let mut file = tempfile::NamedTempFile::new_in(dir)?;
        file.write_all(&serde_json::to_vec(journal)?)?;
        file.as_file().sync_all()?;
        file.persist(&path).map_err(|_| MpcError::Internal)?;
        fs::File::open(dir)?.sync_all()?;
        Ok(())
    }
    fn reconcile_utxos(&mut self, record: &Record, journal: &mut Journal) -> Result<()> {
        if terminal(&journal.view.state) {
            return Ok(());
        }
        if journal.view.state == "SUBMITTING" {
            let signed = journal.signed_psbt.clone().ok_or(MpcError::Internal)?;
            let online = self.send_online(record)?;
            let count = self.open(record)?.wallet.create_utxos_end(online, signed)?;
            if count as usize
                != journal
                    .view
                    .outputs
                    .iter()
                    .filter(|o| o.role == Role::Rgb)
                    .count()
            {
                return Err(MpcError::Conflict(
                    "Created pool differs from approved transaction",
                ));
            }
            journal.view.created = Some(count);
            journal.view.state = "COMPLETED".into();
        } else if ["PREPARING", "CANCELLING"].contains(&journal.view.state.as_str()) {
            let pending = self.open(record)?.wallet.list_pending_vanilla_txs()?;
            let txid = journal.view.txid.as_ref().ok_or(MpcError::Internal)?;
            let reserved = pending.iter().any(|p| {
                p.txid == *txid && p.r#type == rgb_lib::WalletTransactionType::CreateUtxos
            });
            if journal.view.state == "CANCELLING" {
                if reserved {
                    self.open(record)?
                        .wallet
                        .abort_pending_vanilla_tx(txid.clone())?;
                }
                journal.view.state = "CANCELLED".into();
            } else if reserved {
                journal.view.state = "AWAITING_SIGNATURE".into();
                journal.view.psbt = journal.original_psbt.clone();
            } else if pending
                .iter()
                .any(|p| !journal.prior_pending.contains(&p.txid))
            {
                journal.view.state = "NEEDS_REVIEW".into();
                journal.view.message = Some("Pool preparation differs from the saved preview; preserve its reservations for reconciliation".into());
            } else {
                journal.view.state = "FAILED".into();
                journal.view.message =
                    Some("Preparation did not commit; no transaction was sent to a signer".into());
            }
        } else {
            return Ok(());
        }
        self.save_utxos(journal)
    }
    fn prepare_utxos(
        &mut self,
        owner: &Owner,
        id: Uuid,
        request: CreateUtxosRequest,
    ) -> Result<CreateUtxosView> {
        let record = self.owned(owner, id)?;
        if self.utxos_path(id, request.request_id).exists() {
            let mut journal = self.read_utxos(id, request.request_id)?;
            if journal.request != request {
                return Err(MpcError::Conflict(
                    "UTXO request ID already has another intent",
                ));
            }
            self.reconcile_utxos(&record, &mut journal)?;
            return Ok(journal.view);
        }
        let dir = self.root.join("utxo-preparations").join(id.to_string());
        private_directory(&dir)?;
        for file in fs::read_dir(dir)? {
            let path = file?.path();
            if path.extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            let mut journal =
                self.read_utxos(id, registration_file_id(&path).ok_or(MpcError::Internal)?)?;
            self.reconcile_utxos(&record, &mut journal)?;
            if !terminal(&journal.view.state) {
                return Err(MpcError::Conflict("Resume the pending UTXO preparation"));
            }
        }
        let mut journal = Journal {
            view: CreateUtxosView {
                version: 1,
                wallet_id: id,
                request_id: request.request_id,
                state: "FAILED".into(),
                message: None,
                num: request.num,
                size: request.size,
                up_to: request.up_to,
                created: None,
                psbt: None,
                txid: None,
                fee_sat: None,
                key_groups: vec![],
                outputs: vec![],
            },
            request,
            original_psbt: None,
            signed_psbt: None,
            snapshot: None,
            prior_pending: vec![],
            policy: self.config.mpc_send.clone(),
        };
        let preparation = (|| -> Result<()> {
            journal.request.validate()?;
            let online = self.send_online(&record)?;
            let wallet = &mut self.open(&record)?.wallet;
            let snapshot = InputSnapshot {
                rgb: wallet.list_unspents(Some(online), false, false)?,
                prevouts: wallet.list_mpc_unspents(online)?,
            };
            let mut dry = parse_psbt(&wallet.create_utxos_begin(
                online,
                journal.request.up_to,
                Some(journal.request.num),
                Some(journal.request.size),
                journal.policy.fee_rate_sat_vb,
                true,
                true,
            )?)?;
            add_internal_keys(&record, &mut dry, self.network)?;
            let prepared = plan(
                &record,
                dry.clone(),
                &snapshot,
                &journal.request,
                self.network,
                &journal.policy,
            )?;
            journal.view.txid = Some(dry.unsigned_tx.compute_txid().to_string());
            journal.view.fee_sat = Some(prepared.fee_sat());
            journal.view.outputs = outputs(&record, &dry, self.network)?;
            journal.view.key_groups = groups(&record, &dry, self.network)?;
            journal.original_psbt = Some(dry.to_string());
            journal.snapshot = Some(snapshot);
            journal.prior_pending = self
                .open(&record)?
                .wallet
                .list_pending_vanilla_txs()?
                .into_iter()
                .map(|p| p.txid)
                .collect();
            Ok(())
        })();
        if let Err(error) = preparation {
            if matches!(error, MpcError::Invalid(_))
                || matches!(&error, MpcError::Rgb(e) if crate::wallet::classify(e).is_client_error())
            {
                journal.view.message = Some("Invalid pool parameters, sufficient allocations already exist, or eligible Internal funding is insufficient".into());
                self.save_utxos(&journal)?;
                return Ok(journal.view);
            }
            return Err(error);
        }
        // Persist the exact preview before the library can commit reservations.
        journal.view.state = "PREPARING".into();
        self.save_utxos(&journal)?;
        let online = self.send_online(&record)?;
        let begun = self.open(&record)?.wallet.create_utxos_begin(
            online,
            journal.request.up_to,
            Some(journal.request.num),
            Some(journal.request.size),
            journal.policy.fee_rate_sat_vb,
            true,
            false,
        );
        self.reconcile_utxos(&record, &mut journal)?;
        if let Ok(begun) = begun {
            if Some(parse_psbt(&begun)?.unsigned_tx.compute_txid().to_string()) != journal.view.txid
            {
                return Err(MpcError::Conflict(
                    "Pool inputs changed during preparation; reconcile the saved operation",
                ));
            }
        } else if journal.view.state != "FAILED" {
            begun?;
        }
        Ok(journal.view)
    }
}

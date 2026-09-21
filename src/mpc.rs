//! Gateway-only, public-key MPC registry and witness receiving for the NIA POC.
//! A service credential authenticates the Gateway, which must verify provider
//! ownership before registering keys. This module never signs transactions.
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
    str::FromStr,
    sync::{Arc, Mutex},
    time::Instant,
};

use crate::wallet::Config;
use rgb_lib::{
    AssetSchema, Assignment, BitcoinNetwork,
    bdk_wallet::KeychainKind,
    bitcoin::{
        Address, CompressedPublicKey, Network, Psbt,
        blockdata::constants::genesis_block,
        hashes::{Hash, sha256},
        key::TweakedPublicKey,
        secp256k1::XOnlyPublicKey,
    },
    mpc::{MpcAddressInfo, MpcWalletProvider},
    wallet::{
        AssetFilter, Assets, DatabaseType, MpcWallet, Online, OnlineOptions, ReceiveData,
        RgbWalletOpsOffline, RgbWalletOpsOnline, Transfer, WalletData,
    },
};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use uuid::Uuid;

// DYNAMIC_EMBEDDED_POC BEGIN: optional Dynamic return adapter.
#[path = "mpc_send.rs"]
mod send;
pub use send::{SendRequest, SendView};
// DYNAMIC_EMBEDDED_POC END

#[derive(Debug)]
pub enum MpcError {
    Disabled,
    Unauthorized,
    NotFound,
    Invalid(&'static str),
    Conflict(&'static str),
    Internal,
    Rgb(rgb_lib::Error),
}
impl From<rgb_lib::Error> for MpcError {
    fn from(value: rgb_lib::Error) -> Self {
        Self::Rgb(value)
    }
}
impl From<std::io::Error> for MpcError {
    fn from(_: std::io::Error) -> Self {
        Self::Internal
    }
}
impl From<serde_json::Error> for MpcError {
    fn from(_: serde_json::Error) -> Self {
        Self::Internal
    }
}
impl std::fmt::Display for MpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for MpcError {}
type Result<T> = std::result::Result<T, MpcError>;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Owner {
    pub tenant_id: String,
    pub user_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    DynamicEmbedded,
    FireblocksVault,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Rgb,
    Fee,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScriptType {
    P2wpkh,
    P2tr,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegisteredAddress {
    pub role: Role,
    pub script_type: ScriptType,
    pub address: String,
    /// Compressed key for P2WPKH; tweaked x-only OUTPUT key for P2TR.
    pub public_key: String,
    pub signing_key_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registration {
    pub wallet_id: Uuid,
    pub provider: Provider,
    pub provider_environment: String,
    pub provider_wallet_ref: String,
    pub bitcoin_network: String,
    pub genesis_hash: String,
    pub addresses: Vec<RegisteredAddress>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WitnessRequest {
    pub request_id: Uuid,
    pub asset_id: Option<String>,
    /// Integer base units. None creates an unconstrained receive invoice.
    pub amount: Option<String>,
    pub expiration_timestamp: u64,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct WitnessInvoice {
    pub wallet_id: Uuid,
    pub request_id: Uuid,
    pub invoice: String,
    pub recipient_id: String,
    pub expiration_timestamp: u64,
    pub batch_transfer_idx: i32,
}

#[derive(Clone, Serialize, Deserialize)]
struct InvoiceRecord {
    request: WitnessRequest,
    result: Option<WitnessInvoice>,
    #[serde(default)]
    prior_batch_ids: Option<Vec<i32>>,
    #[serde(default)]
    failed_before_receive: bool,
}

#[derive(Clone, Serialize, Deserialize)]
struct Record {
    version: u32,
    owner: Owner,
    registration: Registration,
    invoices: BTreeMap<Uuid, InvoiceRecord>,
}

#[derive(Serialize)]
pub struct WalletView {
    pub wallet_id: Uuid,
    pub provider: Provider,
    pub bitcoin_network: String,
    pub genesis_hash: String,
    pub addresses: Vec<RegisteredAddress>,
    pub supported_schemas: Vec<String>,
    pub witness_receive: bool,
    pub signing: bool,
}

struct Entry {
    wallet: MpcWallet,
    online: Option<Online>,
}

// The registry lock covers registration metadata only. Each opened wallet has
// its own manager/lock; no network operation holds the registry or cache lock.
const MAX_OPEN_WALLETS: usize = 32;
struct WalletSlot {
    manager: Arc<Mutex<Manager>>,
    last_used: Instant,
}
struct ServiceState {
    registry: Mutex<Manager>,
    wallets: Mutex<BTreeMap<Uuid, WalletSlot>>,
}

#[derive(Clone)]
pub struct MpcService {
    token_hash: Option<[u8; 32]>,
    manager: Option<Arc<ServiceState>>,
}

impl MpcService {
    /// Refresh persisted registrations, including wallets not opened since a
    /// restart. Called by the background worker; no browser session is needed
    /// to accept an incoming transfer.
    pub async fn refresh_registered(&self) -> Result<usize> {
        if self.manager.is_none() {
            return Ok(0);
        }
        let registrations = self
            .call_registry(|manager| manager.registrations())
            .await?;
        let mut pending = tokio::task::JoinSet::new();
        let mut refreshed = 0;
        for (owner, id) in registrations {
            let service = self.clone();
            pending.spawn(async move {
                match service.refresh(owner, id).await {
                    Ok(()) => 1,
                    Err(_) => {
                        log::warn!("MPC receive refresh failed for {id}");
                        0
                    }
                }
            });
            if pending.len() >= 4 {
                refreshed += pending
                    .join_next()
                    .await
                    .ok_or(MpcError::Internal)?
                    .map_err(|_| MpcError::Internal)?;
            }
        }
        while let Some(result) = pending.join_next().await {
            refreshed += result.map_err(|_| MpcError::Internal)?;
        }
        Ok(refreshed)
    }

    pub async fn refresh_task(self, cancel: tokio_util::sync::CancellationToken) {
        if self.manager.is_none() {
            return;
        }
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = tokio::time::sleep(std::time::Duration::from_secs(10)) => {},
            }
            if let Err(error) = self.refresh_registered().await {
                log::warn!("MPC registry refresh unavailable: {error}");
            }
        }
    }

    pub fn new(config: Config, token: Option<String>) -> Result<Self> {
        let Some(token) = token else {
            return Ok(Self {
                token_hash: None,
                manager: None,
            });
        };
        if token.len() < 32 {
            return Err(MpcError::Invalid(
                "MPC service token must contain at least 32 bytes",
            ));
        }
        let network = config.net()?;
        if network == BitcoinNetwork::Mainnet {
            return Err(MpcError::Invalid("MPC POC requires a Bitcoin test network"));
        }
        let root = Path::new(&config.datadir()).join("mpc");
        private_directory(&root)?;
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(root.join("service.lock"))?;
        lock.try_lock()
            .map_err(|_| MpcError::Conflict("MPC data directory is already in use"))?;
        private_directory(&root.join("registrations"))?;
        private_directory(&root.join("wallets"))?;
        Ok(Self {
            token_hash: Some(sha256::Hash::hash(token.as_bytes()).to_byte_array()),
            manager: Some(Arc::new(ServiceState {
                registry: Mutex::new(Manager {
                    config,
                    network,
                    root,
                    entries: BTreeMap::new(),
                    _lock: Arc::new(lock),
                }),
                wallets: Mutex::new(BTreeMap::new()),
            })),
        })
    }

    pub fn authorize(&self, bearer: Option<&str>, owner: Owner) -> Result<Owner> {
        let expected = self.token_hash.ok_or(MpcError::Disabled)?;
        let token = bearer
            .and_then(|value| value.strip_prefix("Bearer "))
            .ok_or(MpcError::Unauthorized)?;
        let actual = sha256::Hash::hash(token.as_bytes()).to_byte_array();
        if !bool::from(actual.ct_eq(&expected)) {
            return Err(MpcError::Unauthorized);
        }
        if !identifier(&owner.tenant_id) || !identifier(&owner.user_id) {
            return Err(MpcError::Invalid(
                "Gateway tenant/user headers are required",
            ));
        }
        Ok(owner)
    }

    async fn call_registry<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Manager) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let manager = self.manager.clone().ok_or(MpcError::Disabled)?;
        tokio::task::spawn_blocking(move || {
            let mut guard = manager.registry.lock().map_err(|_| MpcError::Internal)?;
            operation(&mut guard)
        })
        .await
        .map_err(|_| MpcError::Internal)?
    }
    async fn call<T: Send + 'static>(
        &self,
        id: Uuid,
        operation: impl FnOnce(&mut Manager) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let state = self.manager.clone().ok_or(MpcError::Disabled)?;
        tokio::task::spawn_blocking(move || {
            let manager = {
                let mut slots = state.wallets.lock().map_err(|_| MpcError::Internal)?;
                if !slots.contains_key(&id) {
                    if slots.len() >= MAX_OPEN_WALLETS {
                        let idle = slots
                            .iter()
                            .filter(|(_, slot)| Arc::strong_count(&slot.manager) == 1)
                            .min_by_key(|(_, slot)| slot.last_used)
                            .map(|(id, _)| *id)
                            .ok_or(MpcError::Conflict(
                                "MPC wallet capacity is busy; retry the saved request",
                            ))?;
                        slots.remove(&idle);
                    }
                    let registry = state.registry.lock().map_err(|_| MpcError::Internal)?;
                    slots.insert(
                        id,
                        WalletSlot {
                            manager: Arc::new(Mutex::new(Manager {
                                config: registry.config.clone(),
                                network: registry.network,
                                root: registry.root.clone(),
                                entries: BTreeMap::new(),
                                _lock: registry._lock.clone(),
                            })),
                            last_used: Instant::now(),
                        },
                    );
                }
                let slot = slots.get_mut(&id).ok_or(MpcError::Internal)?;
                slot.last_used = Instant::now();
                slot.manager.clone()
            };
            let mut manager = manager.lock().map_err(|_| MpcError::Internal)?;
            operation(&mut manager)
        })
        .await
        .map_err(|_| MpcError::Internal)?
    }

    pub async fn register(&self, owner: Owner, request: Registration) -> Result<WalletView> {
        let id = request.wallet_id;
        let registration_owner = owner.clone();
        self.call_registry(move |manager| manager.register(registration_owner, request))
            .await?;
        self.wallet(owner, id).await
    }
    pub async fn wallet(&self, owner: Owner, id: Uuid) -> Result<WalletView> {
        self.call(id, move |manager| {
            let record = manager.owned(&owner, id)?;
            manager.open(&record)?;
            Ok(view(&record))
        })
        .await
    }
    pub async fn witness(
        &self,
        owner: Owner,
        id: Uuid,
        request: WitnessRequest,
    ) -> Result<WitnessInvoice> {
        self.call(id, move |manager| manager.witness(&owner, id, request))
            .await
    }
    pub async fn assets(&self, owner: Owner, id: Uuid) -> Result<Assets> {
        self.call(id, move |manager| {
            let record = manager.owned(&owner, id)?;
            Ok(manager
                .open(&record)?
                .wallet
                .list_assets(vec![AssetSchema::Nia])?)
        })
        .await
    }
    pub async fn transfers(&self, owner: Owner, id: Uuid) -> Result<Vec<Transfer>> {
        self.transfers_for_asset(owner, id, None).await
    }
    pub async fn transfers_for_asset(
        &self,
        owner: Owner,
        id: Uuid,
        asset_id: Option<String>,
    ) -> Result<Vec<Transfer>> {
        self.call(id, move |manager| {
            let record = manager.owned(&owner, id)?;
            let filter = asset_id
                .map(AssetFilter::Id)
                .unwrap_or(AssetFilter::AnyOrNone);
            Ok(manager.open(&record)?.wallet.list_transfers(filter, None)?)
        })
        .await
    }
    pub async fn refresh(&self, owner: Owner, id: Uuid) -> Result<()> {
        self.call(id, move |manager| {
            let record = manager.owned(&owner, id)?;
            let options = OnlineOptions {
                indexer_url: manager.config.indexer_address.clone(),
                skip_consistency_check: false,
                vanilla_sync_lookback: 20,
                eth_rpc_url: manager.config.eth_rpc_url.clone(),
            };
            let entry = manager.open(&record)?;
            let online = match entry.online {
                Some(value) => value,
                None => {
                    let online = entry.wallet.go_online(options)?;
                    entry.online = Some(online);
                    online
                }
            };
            entry.wallet.refresh(online, None, vec![], false)?;
            Ok(())
        })
        .await
    }
}

fn registration_file_id(path: &Path) -> Option<Uuid> {
    if path.extension()?.to_str()? != "json" {
        return None;
    }
    Uuid::parse_str(path.file_stem()?.to_str()?).ok()
}

fn identifier(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && value.bytes().all(|b| b.is_ascii_graphic())
}

fn private_directory(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn checked_address(value: &RegisteredAddress, network: BitcoinNetwork) -> Result<Address> {
    if !identifier(&value.signing_key_id) {
        return Err(MpcError::Invalid("Invalid signing key reference"));
    }
    let btc_network = Network::from(network);
    let address = Address::from_str(&value.address)
        .map_err(|_| MpcError::Invalid("Invalid Bitcoin address"))?
        .require_network(btc_network)
        .map_err(|_| MpcError::Invalid("Bitcoin address network mismatch"))?;
    let expected = match value.script_type {
        ScriptType::P2wpkh => {
            let key = CompressedPublicKey::from_str(&value.public_key)
                .map_err(|_| MpcError::Invalid("Expected a compressed P2WPKH key"))?;
            Address::p2wpkh(&key, btc_network)
        }
        ScriptType::P2tr => {
            let output_key = XOnlyPublicKey::from_str(&value.public_key)
                .map_err(|_| MpcError::Invalid("Expected a Taproot output key"))?;
            Address::p2tr_tweaked(
                TweakedPublicKey::dangerous_assume_tweaked(output_key),
                btc_network,
            )
        }
    };
    if address.script_pubkey() != expected.script_pubkey() {
        return Err(MpcError::Invalid(
            "Bitcoin address does not match its public key and script type",
        ));
    }
    Ok(address)
}

/// Pinned role addresses, supplied only by a trusted provider adapter. This
/// implements the library's address callback without derivation or secrets.
pub struct RegistryProvider {
    network: BitcoinNetwork,
    addresses: Vec<RegisteredAddress>,
}
impl RegistryProvider {
    pub fn new(network: BitcoinNetwork, addresses: Vec<RegisteredAddress>) -> Result<Self> {
        if !(1..=2).contains(&addresses.len())
            || addresses.iter().filter(|a| a.role == Role::Rgb).count() != 1
            || addresses.iter().filter(|a| a.role == Role::Fee).count() > 1
        {
            return Err(MpcError::Invalid(
                "Exactly one RGB address and at most one fee address are required",
            ));
        }
        let mut scripts = HashSet::new();
        for value in &addresses {
            if !scripts.insert(checked_address(value, network)?.script_pubkey()) {
                return Err(MpcError::Invalid("RGB and fee addresses must be distinct"));
            }
        }
        Ok(Self { network, addresses })
    }
}
impl MpcWalletProvider for RegistryProvider {
    fn create_address(
        &self,
        network: BitcoinNetwork,
        keychain: KeychainKind,
        index: u32,
    ) -> std::result::Result<MpcAddressInfo, rgb_lib::Error> {
        if network != self.network || index != 0 {
            return Err(rgb_lib::Error::MpcProvider {
                details: "Pinned registry address unavailable for this network/index".into(),
            });
        }
        let role = match keychain {
            KeychainKind::External => Role::Rgb,
            KeychainKind::Internal => Role::Fee,
        };
        let value = self
            .addresses
            .iter()
            .find(|a| a.role == role)
            .ok_or_else(|| rgb_lib::Error::MpcProvider {
                details: "Role not registered".into(),
            })?;
        let address = checked_address(value, network).map_err(|_| rgb_lib::Error::MpcProvider {
            details: "Invalid registered address".into(),
        })?;
        Ok(MpcAddressInfo {
            address: address.to_string(),
            script_pubkey: address.script_pubkey(),
            signing_key_id: value.signing_key_id.clone(),
            derivation_index: index,
        })
    }
    fn sign_psbt(&self, _: Psbt, _: Vec<String>) -> std::result::Result<Psbt, rgb_lib::Error> {
        Err(rgb_lib::Error::WatchOnly)
    }
}

struct Manager {
    config: Config,
    network: BitcoinNetwork,
    root: PathBuf,
    entries: BTreeMap<Uuid, Entry>,
    _lock: Arc<fs::File>,
}
impl Manager {
    fn registrations(&self) -> Result<Vec<(Owner, Uuid)>> {
        let mut registered = Vec::new();
        for file in fs::read_dir(self.root.join("registrations"))? {
            let path = file?.path();
            let Some(id) = registration_file_id(&path) else {
                continue;
            };
            match self.read(id) {
                Ok(record) => registered.push((record.owner, id)),
                Err(_) => log::warn!("MPC registration {id} needs repair; refresh skipped"),
            }
        }
        Ok(registered)
    }

    fn record_path(&self, id: Uuid) -> PathBuf {
        self.root.join("registrations").join(format!("{id}.json"))
    }
    fn read(&self, id: Uuid) -> Result<Record> {
        let bytes = fs::read(self.record_path(id)).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                MpcError::NotFound
            } else {
                MpcError::Internal
            }
        })?;
        let record: Record = serde_json::from_slice(&bytes)?;
        if record.version != 1 || record.registration.wallet_id != id {
            return Err(MpcError::Internal);
        }
        Ok(record)
    }
    fn owned(&self, owner: &Owner, id: Uuid) -> Result<Record> {
        let record = self.read(id)?;
        if &record.owner != owner {
            return Err(MpcError::NotFound);
        }
        Ok(record)
    }
    fn save(&self, record: &Record) -> Result<()> {
        let dir = self.root.join("registrations");
        let mut temp = tempfile::NamedTempFile::new_in(&dir)?;
        temp.write_all(&serde_json::to_vec_pretty(record)?)?;
        temp.as_file().sync_all()?;
        temp.persist(self.record_path(record.registration.wallet_id))
            .map_err(|_| MpcError::Internal)?;
        fs::File::open(dir)?.sync_all()?;
        Ok(())
    }
    fn validate(&self, request: &Registration) -> Result<()> {
        let genesis = genesis_block(Network::from(self.network))
            .block_hash()
            .to_string();
        if request.bitcoin_network != self.config.network.to_ascii_lowercase()
            || request.genesis_hash != genesis
        {
            return Err(MpcError::Invalid(
                "Configured Bitcoin network/genesis mismatch",
            ));
        }
        if !identifier(&request.provider_environment) || !identifier(&request.provider_wallet_ref) {
            return Err(MpcError::Invalid(
                "Provider environment and wallet reference are required",
            ));
        }
        RegistryProvider::new(self.network, request.addresses.clone())?;
        Ok(())
    }
    fn open(&mut self, record: &Record) -> Result<&mut Entry> {
        let request = &record.registration;
        self.validate(request)?;
        if !self.entries.contains_key(&request.wallet_id) {
            // The library derives an eight-character fingerprint internally;
            // a full UUID parent directory prevents cross-wallet collisions.
            let directory = self
                .root
                .join("wallets")
                .join(request.wallet_id.to_string());
            private_directory(&directory)?;
            let provider = RegistryProvider::new(self.network, request.addresses.clone())?;
            let mut wallet = MpcWallet::new(
                WalletData {
                    data_dir: directory.to_string_lossy().into(),
                    bitcoin_network: self.network,
                    database_type: DatabaseType::Sqlite,
                    max_allocations_per_utxo: 5,
                    supported_schemas: vec![AssetSchema::Nia],
                    reuse_addresses: true,
                },
                request.wallet_id.to_string(),
                Box::new(provider),
            )?;
            if request
                .addresses
                .iter()
                .any(|address| address.role == Role::Fee)
            {
                wallet.get_address()?;
            }
            // Receiving needs only the RGB role, registered atomically with its first invoice.
            self.entries.insert(
                request.wallet_id,
                Entry {
                    wallet,
                    online: None,
                },
            );
        }
        self.entries
            .get_mut(&request.wallet_id)
            .ok_or(MpcError::Internal)
    }
    fn register(&mut self, owner: Owner, mut request: Registration) -> Result<WalletView> {
        if !identifier(&owner.tenant_id) || !identifier(&owner.user_id) {
            return Err(MpcError::Invalid("Owner required"));
        }
        request.addresses.sort_by_key(|a| a.role);
        self.validate(&request)?;
        for address in &mut request.addresses {
            address.address = checked_address(address, self.network)?.to_string();
            address.public_key = address.public_key.to_ascii_lowercase();
        }
        match self.read(request.wallet_id) {
            Ok(record) => {
                if record.owner != owner {
                    return Err(MpcError::NotFound);
                }
                if record.registration != request {
                    return Err(MpcError::Conflict("Wallet binding is immutable"));
                }
                return Ok(view(&record));
            }
            Err(MpcError::NotFound) => {}
            Err(error) => return Err(error),
        }
        for file in fs::read_dir(self.root.join("registrations"))? {
            let path = file?.path();
            if path.extension().is_none_or(|extension| extension != "json") {
                continue;
            }
            let Some(id) = registration_file_id(&path) else {
                continue;
            };
            // A damaged real registration might own the requested address.
            // Fail closed for new bindings, while other wallets keep working.
            let existing = self.read(id).map_err(|_| {
                MpcError::Conflict("Registry record needs repair before registering another wallet")
            })?;
            let registration = &existing.registration;
            if (registration.provider == request.provider
                && registration.provider_environment == request.provider_environment
                && registration.provider_wallet_ref == request.provider_wallet_ref)
                || registration.addresses.iter().any(|old| {
                    request
                        .addresses
                        .iter()
                        .any(|new| old.address == new.address)
                })
            {
                return Err(MpcError::Conflict(
                    "Provider wallet or address already registered",
                ));
            }
        }
        let record = Record {
            version: 1,
            owner,
            registration: request,
            invoices: BTreeMap::new(),
        };
        self.save(&record)?;
        Ok(view(&record))
    }
    fn recover_invoice(
        &mut self,
        record: &Record,
        pending: &InvoiceRecord,
    ) -> Result<Option<WitnessInvoice>> {
        let Some(before) = &pending.prior_batch_ids else {
            return Ok(None);
        };
        let transfers = self
            .open(record)?
            .wallet
            .list_transfers(AssetFilter::AnyOrNone, None)?;
        let added: Vec<_> = transfers
            .iter()
            .filter(|t| !before.contains(&t.batch_transfer_idx))
            .collect();
        if added.is_empty() {
            return Ok(None);
        }
        if added.len() != 1 {
            return Err(MpcError::Conflict(
                "Ambiguous invoice preparation; operator reconciliation required",
            ));
        }
        let transfer = added[0];
        let invoice = transfer.invoice_string.clone().ok_or(MpcError::Internal)?;
        let data = rgb_lib::wallet::Invoice::new(invoice.clone())?.invoice_data();
        let expected = pending
            .request
            .amount
            .as_ref()
            .map(|amount| amount.parse::<u64>())
            .transpose()
            .map_err(|_| MpcError::Internal)?
            .map(Assignment::Fungible)
            .unwrap_or(Assignment::Any);
        let address = record
            .registration
            .addresses
            .iter()
            .find(|a| a.role == Role::Rgb)
            .ok_or(MpcError::Internal)?;
        if transfer.kind != rgb_lib::wallet::TransferKind::ReceiveWitness
            || data.asset_id != pending.request.asset_id
            || data.assignment != expected
            || data.expiration_timestamp != Some(pending.request.expiration_timestamp)
            || data.network != self.network
            || rgb_lib::utils::script_buf_from_recipient_id(data.recipient_id.clone())?
                != Some(checked_address(address, self.network)?.script_pubkey())
            || data.transport_endpoints.len() != self.config.proxy_address.len()
            || !data
                .transport_endpoints
                .iter()
                .zip(&self.config.proxy_address)
                .all(|(actual, configured)| actual.split('?').next() == Some(configured.as_str()))
        {
            return Err(MpcError::Conflict(
                "Recovered invoice differs from its saved request",
            ));
        }
        Ok(Some(WitnessInvoice {
            wallet_id: record.registration.wallet_id,
            request_id: pending.request.request_id,
            invoice,
            recipient_id: data.recipient_id,
            expiration_timestamp: pending.request.expiration_timestamp,
            batch_transfer_idx: transfer.batch_transfer_idx,
        }))
    }

    fn witness(
        &mut self,
        owner: &Owner,
        id: Uuid,
        request: WitnessRequest,
    ) -> Result<WitnessInvoice> {
        let mut record = self.owned(owner, id)?;
        if let Some(previous) = record.invoices.get(&request.request_id) {
            if previous.request != request {
                return Err(MpcError::Conflict(
                    "Request ID already used with a different invoice request",
                ));
            }
            if let Some(result) = previous.result.clone() {
                return Ok(result);
            }
            if let Some(result) = self.recover_invoice(&record, previous)? {
                record
                    .invoices
                    .get_mut(&request.request_id)
                    .ok_or(MpcError::Internal)?
                    .result = Some(result.clone());
                self.save(&record)?;
                return Ok(result);
            }
            if !previous.failed_before_receive {
                return Err(MpcError::Conflict(
                    "Invoice preparation needs reconciliation; keep the saved request ID",
                ));
            }
        }
        if record.invoices.iter().any(|(id, invoice)| {
            *id != request.request_id && invoice.result.is_none() && !invoice.failed_before_receive
        }) {
            return Err(MpcError::Conflict(
                "Reconcile the saved invoice before creating another",
            ));
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| MpcError::Internal)?
            .as_secs();
        if request.expiration_timestamp <= now || request.expiration_timestamp > now + 7 * 86400 {
            return Err(MpcError::Invalid(
                "Invoice expiry must be in the next seven days",
            ));
        }
        let assignment = match &request.amount {
            None => Assignment::Any,
            Some(amount) => {
                if amount.is_empty()
                    || amount.starts_with('0')
                    || !amount.bytes().all(|b| b.is_ascii_digit())
                {
                    return Err(MpcError::Invalid(
                        "Amount must be a positive uint64 decimal string",
                    ));
                }
                Assignment::Fungible(
                    amount
                        .parse::<u64>()
                        .map_err(|_| MpcError::Invalid("Amount exceeds uint64"))?,
                )
            }
        };
        let endpoints = self.config.proxy_address.clone();
        // This POC pins one RGB address, so each invoice needs the library's
        // transport nonce. Out-of-band address reuse is not exposed here.
        if endpoints.is_empty() {
            return Err(MpcError::Invalid("A configured RGB transport is required"));
        }
        let entry = self.open(&record)?;
        if let Some(asset) = &request.asset_id {
            entry.wallet.get_asset_metadata(asset.clone())?;
        }
        let prior_batch_ids = entry
            .wallet
            .list_transfers(AssetFilter::AnyOrNone, None)?
            .iter()
            .map(|transfer| transfer.batch_transfer_idx)
            .collect();
        record.invoices.insert(
            request.request_id,
            InvoiceRecord {
                request: request.clone(),
                result: None,
                prior_batch_ids: Some(prior_batch_ids),
                failed_before_receive: false,
            },
        );
        self.save(&record)?;
        let receive = self.open(&record)?.wallet.witness_receive(
            request.asset_id.clone(),
            assignment,
            request.expiration_timestamp,
            endpoints,
            1,
        );
        let receive: ReceiveData = match receive {
            Ok(receive) => receive,
            Err(error) => {
                let pending = record
                    .invoices
                    .get(&request.request_id)
                    .ok_or(MpcError::Internal)?;
                let before = pending.prior_batch_ids.as_ref().ok_or(MpcError::Internal)?;
                let after = self
                    .open(&record)?
                    .wallet
                    .list_transfers(AssetFilter::AnyOrNone, None)?;
                if after.iter().all(|t| before.contains(&t.batch_transfer_idx)) {
                    record
                        .invoices
                        .get_mut(&request.request_id)
                        .ok_or(MpcError::Internal)?
                        .failed_before_receive = true;
                    self.save(&record)?;
                }
                return Err(error.into());
            }
        };
        let result = WitnessInvoice {
            wallet_id: id,
            request_id: request.request_id,
            invoice: receive.invoice,
            recipient_id: receive.recipient_id,
            expiration_timestamp: receive.expiration_timestamp,
            batch_transfer_idx: receive.batch_transfer_idx,
        };
        record
            .invoices
            .get_mut(&request.request_id)
            .ok_or(MpcError::Internal)?
            .result = Some(result.clone());
        self.save(&record)?;
        Ok(result)
    }
}
fn view(record: &Record) -> WalletView {
    let registration = &record.registration;
    WalletView {
        wallet_id: registration.wallet_id,
        provider: registration.provider.clone(),
        bitcoin_network: registration.bitcoin_network.clone(),
        genesis_hash: registration.genesis_hash.clone(),
        addresses: registration.addresses.clone(),
        supported_schemas: vec!["nia".into()],
        witness_receive: true,
        signing: false,
    }
}

#[cfg(test)]
mod isolation_tests {
    use super::*;

    fn service() -> (tempfile::TempDir, MpcService) {
        let dir = tempfile::tempdir().unwrap();
        let service = MpcService::new(
            Config {
                data_dir: dir.path().to_string_lossy().into(),
                network: "regtest".into(),
                ..Default::default()
            },
            Some("offline-registry-test-service-token".into()),
        )
        .unwrap();
        (dir, service)
    }

    #[tokio::test]
    async fn slow_wallet_does_not_block_other_wallets_or_release_the_process_lock() {
        let (dir, service) = service();
        let slow = service.clone();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let work = tokio::spawn(async move {
            slow.call(Uuid::new_v4(), move |_| {
                started_tx.send(()).unwrap();
                release_rx
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap();
                Ok(())
            })
            .await
        });
        started_rx.await.unwrap();
        let other = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            service.call(Uuid::new_v4(), |_| Ok(42)),
        )
        .await;
        assert_eq!(other.unwrap().unwrap(), 42);
        drop(service);
        let config = Config {
            data_dir: dir.path().to_string_lossy().into(),
            network: "regtest".into(),
            ..Default::default()
        };
        assert!(matches!(
            MpcService::new(
                config.clone(),
                Some("offline-registry-test-service-token".into())
            ),
            Err(MpcError::Conflict(_))
        ));
        release_tx.send(()).unwrap();
        work.await.unwrap().unwrap();
        assert!(
            MpcService::new(config, Some("offline-registry-test-service-token".into())).is_ok()
        );
    }

    #[tokio::test]
    async fn idle_wallet_managers_are_evicted_and_one_poisoned_wallet_is_isolated() {
        let (_dir, service) = service();
        let broken = Uuid::new_v4();
        assert!(
            service
                .call::<()>(broken, |_| panic!("isolated fixture panic"))
                .await
                .is_err()
        );
        assert!(service.call(broken, |_| Ok(())).await.is_err());
        for _ in 0..MAX_OPEN_WALLETS + 2 {
            service.call(Uuid::new_v4(), |_| Ok(())).await.unwrap();
        }
        let state = service.manager.as_ref().unwrap();
        assert_eq!(state.wallets.lock().unwrap().len(), MAX_OPEN_WALLETS);
    }
}

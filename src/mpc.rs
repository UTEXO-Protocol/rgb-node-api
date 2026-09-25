//! Gateway-only, public-key MPC registry and blind receiving for the NIA POC.
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
    Assignment, BitcoinNetwork,
    bdk_wallet::KeychainKind,
    bitcoin::{
        Address, Network, Psbt,
        blockdata::constants::genesis_block,
        hashes::{Hash, sha256},
        key::TweakedPublicKey,
        secp256k1::XOnlyPublicKey,
    },
    mpc::{MpcAddressInfo, MpcWalletProvider},
    wallet::{
        AssetFilter, Assets, BtcBalance, DatabaseType, MpcWallet, Online, OnlineOptions,
        ReceiveData, RgbWalletOpsOffline, RgbWalletOpsOnline, Transfer, WalletData,
    },
};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use uuid::Uuid;

#[path = "mpc_send.rs"]
mod send;
pub use send::{SendProfile, SendRequest, SendView};
#[path = "mpc_utxos.rs"]
mod utxos;
pub use utxos::{CreateUtxosRequest, CreateUtxosView};

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
#[serde(try_from = "String", into = "String")]
pub struct Provider(String);
impl Provider {
    pub fn as_str(&self) -> &str {
        &self.0
    }
    fn validate(&self) -> Result<()> {
        let bytes = self.0.as_bytes();
        if bytes.is_empty()
            || bytes.len() > 64
            || !bytes[0].is_ascii_lowercase()
            || !bytes
                .iter()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_' || *b == b'-')
        {
            return Err(MpcError::Invalid("Invalid provider identifier"));
        }
        Ok(())
    }
}
impl TryFrom<String> for Provider {
    type Error = MpcError;
    fn try_from(value: String) -> Result<Self> {
        let provider = Self(value);
        provider.validate()?;
        Ok(provider)
    }
}
impl From<Provider> for String {
    fn from(value: Provider) -> Self {
        value.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Rgb,
    Fee,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScriptType {
    P2tr,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegisteredAddress {
    pub role: Role,
    pub script_type: ScriptType,
    pub address: String,
    /// Tweaked x-only OUTPUT key for P2TR.
    pub public_key: String,
    /// Verified untweaked x-only key; no script tree is supported.
    pub internal_key: String,
    pub provider_wallet_id: String,
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
pub struct BlindRequest {
    pub request_id: Uuid,
    pub asset_id: Option<String>,
    /// Integer base units. None creates an unconstrained receive invoice.
    pub amount: Option<String>,
    /// Schema of `asset_id` while the wallet has not seen that contract yet.
    /// Absent means NIA, so saved requests and NIA deployments are unchanged.
    #[serde(default)]
    pub schema: Option<rgb_lib::AssetSchema>,
    pub expiration_timestamp: u64,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct BlindInvoice {
    pub wallet_id: Uuid,
    pub request_id: Uuid,
    pub invoice: String,
    pub recipient_id: String,
    pub expiration_timestamp: u64,
    pub batch_transfer_idx: i32,
}

#[derive(Clone, Serialize, Deserialize)]
struct InvoiceRecord {
    request: BlindRequest,
    result: Option<BlindInvoice>,
    #[serde(default)]
    prior_batch_ids: Option<Vec<i32>>,
    #[serde(default)]
    failed_before_receive: bool,
    #[serde(default)]
    unbound_asset: bool,
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
    pub blind_receive: bool,
    pub witness_receive: bool,
    /// Supported external PSBT contract, independent of provider name or funding.
    pub external_send: Option<SendProfile>,
    /// Local server signing; external send does not require this capability.
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
        config.mpc_send.validate().map_err(MpcError::Invalid)?;
        let network = config.net()?;
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
        bind_network(&root, network)?;
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
            view(&record, &manager.config)
        })
        .await
    }
    pub async fn blind(
        &self,
        owner: Owner,
        id: Uuid,
        request: BlindRequest,
    ) -> Result<BlindInvoice> {
        self.call(id, move |manager| manager.blind(&owner, id, request))
            .await
    }
    pub async fn assets(&self, owner: Owner, id: Uuid) -> Result<Assets> {
        self.call(id, move |manager| {
            let record = manager.owned(&owner, id)?;
            Ok(manager.open(&record)?.wallet.list_assets(vec![])?)
        })
        .await
    }
    /// Include a live BTC snapshot without making cached RGB balances depend on
    /// indexer availability. Ownership is checked before either wallet read.
    pub async fn asset_snapshot(
        &self,
        owner: Owner,
        id: Uuid,
    ) -> Result<(Assets, Option<BtcBalance>)> {
        self.call(id, move |manager| {
            let record = manager.owned(&owner, id)?;
            let options = OnlineOptions {
                indexer_url: manager.config.indexer_address.clone(),
                skip_consistency_check: false,
                vanilla_sync_lookback: 20,
                eth_rpc_url: manager.config.eth_rpc(),
            };
            let entry = manager.open(&record)?;
            let assets = entry.wallet.list_assets(vec![])?;
            let btc_balance = (|| -> std::result::Result<BtcBalance, rgb_lib::Error> {
                if entry.online.is_none() {
                    entry.online = Some(entry.wallet.go_online(options)?);
                }
                // MPC reads current UTXOs from the indexer; no RGB refresh or
                // provider signature is needed just to display BTC.
                entry.wallet.get_btc_balance(entry.online, true)
            })()
            .ok();
            Ok((assets, btc_balance))
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
                eth_rpc_url: manager.config.eth_rpc(),
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
        ScriptType::P2tr => {
            let output_key = XOnlyPublicKey::from_str(&value.public_key)
                .map_err(|_| MpcError::Invalid("Expected a Taproot output key"))?;
            Address::p2tr_tweaked(
                TweakedPublicKey::dangerous_assume_tweaked(output_key),
                btc_network,
            )
        }
    };
    let internal = XOnlyPublicKey::from_str(&value.internal_key)
        .map_err(|_| MpcError::Invalid("Invalid Taproot internal key"))?;
    if !identifier(&value.provider_wallet_id)
        || Address::p2tr(
            &rgb_lib::bitcoin::secp256k1::Secp256k1::verification_only(),
            internal,
            None,
            btc_network,
        ) != expected
        || address.script_pubkey() != expected.script_pubkey()
    {
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
        if addresses.len() != 2
            || addresses.iter().filter(|a| a.role == Role::Rgb).count() != 1
            || addresses.iter().filter(|a| a.role == Role::Fee).count() != 1
        {
            return Err(MpcError::Invalid(
                "Exactly one RGB and one fee address are required",
            ));
        }
        let mut scripts = HashSet::new();
        let mut keys = HashSet::new();
        let mut wallets = HashSet::new();
        for value in &addresses {
            if !scripts.insert(checked_address(value, network)?.script_pubkey())
                || value.signing_key_id.is_empty()
                || value.provider_wallet_id.is_empty()
                || !keys.insert(value.signing_key_id.clone())
                || !wallets.insert(value.provider_wallet_id.clone())
            {
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

#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct NetworkBinding {
    version: u32,
    bitcoin_network: String,
    genesis_hash: String,
}

// Called under service.lock. One state directory belongs to one chain; changing
// configuration must not reinterpret persisted registrations or send journals.
fn bind_network(root: &Path, network: BitcoinNetwork) -> Result<()> {
    let expected = NetworkBinding {
        version: 1,
        bitcoin_network: network.to_string().to_ascii_lowercase(),
        genesis_hash: genesis_block(Network::from(network))
            .block_hash()
            .to_string(),
    };
    let path = root.join("network.json");
    match fs::read(&path) {
        Ok(bytes) => {
            let saved: NetworkBinding = serde_json::from_slice(&bytes)?;
            if saved != expected {
                return Err(MpcError::Conflict(
                    "MPC data directory belongs to another Bitcoin network; use a separate data_dir",
                ));
            }
            return Ok(());
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    // Adopt legacy state only when its registrations confirm the configured
    // chain. Do not rewrite registrations, wallets, or operation journals.
    let mut registrations = 0;
    for file in fs::read_dir(root.join("registrations"))? {
        let path = file?.path();
        let Some(id) = registration_file_id(&path) else {
            continue;
        };
        let record: Record = serde_json::from_slice(&fs::read(path)?)?;
        if record.version != 1 || record.registration.wallet_id != id {
            return Err(MpcError::Internal);
        }
        if Config::parse_network(&record.registration.bitcoin_network)? != network
            || record.registration.genesis_hash != expected.genesis_hash
        {
            return Err(MpcError::Conflict(
                "Existing MPC registrations belong to another Bitcoin network",
            ));
        }
        registrations += 1;
    }
    if registrations == 0 {
        for directory in ["wallets", "sends"] {
            let path = root.join(directory);
            if path.exists() && fs::read_dir(path)?.next().is_some() {
                return Err(MpcError::Conflict(
                    "Cannot determine Bitcoin network for existing MPC state without registrations",
                ));
            }
        }
    }
    let mut temp = tempfile::NamedTempFile::new_in(root)?;
    temp.write_all(&serde_json::to_vec_pretty(&expected)?)?;
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|_| MpcError::Internal)?;
    fs::File::open(root)?.sync_all()?;
    Ok(())
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
        request.provider.validate()?;
        let genesis = genesis_block(Network::from(self.network))
            .block_hash()
            .to_string();
        if Config::parse_network(&request.bitcoin_network)? != self.network
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
                    supported_schemas: self.config.supported_schemas()?,
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
            wallet.get_rgb_address()?;
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
        request.bitcoin_network = self.network.to_string().to_ascii_lowercase();
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
                return view(&record, &self.config);
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
        view(&record, &self.config)
    }
    fn recover_invoice(
        &mut self,
        record: &Record,
        pending: &InvoiceRecord,
    ) -> Result<Option<BlindInvoice>> {
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
        let mut invoice = transfer.invoice_string.clone().ok_or(MpcError::Internal)?;
        if pending.unbound_asset {
            invoice = bind_first_invoice(
                invoice,
                pending.request.asset_id.as_deref(),
                pending.request.schema.unwrap_or(rgb_lib::AssetSchema::Nia),
            )?;
        }
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
        if transfer.kind != rgb_lib::wallet::TransferKind::ReceiveBlind
            || data.asset_id != pending.request.asset_id
            || data.assignment != expected
            || data.expiration_timestamp != Some(pending.request.expiration_timestamp)
            || data.network != self.network
            || rgb_lib::utils::script_buf_from_recipient_id(data.recipient_id.clone())?.is_some()
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
        Ok(Some(BlindInvoice {
            wallet_id: record.registration.wallet_id,
            request_id: pending.request.request_id,
            invoice,
            recipient_id: data.recipient_id,
            expiration_timestamp: pending.request.expiration_timestamp,
            batch_transfer_idx: transfer.batch_transfer_idx,
        }))
    }

    fn blind(&mut self, owner: &Owner, id: Uuid, request: BlindRequest) -> Result<BlindInvoice> {
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
        if endpoints.is_empty() {
            return Err(MpcError::Invalid("A configured RGB transport is required"));
        }
        let options = OnlineOptions {
            indexer_url: self.config.indexer_address.clone(),
            skip_consistency_check: false,
            vanilla_sync_lookback: 20,
            eth_rpc_url: self.config.eth_rpc(),
        };
        let supported = self.config.supported_schemas()?;
        let requested_schema = request.schema.unwrap_or(rgb_lib::AssetSchema::Nia);
        let entry = self.open(&record)?;
        let online = match entry.online {
            Some(online) => online,
            None => {
                let online = entry.wallet.go_online(options)?;
                entry.online = Some(online);
                online
            }
        };
        // Discover the funded External UTXO before constructing its blind seal.
        entry.wallet.list_unspents(Some(online), false, false)?;
        let mut unbound_asset = false;
        if let Some(asset) = &request.asset_id {
            rgb_lib::ContractId::from_str(asset)
                .map_err(|_| MpcError::Invalid("Invalid asset ID"))?;
            match entry.wallet.get_asset_metadata(asset.clone()) {
                // A known contract states its own schema; the request cannot override it.
                Ok(metadata) if supported.contains(&metadata.asset_schema) => {
                    if request.schema.is_some_and(|s| s != metadata.asset_schema) {
                        return Err(MpcError::Invalid(
                            "Requested schema differs from the known contract",
                        ));
                    }
                }
                Ok(_) => return Err(MpcError::Invalid("Unsupported asset schema")),
                Err(rgb_lib::Error::AssetNotFound { .. }) => {
                    if !supported.contains(&requested_schema) {
                        return Err(MpcError::Invalid("Unsupported asset schema"));
                    }
                    unbound_asset = true;
                }
                Err(error) => return Err(error.into()),
            }
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
                unbound_asset,
            },
        );
        self.save(&record)?;
        let receive = self.open(&record)?.wallet.blind_receive(
            if unbound_asset {
                None
            } else {
                request.asset_id.clone()
            },
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
        let result = BlindInvoice {
            wallet_id: id,
            request_id: request.request_id,
            invoice: if unbound_asset {
                bind_first_invoice(
                    receive.invoice,
                    request.asset_id.as_deref(),
                    requested_schema,
                )?
            } else {
                receive.invoice
            },
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
// A fresh wallet cannot name an unknown asset in its receive database yet.
// Keep the library's validated generic receive and pin the public invoice to
// the requested NIA contract. The immutable record reconstructs it on restart.
fn bind_first_invoice(
    invoice: String,
    asset: Option<&str>,
    schema: rgb_lib::AssetSchema,
) -> Result<String> {
    let mut invoice = rgbinvoice::RgbInvoice::from_str(&invoice).map_err(|_| MpcError::Internal)?;
    if let Some(asset) = asset {
        invoice.contract = Some(
            asset
                .parse()
                .map_err(|_| MpcError::Invalid("Invalid asset ID"))?,
        );
        invoice.schema = Some(schema.into());
    }
    Ok(invoice.to_string())
}
fn view(record: &Record, config: &Config) -> Result<WalletView> {
    let registration = &record.registration;
    Ok(WalletView {
        wallet_id: registration.wallet_id,
        provider: registration.provider.clone(),
        bitcoin_network: registration.bitcoin_network.clone(),
        genesis_hash: registration.genesis_hash.clone(),
        addresses: registration.addresses.clone(),
        supported_schemas: config
            .supported_schemas()?
            .iter()
            .map(|schema| format!("{schema:?}").to_ascii_lowercase())
            .collect(),
        blind_receive: true,
        witness_receive: false,
        external_send: send::supported_profile(registration),
        signing: false,
    })
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

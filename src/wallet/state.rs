use std::collections::{BTreeSet, HashMap};

use rgb_lib::wallet::{
    Balance, BtcBalance, DatabaseType, IndexerConfig, IndexerProtocol, Online, OnlineOptions,
    ReceiveData, RgbWalletOpsOffline, RgbWalletOpsOnline, SinglesigKeys, Wallet, WalletData,
    check_indexer_url,
};

use super::Config;
use super::ReceiveKind;

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

/// rgb-lib no longer guesses the indexer protocol from the URL when going
/// online, so probe it here (electrum first, then esplora, as before).
fn indexer_config(config: &Config) -> Result<IndexerConfig, rgb_lib::Error> {
    let url = config.indexer_address.clone();
    Ok(match check_indexer_url(&url, None, config.net())? {
        IndexerProtocol::Electrum => IndexerConfig::Electrum { url },
        IndexerProtocol::Esplora => IndexerConfig::Esplora { url },
        IndexerProtocol::Bitcoind => {
            return Err(rgb_lib::Error::InvalidIndexer {
                details: "bitcoind indexers are not supported".into(),
            });
        }
    })
}

pub struct RgbWalletState {
    pub id: String,
    pub master_xpub: String,
    pub wallet: Wallet,
    pub wallet_online: Online,

    proxy_host: BTreeSet<String>,
    /// Our own proxy, where recipients post their ACK when we send.
    pub sender_proxy: Option<String>,
}

impl RgbWalletState {
    pub fn new_ro_wallet(
        config: &Config,
        xpub_colored: String,
        xpub_vanilla: String,
        master_fingerprint: String,
        max_allocations_per_utxo: Option<u32>,
    ) -> Result<Self, rgb_lib::Error> {
        let data_dir = config.datadir();

        std::fs::create_dir_all(&data_dir)?;

        let mut wallet = Wallet::new(
            WalletData {
                data_dir,
                bitcoin_network: config.net(),
                database_type: DatabaseType::Sqlite,
                max_allocations_per_utxo: max_allocations_per_utxo.unwrap_or(5),
                supported_schemas: config.supported_schemas()?,
            },
            SinglesigKeys {
                account_xpub_colored: xpub_colored,
                account_xpub_vanilla: xpub_vanilla,
                mnemonic: None,
                master_fingerprint: master_fingerprint.clone(),
                vanilla_keychain: None,
                witness_version: Default::default(),
            },
        )?;

        let online = wallet.go_online(OnlineOptions {
            indexer_config: indexer_config(config)?,
            skip_consistency_check: false,
            vanilla_sync_lookback: 20,
            // Keep detecting late BTC payments to failed witness receives for a week.
            failed_witness_receive_grace_secs: 604800,
            // Our proxies run without proxy-level authentication.
            proxy_auth_keys: HashMap::new(),
            evm_rpc_urls: config.evm_rpc_urls(),
        })?;

        Ok(RgbWalletState {
            id: master_fingerprint,
            wallet,
            wallet_online: online,
            proxy_host: config.proxy_address.iter().cloned().collect(),
            sender_proxy: config.proxy_address.first().cloned(),
            master_xpub: Default::default(),
        })
    }

    pub fn receive_token(
        &mut self,
        kind: ReceiveKind,
        asset_id: Option<String>,
        assigment: rgb_lib::Assignment,
        duration_seconds: u32,
        min_confirmations: u8,
    ) -> Result<ReceiveData, rgb_lib::Error> {
        let transport_endpoints = self.proxy_host.iter().cloned().collect();

        // The API takes a duration, rgb-lib wants an absolute unix timestamp:
        // passing the duration through unchanged lands in 1970 and rgb-lib
        // rejects it with InvalidExpiration.
        // ReceiveReq has already resolved omitted/null durations to the default.
        let expiration_timestamp = unix_now().saturating_add(u64::from(duration_seconds));

        // Online is needed to reserve the recipient ID on the proxies.
        // No encryption preference: rgb-lib applies its default.
        let rd = match kind {
            ReceiveKind::Blind => self.wallet.blind_receive(
                Some(self.wallet_online),
                asset_id,
                assigment,
                expiration_timestamp,
                transport_endpoints,
                min_confirmations,
                None,
            )?,
            ReceiveKind::Witness => self.wallet.witness_receive(
                Some(self.wallet_online),
                asset_id,
                assigment,
                expiration_timestamp,
                transport_endpoints,
                min_confirmations,
                None,
            )?,
        };

        Ok(rd)
    }

    pub fn balance_btc(&mut self) -> Result<BtcBalance, rgb_lib::Error> {
        let balance = self
            .wallet
            .get_btc_balance(Some(self.wallet_online), false)?;
        Ok(balance)
    }

    pub fn balance_token(&mut self, asset_id: &str) -> Result<Balance, rgb_lib::Error> {
        let balance = self.wallet.get_asset_balance(asset_id.to_owned())?;

        Ok(balance)
    }

    pub fn refresh(&mut self) -> anyhow::Result<()> {
        self.wallet
            .refresh(self.wallet_online, None, Vec::new(), false)?;

        Ok(())
    }
}

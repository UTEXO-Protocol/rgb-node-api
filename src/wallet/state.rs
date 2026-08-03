use std::collections::BTreeSet;

use rgb_lib::AssetSchema;
use rgb_lib::wallet::{
    Balance, BtcBalance, DatabaseType, Online, OnlineOptions, ReceiveData, RgbWalletOpsOffline,
    RgbWalletOpsOnline, SinglesigKeys, Wallet, WalletData,
};

use super::Config;

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

pub struct RgbWalletState {
    pub id: String,
    pub master_xpub: String,
    pub wallet: rgb_lib::Wallet,
    pub wallet_online: Online,

    proxy_host: BTreeSet<String>,
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
                supported_schemas: vec![AssetSchema::Nia],
                reuse_addresses: false,
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
            indexer_url: config.indexer_address.clone(),
            skip_consistency_check: false,
            vanilla_sync_lookback: 20,
        })?;

        Ok(RgbWalletState {
            id: master_fingerprint,
            wallet,
            wallet_online: online,
            proxy_host: config.proxy_address.iter().cloned().collect(),
            master_xpub: Default::default(),
        })
    }

    pub fn blind_receive_token(
        &mut self,
        asset_id: Option<String>,
        assigment: rgb_lib::Assignment,
        duration_seconds: Option<u32>,
        min_confirmations: u8,
    ) -> Result<ReceiveData, rgb_lib::Error> {
        let transport_endpoints = self.proxy_host.iter().cloned().collect();

        // The API takes a duration, rgb-lib wants an absolute unix timestamp:
        // passing the duration through unchanged lands in 1970 and rgb-lib
        // rejects it with InvalidExpiration.
        let expiration_timestamp =
            duration_seconds.map(|secs| unix_now().saturating_add(u64::from(secs)));

        let rd = self.wallet.blind_receive(
            asset_id,
            assigment,
            expiration_timestamp,
            transport_endpoints,
            min_confirmations,
        )?;

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

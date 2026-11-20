//! Optional server-side signing, for the `/wallet/send` and `/wallet/burn`
//! shortcuts that skip the begin/end round trip. Off unless
//! `allow_server_signing` is set in the config.

use rgb_lib::bdk_wallet::SignOptions;
use rgb_lib::keys::{Keys, WitnessVersion, generate_keys, restore_keys};
use rgb_lib::wallet::{DatabaseType, SinglesigKeys, Wallet, WalletData};

use super::Config;

/// Must match the watch-only wallets built in `state.rs`
/// (`witness_version: Default::default()`).
const WITNESS_VERSION: WitnessVersion = WitnessVersion::Taproot;
const MAX_ALLOCATIONS_PER_UTXO: u32 = 5;

/// Make a fresh mnemonic and the matching keys for the configured network.
pub fn new_keys(cfg: &Config) -> Keys {
    generate_keys(cfg.net(), WITNESS_VERSION)
}

/// Sign a PSBT with the wallet that `mnemonic` describes.
///
/// `expected_fingerprint` is the wallet id the request names. The mnemonic
/// must belong to that wallet.
pub fn sign_psbt(
    cfg: &Config,
    mnemonic: &str,
    expected_fingerprint: &str,
    psbt: String,
) -> Result<String, rgb_lib::Error> {
    let keys = restore_keys(cfg.net(), mnemonic.to_owned(), WITNESS_VERSION)?;
    if keys.master_fingerprint != expected_fingerprint {
        return Err(rgb_lib::Error::FingerprintMismatch);
    }

    // Own directory: it must not share files with the watch-only wallet.
    let data_dir = std::path::Path::new(&cfg.datadir()).join("signer");
    std::fs::create_dir_all(&data_dir)?;

    let wallet = Wallet::new(
        WalletData {
            data_dir: data_dir.to_string_lossy().into_owned(),
            bitcoin_network: cfg.net(),
            database_type: DatabaseType::Sqlite,
            max_allocations_per_utxo: MAX_ALLOCATIONS_PER_UTXO,
            supported_schemas: cfg.supported_schemas()?,
            allow_reckless_schemas: false,
        },
        SinglesigKeys {
            account_xpub_colored: keys.account_xpub_colored,
            account_xpub_vanilla: keys.account_xpub_vanilla,
            mnemonic: Some(mnemonic.to_owned()),
            master_fingerprint: keys.master_fingerprint,
            vanilla_keychain: None,
            witness_version: WITNESS_VERSION,
        },
    )?;

    wallet.sign_psbt(
        psbt,
        Some(SignOptions {
            trust_witness_utxo: true,
            ..Default::default()
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_a_mnemonic_of_another_wallet() {
        let cfg = Config {
            network: "regtest".into(),
            ..Config::default()
        };
        let keys = new_keys(&cfg);
        let err = sign_psbt(&cfg, &keys.mnemonic, "00000000", String::new()).unwrap_err();
        assert!(matches!(err, rgb_lib::Error::FingerprintMismatch));
    }

    #[test]
    fn signing_token_check() {
        let cfg = Config {
            allow_server_signing: true,
            server_signing_token: Some("0123456789abcdef".into()),
            ..Config::default()
        };
        assert!(cfg.signing_token_matches("0123456789abcdef"));
        assert!(!cfg.signing_token_matches("0123456789abcdeX"));
        assert!(!cfg.signing_token_matches("0123456789abcde"));
        assert!(!cfg.signing_token_matches(""));
        assert!(Config::default().check_server_signing().is_ok());
        let no_token = Config {
            allow_server_signing: true,
            ..Config::default()
        };
        assert!(no_token.check_server_signing().is_err());
    }
}

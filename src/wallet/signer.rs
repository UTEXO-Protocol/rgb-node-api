//! Optional server-side signing, for the `/wallet/send` and `/wallet/burn`
//! shortcuts that skip the begin/end round trip. Off unless
//! `allow_server_signing` is set in the config.

use rgb_lib::bdk_wallet::SignOptions;
use rgb_lib::keys::{Keys, WitnessVersion, generate_keys, restore_keys};
use rgb_lib::wallet::{DatabaseType, SinglesigKeys, Wallet, WalletData};

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use zeroize::Zeroize;

use super::Config;

/// Must match the watch-only wallets built in `state.rs`
/// (`witness_version: Default::default()`).
const WITNESS_VERSION: WitnessVersion = WitnessVersion::Taproot;
const MAX_ALLOCATIONS_PER_UTXO: u32 = 5;

/// Make a fresh mnemonic and the matching keys for the configured network.
pub fn new_keys(cfg: &Config) -> Keys {
    generate_keys(cfg.net(), WITNESS_VERSION)
}

/// Check that `mnemonic` belongs to the wallet `expected_fingerprint`.
///
/// Call this before `send_begin`/`burn_begin`, so a wrong mnemonic never
/// leaves a started transfer behind.
pub fn check_mnemonic(
    cfg: &Config,
    mnemonic: &str,
    expected_fingerprint: &str,
) -> Result<(), rgb_lib::Error> {
    let mut keys = restore_keys(cfg.net(), mnemonic.to_owned(), WITNESS_VERSION)?;
    keys.mnemonic.zeroize();
    if keys.master_fingerprint != expected_fingerprint {
        return Err(rgb_lib::Error::FingerprintMismatch);
    }
    Ok(())
}

/// A scratch folder for one signing call. It is removed on drop.
struct ScratchDir(PathBuf);

impl ScratchDir {
    fn new(cfg: &Config) -> std::io::Result<Self> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let name = format!(
            "{}-{nanos}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        // Own folder per call: no shared files, so two calls never collide.
        let dir = Path::new(&cfg.datadir()).join("signer").join(name);
        std::fs::create_dir_all(&dir)?;
        Ok(Self(dir))
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
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

    let scratch = ScratchDir::new(cfg)?;
    let wallet = Wallet::new(
        WalletData {
            data_dir: scratch.0.to_string_lossy().into_owned(),
            bitcoin_network: cfg.net(),
            database_type: DatabaseType::Sqlite,
            max_allocations_per_utxo: MAX_ALLOCATIONS_PER_UTXO,
            supported_schemas: cfg.supported_schemas()?,
            allow_reckless_schemas: false,
        },
        SinglesigKeys {
            account_xpub_colored: keys.account_xpub_colored,
            account_xpub_vanilla: keys.account_xpub_vanilla,
            // Moved, not copied: one fewer copy of the mnemonic in memory.
            mnemonic: Some(keys.mnemonic),
            master_fingerprint: keys.master_fingerprint,
            vanilla_keychain: None,
            witness_version: WITNESS_VERSION,
        },
    )?;

    let res = wallet.sign_psbt(
        psbt,
        Some(SignOptions {
            trust_witness_utxo: true,
            ..Default::default()
        }),
    );
    // Close the database before the folder goes away.
    drop(wallet);
    res
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
    fn check_mnemonic_matches_only_its_own_wallet() {
        let cfg = Config {
            network: "regtest".into(),
            ..Config::default()
        };
        let keys = new_keys(&cfg);
        assert!(check_mnemonic(&cfg, &keys.mnemonic, &keys.master_fingerprint).is_ok());
        assert!(matches!(
            check_mnemonic(&cfg, &keys.mnemonic, "00000000"),
            Err(rgb_lib::Error::FingerprintMismatch)
        ));
    }

    #[test]
    fn token_is_redacted_in_debug() {
        let cfg = Config {
            server_signing_token: Some("0123456789abcdef".into()),
            ..Config::default()
        };
        assert!(!format!("{cfg:?}").contains("0123456789abcdef"));
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

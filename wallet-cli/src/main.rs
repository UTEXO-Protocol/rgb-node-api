//! Dev-only companion CLI for manual `rgb-node-api` testing.
//!
//! The service is deliberately keyless: wallets are registered watch-only via
//! the `xpub-van` / `xpub-col` / `master-fingerprint` headers, and every
//! mutating flow is `*begin` -> client signs the PSBT -> `*end`. That makes the
//! `*begin`/`*end` pairs impossible to exercise from an HTTP client alone. This
//! binary supplies the missing client half: it derives the registration headers
//! from a mnemonic and signs PSBTs.
//!
//! Signing is stateless with respect to the transaction — a PSBT already
//! carries `witness_utxo` and `bip32_derivation` for each input, so nothing has
//! to be imported or exported per transaction.

use std::io::Read;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use rgb_lib::bdk_wallet::SignOptions;
use rgb_lib::keys::{Keys, WitnessVersion, generate_keys, restore_keys};
use rgb_lib::wallet::{DatabaseType, SinglesigKeys, Wallet, WalletData};
use rgb_lib::{AssetSchema, BitcoinNetwork};
use serde::Serialize;

/// Header names accepted by `POST /wallet/register`, kept in sync with
/// `rgb-node-api`'s `HEADER_*` constants.
const HEADER_XPUB_VAN: &str = "xpub-van";
const HEADER_XPUB_COL: &str = "xpub-col";
const HEADER_MASTER_FINGERPRINT: &str = "master-fingerprint";

/// The service builds its watch-only wallets with `witness_version:
/// Default::default()`, so this is not configurable — a mismatch would yield
/// signatures the service cannot validate, with no useful error.
const WITNESS_VERSION: WitnessVersion = WitnessVersion::Taproot;

/// Kept identical to the service's wallet setup so both sides derive the same
/// descriptors.
const MAX_ALLOCATIONS_PER_UTXO: u32 = 5;

#[derive(Parser)]
#[command(
    name = "wallet-cli",
    version,
    about = "Dev-only key and PSBT signing helper for rgb-node-api",
    long_about = None,
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Generate a fresh mnemonic and print the registration headers.
    New(KeyArgs),
    /// Re-derive the registration headers from an existing mnemonic.
    Restore(RestoreArgs),
    /// Sign a PSBT and print the signed PSBT.
    Sign(SignArgs),
}

#[derive(Args)]
struct KeyArgs {
    #[command(flatten)]
    network: NetworkArg,
    /// Output format.
    #[arg(long, short, value_enum, default_value_t = Format::Human)]
    format: Format,
}

#[derive(Args)]
struct RestoreArgs {
    #[command(flatten)]
    mnemonic: MnemonicArg,
    #[command(flatten)]
    network: NetworkArg,
    /// Output format.
    #[arg(long, short, value_enum, default_value_t = Format::Human)]
    format: Format,
}

#[derive(Args)]
struct SignArgs {
    #[command(flatten)]
    mnemonic: MnemonicArg,
    #[command(flatten)]
    network: NetworkArg,
    /// PSBT to sign, base64-encoded. Read from stdin when omitted.
    #[arg(long)]
    psbt: Option<String>,
    /// Scratch directory for the signing wallet. Created on first use and
    /// reused; it holds no funds and never needs to match the service's
    /// `data_dir`, since descriptors depend only on the keys and the network.
    #[arg(long, env = "RGB_CLI_DATA_DIR")]
    data_dir: Option<PathBuf>,
    /// Require `non_witness_utxo` on segwit inputs, matching BDK's default.
    /// PSBTs produced by the `*begin` endpoints usually carry only
    /// `witness_utxo`, so leaving this off is normally what you want.
    #[arg(long)]
    no_trust_witness_utxo: bool,
}

#[derive(Args)]
struct MnemonicArg {
    /// BIP39 mnemonic. Prefer the environment variable so the phrase does not
    /// land in your shell history.
    #[arg(long, short, env = "RGB_MNEMONIC", hide_env_values = true)]
    mnemonic: String,
}

#[derive(Args)]
struct NetworkArg {
    /// Bitcoin network the keys are derived for.
    #[arg(long, short, value_enum, env = "RGB_NETWORK", default_value_t = Network::Regtest)]
    network: Network,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum Network {
    Mainnet,
    Testnet,
    Testnet4,
    Signet,
    Regtest,
}

impl From<Network> for BitcoinNetwork {
    fn from(network: Network) -> Self {
        match network {
            Network::Mainnet => BitcoinNetwork::Mainnet,
            Network::Testnet => BitcoinNetwork::Testnet,
            Network::Testnet4 => BitcoinNetwork::Testnet4,
            Network::Signet => BitcoinNetwork::Signet,
            Network::Regtest => BitcoinNetwork::Regtest,
        }
    }
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum Format {
    /// Labelled block, including the mnemonic.
    Human,
    /// Bare `name: value` header lines, ready to paste into an HTTP client.
    Headers,
    /// Single JSON object.
    Json,
}

/// Everything a caller needs to register the wallet and to sign for it later.
#[derive(Serialize)]
struct KeyOutput {
    mnemonic: String,
    master_fingerprint: String,
    account_xpub_vanilla: String,
    account_xpub_colored: String,
    master_xpub: String,
}

impl From<Keys> for KeyOutput {
    fn from(keys: Keys) -> Self {
        Self {
            mnemonic: keys.mnemonic,
            master_fingerprint: keys.master_fingerprint,
            account_xpub_vanilla: keys.account_xpub_vanilla,
            account_xpub_colored: keys.account_xpub_colored,
            master_xpub: keys.xpub,
        }
    }
}

impl KeyOutput {
    fn print(&self, format: Format) -> Result<()> {
        match format {
            Format::Human => {
                println!("mnemonic:           {}", self.mnemonic);
                println!("master xpub:        {}", self.master_xpub);
                println!();
                println!("Registration headers for POST /wallet/register:");
                println!("  {HEADER_XPUB_VAN}: {}", self.account_xpub_vanilla);
                println!("  {HEADER_XPUB_COL}: {}", self.account_xpub_colored);
                println!("  {HEADER_MASTER_FINGERPRINT}: {}", self.master_fingerprint);
            }
            Format::Headers => {
                println!("{HEADER_XPUB_VAN}: {}", self.account_xpub_vanilla);
                println!("{HEADER_XPUB_COL}: {}", self.account_xpub_colored);
                println!("{HEADER_MASTER_FINGERPRINT}: {}", self.master_fingerprint);
            }
            Format::Json => println!("{}", serde_json::to_string_pretty(self)?),
        }
        Ok(())
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::New(args) => {
            let keys = generate_keys(args.network.network.into(), WITNESS_VERSION);
            KeyOutput::from(keys).print(args.format)
        }
        Command::Restore(args) => {
            let keys = restore_keys(
                args.network.network.into(),
                args.mnemonic.mnemonic,
                WITNESS_VERSION,
            )
            .context("failed to derive keys from the supplied mnemonic")?;
            KeyOutput::from(keys).print(args.format)
        }
        Command::Sign(args) => sign(args),
    }
}

fn sign(args: SignArgs) -> Result<()> {
    let psbt = match args.psbt {
        Some(psbt) => psbt,
        None => {
            let mut buf = String::new();
            std::io::stdin()
                .read_to_string(&mut buf)
                .context("failed to read PSBT from stdin")?;
            buf
        }
    };
    let psbt = psbt.trim().to_owned();
    if psbt.is_empty() {
        anyhow::bail!("no PSBT supplied — pass --psbt or pipe one on stdin");
    }

    let data_dir = args
        .data_dir
        .unwrap_or_else(|| std::env::temp_dir().join("rgb-wallet-cli"));

    let wallet = signing_wallet(
        &args.mnemonic.mnemonic,
        args.network.network.into(),
        &data_dir,
    )?;

    let sign_options = SignOptions {
        trust_witness_utxo: !args.no_trust_witness_utxo,
        ..Default::default()
    };
    let signed = wallet
        .sign_psbt(psbt, Some(sign_options))
        .context("failed to sign PSBT")?;

    println!("{signed}");
    Ok(())
}

/// Build a signing wallet from the mnemonic.
///
/// This is constructed once per invocation rather than per transaction — the
/// PSBT itself carries the per-input data the signer needs. The wallet exists
/// only so that the descriptors come from rgb-lib rather than being
/// reimplemented here, where they could silently drift on a dependency bump.
fn signing_wallet(mnemonic: &str, network: BitcoinNetwork, data_dir: &PathBuf) -> Result<Wallet> {
    let keys = restore_keys(network, mnemonic.to_owned(), WITNESS_VERSION)
        .context("failed to derive keys from the supplied mnemonic")?;

    std::fs::create_dir_all(data_dir)
        .with_context(|| format!("failed to create data dir {}", data_dir.display()))?;

    let wallet = Wallet::new(
        WalletData {
            data_dir: data_dir.to_string_lossy().into_owned(),
            bitcoin_network: network,
            database_type: DatabaseType::Sqlite,
            max_allocations_per_utxo: MAX_ALLOCATIONS_PER_UTXO,
            supported_schemas: vec![AssetSchema::Nia],
            reuse_addresses: false,
        },
        SinglesigKeys {
            account_xpub_colored: keys.account_xpub_colored,
            account_xpub_vanilla: keys.account_xpub_vanilla,
            mnemonic: Some(mnemonic.to_owned()),
            master_fingerprint: keys.master_fingerprint,
            vanilla_keychain: None,
            witness_version: WITNESS_VERSION,
        },
    )
    .context("failed to build signing wallet")?;

    Ok(wallet)
}

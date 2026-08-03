//! Classification of `rgb_lib::Error`.
//!
//! rgb-lib validates request data deep inside the wallet, so most of its error
//! variants are caused by what the caller sent, not by a failure on our side.
//! Reporting all of them as `500 internal_error` hides the real reason from the
//! client and pollutes the logs with errors nobody can act on. Every variant is
//! therefore mapped to a class here; the API layer turns the class into an HTTP
//! status + error code, and the wallet thread uses it to pick a log level.

use rgb_lib::Error;

/// What kind of failure an `rgb_lib::Error` represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorClass {
    /// The referenced entity does not exist.
    NotFound,
    /// Generic invalid request data.
    BadInput,
    /// The provided bitcoin address is invalid.
    InvalidAddress,
    /// Not enough bitcoins to fund the operation.
    NotEnoughBalance,
    /// Not enough colorable UTXOs, more need to be created.
    NeedMoreUtxos,
    /// Not enough assets to cover the requested assignments.
    NotEnoughAssets,
    /// The provided fee rate is out of the accepted range.
    InvalidFeeRate,
    /// The provided PSBT is invalid or cannot be processed.
    InvalidPsbt,
    /// The provided recipient / invoice / transport data is invalid.
    InvalidRecipient,
    /// The request conflicts with the current state.
    Conflict,
    /// The requested operation is not supported by this wallet.
    Unsupported,
    /// A dependency (indexer, proxy, network) is unavailable.
    Unavailable,
    /// Our fault: a bug, a broken database, a broken data directory.
    Internal,
}

impl ErrorClass {
    /// `true` when the caller can fix the request and retry.
    pub fn is_client_error(self) -> bool {
        !matches!(self, Self::Internal | Self::Unavailable)
    }
}

/// The name of the `rgb_lib::Error` variant, e.g. `InvalidFeeRate`.
///
/// Derived from the `Debug` representation, which always starts with the
/// variant name, so we don't have to keep a second 100+ arm match in sync.
pub fn error_kind(err: &Error) -> String {
    format!("{err:?}")
        .split(|c: char| !c.is_alphanumeric())
        .next()
        .unwrap_or("Unknown")
        .to_string()
}

/// Classify an `rgb_lib::Error`.
///
/// The match is deliberately exhaustive: rgb-lib is tracked from a git branch,
/// so an upgrade that adds a variant must fail to compile here instead of
/// silently landing in some catch-all bucket.
pub fn classify(err: &Error) -> ErrorClass {
    match err {
        // -- the entity asked for does not exist -----------------------------
        Error::AssetNotFound { .. }
        | Error::BatchTransferNotFound { .. }
        | Error::MultisigOperationNotFound { .. }
        | Error::UnknownTransfer { .. }
        | Error::NoConsignment
        | Error::VssBackupNotFound => ErrorClass::NotFound,

        // -- specific, actionable input problems -----------------------------
        Error::InvalidAddress { .. } => ErrorClass::InvalidAddress,
        Error::InsufficientBitcoins { .. } => ErrorClass::NotEnoughBalance,
        Error::InsufficientAllocationSlots => ErrorClass::NeedMoreUtxos,
        Error::InsufficientAssignments { .. } => ErrorClass::NotEnoughAssets,

        Error::InvalidFeeRate { .. } | Error::MaxFeeExceeded { .. } | Error::MinFeeNotMet { .. } => {
            ErrorClass::InvalidFeeRate
        }

        Error::InvalidPsbt { .. }
        | Error::CannotCombinePsbts
        | Error::CannotFinalizePsbt
        | Error::TooManySignaturesInPsbt
        | Error::PsbtInspection { .. } => ErrorClass::InvalidPsbt,

        Error::InvalidInvoice { .. }
        | Error::InvalidProxyProtocol { .. }
        | Error::InvalidRecipientData { .. }
        | Error::InvalidRecipientID
        | Error::InvalidRecipientMap
        | Error::InvalidRecipientNetwork
        | Error::InvalidTransportEndpoint { .. }
        | Error::InvalidTransportEndpoints { .. }
        | Error::NoValidTransportEndpoint
        | Error::RecipientIDDuplicated
        | Error::UnsupportedTransportType => ErrorClass::InvalidRecipient,

        // -- the request clashes with the current state ----------------------
        Error::AllocationsAlreadyAvailable
        | Error::CannotAbortPendingVanillaTx
        | Error::CannotChangeOnline
        | Error::CannotDeleteBatchTransfer
        | Error::CannotFailBatchTransfer
        | Error::FileAlreadyExists { .. }
        | Error::MultisigOperationInProgress
        | Error::MultisigTransferStatusMismatch
        | Error::RecipientIDAlreadyUsed
        | Error::WalletDirAlreadyExists { .. } => ErrorClass::Conflict,

        // -- asked for something this wallet cannot do -----------------------
        Error::AddressReuseDisabled
        | Error::CannotUseIfaOnMainnet
        | Error::NoSupportedSchemas
        | Error::UnsupportedBackupVersion { .. }
        | Error::UnsupportedBurn { .. }
        | Error::UnsupportedInflation { .. }
        | Error::UnsupportedLayer1 { .. }
        | Error::UnsupportedSchema { .. } => ErrorClass::Unsupported,

        // -- a dependency is down or unreachable -----------------------------
        Error::CannotEstimateFees
        | Error::FailedBdkSync { .. }
        | Error::FailedBroadcast { .. }
        | Error::FailedIssuance { .. }
        | Error::Indexer { .. }
        | Error::MpcProvider { .. }
        | Error::MultisigHubService { .. }
        | Error::Network { .. }
        | Error::Offline
        | Error::OnlineNeeded
        | Error::Proxy { .. }
        | Error::RejectListService { .. }
        | Error::RestClientBuild { .. }
        | Error::VssAuth { .. }
        | Error::VssError { .. }
        | Error::VssVersionConflict { .. } => ErrorClass::Unavailable,

        // -- our fault -------------------------------------------------------
        Error::Database { .. }
        | Error::Inconsistency { .. }
        | Error::InexistentDataDir
        | Error::Internal { .. }
        | Error::IO { .. }
        | Error::MultisigCannotMarkOperationProcessed { .. }
        | Error::MultisigCannotRespondToOperation { .. }
        | Error::MultisigUnexpectedData { .. }
        | Error::RestoredBackupInconsistent { .. }
        | Error::RgbInspection { .. }
        // The node keeps watch-only wallets on purpose (signing happens in the
        // client), so hitting a signing-only path is a bug on our side.
        | Error::WatchOnly => ErrorClass::Internal,

        // -- everything else is invalid request data -------------------------
        Error::BitcoinNetworkMismatch
        | Error::EmptyFile { .. }
        | Error::FingerprintMismatch
        | Error::InvalidAmountZero
        | Error::InvalidAssignment
        | Error::InvalidAttachments { .. }
        | Error::InvalidBitcoinKeys
        | Error::InvalidBitcoinNetwork { .. }
        | Error::InvalidColoringInfo { .. }
        | Error::InvalidConsignment
        | Error::InvalidContractLink { .. }
        | Error::InvalidCosigner { .. }
        | Error::InvalidDetails { .. }
        | Error::InvalidElectrum { .. }
        | Error::InvalidEstimationBlocks
        | Error::InvalidExpiration
        | Error::InvalidFilePath { .. }
        | Error::InvalidFingerprint
        | Error::InvalidIndexer { .. }
        | Error::InvalidMnemonic { .. }
        | Error::InvalidMultisigThreshold { .. }
        | Error::InvalidName { .. }
        | Error::InvalidPrecision { .. }
        | Error::InvalidPubkey { .. }
        | Error::InvalidRejectListUrl { .. }
        | Error::InvalidRightOutpoint { .. }
        | Error::InvalidTicker { .. }
        | Error::InvalidTxid
        | Error::InvalidVanillaKeychain
        | Error::InvalidWitnessVersion { .. }
        | Error::MultisigUserNotCosigner
        | Error::NoBurnAmount
        | Error::NoCosignersSupplied
        | Error::NoInflationAmounts
        | Error::NoIssuanceAmounts
        | Error::NoKeysSupplied
        | Error::OutputBelowDustLimit
        | Error::TooHighInflationAmounts
        | Error::TooHighIssuanceAmounts
        | Error::TooManyCosigners
        | Error::UnknownRgbSchema { .. }
        | Error::WrongPassword => ErrorClass::BadInput,
    }
}

/// Log a wallet failure at a level that matches its class.
///
/// Bad input from a client is a `warn`, not an `error`: it needs no operator
/// action and must not drown out real failures. Takes the error first, then
/// the usual `log!` arguments: `log_rgb_err!(err, "send begin: wid={id}")`.
#[macro_export]
macro_rules! log_rgb_err {
    ($err:expr, $($arg:tt)+) => {
        if $crate::wallet::classify(&$err).is_client_error() {
            log::warn!($($arg)+)
        } else {
            log::error!($($arg)+)
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bad_request_data_is_a_client_error() {
        let err = Error::InvalidFeeRate {
            details: "value under minimum 1".to_string(),
        };
        assert_eq!(classify(&err), ErrorClass::InvalidFeeRate);
        assert!(classify(&err).is_client_error());
        assert_eq!(error_kind(&err), "InvalidFeeRate");
    }

    #[test]
    fn our_failures_stay_internal() {
        let err = Error::Internal {
            details: "wallet thread is dead".to_string(),
        };
        assert_eq!(classify(&err), ErrorClass::Internal);
        assert!(!classify(&err).is_client_error());
    }

    #[test]
    fn unreachable_dependencies_are_not_client_errors() {
        let err = Error::Indexer {
            details: "connection refused".to_string(),
        };
        assert_eq!(classify(&err), ErrorClass::Unavailable);
        assert!(!classify(&err).is_client_error());
    }
}

mod common;
use common::*;
use rgb_lib::{BitcoinNetwork, bdk_wallet::KeychainKind, mpc::MpcWalletProvider};
use rgb_node_api::mpc::{MpcError, MpcService, Owner, Provider, RegistryProvider};

#[tokio::test]
async fn registration_invoice_isolation_and_restart() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    let service = MpcService::new(config.clone(), Some(TOKEN.into())).unwrap();
    let request = registration(7);
    let wallet_id = request.wallet_id;
    let view = service.register(owner(), request.clone()).await.unwrap();
    assert!(view.witness_receive);
    assert!(!view.signing);
    assert_eq!(view.supported_schemas, ["nia"]);
    assert_eq!(
        service
            .register(owner(), request.clone())
            .await
            .unwrap()
            .wallet_id,
        wallet_id
    );
    assert!(matches!(
        MpcService::new(config.clone(), Some(TOKEN.into())),
        Err(MpcError::Conflict(_))
    ));
    for foreign in [
        Owner {
            tenant_id: "other".into(),
            ..owner()
        },
        Owner {
            user_id: "bob".into(),
            ..owner()
        },
    ] {
        assert!(matches!(
            service.wallet(foreign.clone(), wallet_id).await,
            Err(MpcError::NotFound)
        ));
        assert!(matches!(
            service.witness(foreign, wallet_id, witness()).await,
            Err(MpcError::NotFound)
        ));
    }
    let mut changed = request.clone();
    changed.provider = Provider::DynamicEmbedded;
    assert!(matches!(
        service.register(owner(), changed).await,
        Err(MpcError::Conflict(_))
    ));
    let invoice_request = witness();
    let invoice = service
        .witness(owner(), wallet_id, invoice_request.clone())
        .await
        .unwrap();
    assert!(invoice.invoice.starts_with("rgb:"));
    assert_eq!(
        service
            .witness(owner(), wallet_id, invoice_request.clone())
            .await
            .unwrap()
            .invoice,
        invoice.invoice
    );
    let mut different = invoice_request.clone();
    different.amount = Some("26".into());
    assert!(matches!(
        service.witness(owner(), wallet_id, different).await,
        Err(MpcError::Conflict(_))
    ));
    let another = service
        .witness(owner(), wallet_id, witness())
        .await
        .unwrap();
    assert_ne!(
        another.invoice, invoice.invoice,
        "Pinned addresses need distinct transport nonces"
    );
    assert_eq!(
        service.transfers(owner(), wallet_id).await.unwrap().len(),
        2
    );
    // Unknown assets must not silently return the two unrelated witness receives.
    assert!(matches!(
        service
            .transfers_for_asset(owner(), wallet_id, Some("unknown-asset".into()))
            .await,
        Err(MpcError::Rgb(rgb_lib::Error::AssetNotFound { .. }))
    ));
    drop(service);
    let reopened = MpcService::new(config, Some(TOKEN.into())).unwrap();
    assert_eq!(
        reopened.wallet(owner(), wallet_id).await.unwrap().wallet_id,
        wallet_id
    );
    assert_eq!(
        reopened
            .witness(owner(), wallet_id, invoice_request)
            .await
            .unwrap()
            .invoice,
        invoice.invoice
    );
    assert_eq!(
        reopened.transfers(owner(), wallet_id).await.unwrap().len(),
        2
    );
    let manifest = std::fs::read_to_string(
        dir.path()
            .join(format!("mpc/registrations/{wallet_id}.json")),
    )
    .unwrap();
    for forbidden in ["mnemonic", "private_key", TOKEN] {
        assert!(!manifest.contains(forbidden));
    }
}

#[tokio::test]
async fn invalid_network_key_binding_duplicate_registration_and_expiry_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let service = MpcService::new(config(dir.path()), Some(TOKEN.into())).unwrap();
    let request = registration(17);
    let mut invalid = request.clone();
    invalid.genesis_hash = "a".repeat(64);
    assert!(matches!(
        service.register(owner(), invalid).await,
        Err(MpcError::Invalid(_))
    ));
    let mut invalid = request.clone();
    invalid.bitcoin_network = "testnet".into();
    assert!(matches!(
        service.register(owner(), invalid).await,
        Err(MpcError::Invalid(_))
    ));
    let mut invalid = request.clone();
    invalid.addresses[0].public_key = invalid.addresses[1].public_key.clone();
    assert!(matches!(
        service.register(owner(), invalid).await,
        Err(MpcError::Invalid(_))
    ));
    service.register(owner(), request.clone()).await.unwrap();
    let mut duplicate = request.clone();
    duplicate.wallet_id = uuid::Uuid::new_v4();
    duplicate.provider_wallet_ref = "another".into();
    duplicate.addresses[0].address = duplicate.addresses[0].address.to_uppercase();
    assert!(matches!(
        service.register(owner(), duplicate).await,
        Err(MpcError::Conflict(_))
    ));
    for amount in ["0", "01", "-1", "1.5", "18446744073709551616"] {
        let mut invoice = witness();
        invoice.amount = Some(amount.into());
        assert!(matches!(
            service.witness(owner(), request.wallet_id, invoice).await,
            Err(MpcError::Invalid(_))
        ));
    }
    let mut expired = witness();
    expired.expiration_timestamp = 1;
    assert!(matches!(
        service.witness(owner(), request.wallet_id, expired).await,
        Err(MpcError::Invalid(_))
    ));
    let registry = RegistryProvider::new(BitcoinNetwork::Regtest, request.addresses).unwrap();
    assert!(
        registry
            .create_address(BitcoinNetwork::Mainnet, KeychainKind::External, 0)
            .is_err()
    );
    assert!(
        registry
            .create_address(BitcoinNetwork::Regtest, KeychainKind::External, 1)
            .is_err()
    );
}

#[tokio::test]
async fn receiving_does_not_require_a_second_provider_address() {
    let dir = tempfile::tempdir().unwrap();
    let service = MpcService::new(config(dir.path()), Some(TOKEN.into())).unwrap();
    let mut registration = registration(57);
    registration
        .addresses
        .retain(|address| address.role == rgb_node_api::mpc::Role::Rgb);
    let wallet = service.register(owner(), registration).await.unwrap();
    assert_eq!(wallet.addresses.len(), 1);
    service
        .witness(owner(), wallet.wallet_id, witness())
        .await
        .unwrap();
    assert_eq!(
        service
            .transfers(owner(), wallet.wallet_id)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn an_interrupted_invoice_recovers_the_same_persisted_invoice() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path());
    let service = MpcService::new(cfg.clone(), Some(TOKEN.into())).unwrap();
    let registration = registration(27);
    let id = registration.wallet_id;
    service.register(owner(), registration).await.unwrap();
    let request = witness();
    let original = service.witness(owner(), id, request.clone()).await.unwrap();
    drop(service);
    let path = dir.path().join(format!("mpc/registrations/{id}.json"));
    let mut record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    record["invoices"][request.request_id.to_string()]["result"] = serde_json::Value::Null;
    std::fs::write(path, serde_json::to_vec(&record).unwrap()).unwrap();
    let reopened = MpcService::new(cfg, Some(TOKEN.into())).unwrap();
    let recovered = reopened.witness(owner(), id, request).await.unwrap();
    assert_eq!(recovered.invoice, original.invoice);
    assert_eq!(recovered.batch_transfer_idx, original.batch_transfer_idx);
    assert_eq!(reopened.transfers(owner(), id).await.unwrap().len(), 1);
}

#[tokio::test]
async fn failed_invoice_can_retry_the_same_request_after_transport_repair() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config(dir.path());
    cfg.proxy_address = vec!["invalid-transport".into()];
    let service = MpcService::new(cfg, Some(TOKEN.into())).unwrap();
    let registration = registration(28);
    let id = registration.wallet_id;
    service.register(owner(), registration).await.unwrap();
    let request = witness();
    assert!(service.witness(owner(), id, request.clone()).await.is_err());
    assert!(service.transfers(owner(), id).await.unwrap().is_empty());
    let path = dir.path().join(format!("mpc/registrations/{id}.json"));
    let record: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(
        record["invoices"][request.request_id.to_string()]["failed_before_receive"],
        true
    );
    drop(service);
    let reopened = MpcService::new(config(dir.path()), Some(TOKEN.into())).unwrap();
    let recovered = reopened
        .witness(owner(), id, request.clone())
        .await
        .unwrap();
    assert_eq!(recovered.request_id, request.request_id);
    assert_eq!(
        reopened
            .witness(owner(), id, request)
            .await
            .unwrap()
            .invoice,
        recovered.invoice
    );
    assert_eq!(reopened.transfers(owner(), id).await.unwrap().len(), 1);
}

#[tokio::test]
async fn corrupt_records_block_new_bindings_but_not_existing_wallet_reads() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path());
    let service = MpcService::new(cfg.clone(), Some(TOKEN.into())).unwrap();
    let registration = registration(27);
    let id = registration.wallet_id;
    service.register(owner(), registration).await.unwrap();
    let request = witness();
    service.witness(owner(), id, request.clone()).await.unwrap();
    drop(service);
    let root = dir.path().join("mpc/registrations");
    let path = root.join(format!("{id}.json"));
    let mut record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let pending = record["invoices"][request.request_id.to_string()]
        .as_object_mut()
        .unwrap();
    pending.insert("result".into(), serde_json::Value::Null);
    pending.remove("prior_batch_ids");
    std::fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
    let service = MpcService::new(cfg, Some(TOKEN.into())).unwrap();
    assert!(matches!(
        service.witness(owner(), id, request).await,
        Err(MpcError::Conflict(_))
    ));
    assert!(matches!(
        service.witness(owner(), id, witness()).await,
        Err(MpcError::Conflict(_))
    ));
    assert_eq!(service.transfers(owner(), id).await.unwrap().len(), 1);
    std::fs::write(root.join("notes.json"), b"not a registration").unwrap();
    let second = common::registration(37);
    service.register(owner(), second.clone()).await.unwrap();
    std::fs::write(
        root.join(format!("{}.json", uuid::Uuid::new_v4())),
        b"damaged registration",
    )
    .unwrap();
    assert_eq!(
        service
            .wallet(owner(), second.wallet_id)
            .await
            .unwrap()
            .wallet_id,
        second.wallet_id
    );
    assert!(matches!(
        service.register(owner(), common::registration(47)).await,
        Err(MpcError::Conflict(_))
    ));
}

#[tokio::test]
// DYNAMIC_EMBEDDED_POC: cancellation must preserve ambiguous signed operations.
async fn cancellation_never_discards_an_unknown_submission_or_foreign_operation() {
    let dir = tempfile::tempdir().unwrap();
    let service = MpcService::new(config(dir.path()), Some(TOKEN.into())).unwrap();
    let wallet = registration(47);
    let id = wallet.wallet_id;
    service.register(owner(), wallet).await.unwrap();
    let request = uuid::Uuid::new_v4();
    let root = dir.path().join(format!("mpc/sends/{id}"));
    std::fs::create_dir_all(&root).unwrap();
    let journal = serde_json::json!({
        "request": { "request_id": request, "invoice": "saved-invoice", "amount": "1" },
        "view": { "wallet_id": id, "request_id": request, "state": "SUBMITTING", "amount": "1", "invoice": "saved-invoice",
            "psbt": null, "txid": null, "fee_sat": null, "input_indexes": [], "batch_transfer_idx": null },
        "original_psbt": "saved-original"
    });
    let path = root.join(format!("{request}.json"));
    let original = serde_json::to_vec(&journal).unwrap();
    std::fs::write(&path, &original).unwrap();
    assert!(matches!(
        service
            .cancel_send(
                Owner {
                    user_id: "bob".into(),
                    ..owner()
                },
                id,
                request
            )
            .await,
        Err(MpcError::NotFound)
    ));
    assert!(matches!(
        service.cancel_send(owner(), id, request).await,
        Err(MpcError::Conflict(_))
    ));
    assert_eq!(std::fs::read(&path).unwrap(), original);
    let mut terminal = journal;
    terminal["view"]["state"] = "FAILED".into();
    terminal["submission_started"] = true.into();
    let terminal = serde_json::to_vec(&terminal).unwrap();
    std::fs::write(&path, &terminal).unwrap();
    assert_eq!(
        service
            .cancel_send(owner(), id, request)
            .await
            .unwrap()
            .state,
        "FAILED"
    );
    assert_eq!(std::fs::read(path).unwrap(), terminal);
}

#[test]
fn disabled_and_unauthorized_access_cannot_reach_the_registry() {
    let dir = tempfile::tempdir().unwrap();
    let disabled = MpcService::new(config(dir.path()), None).unwrap();
    assert!(matches!(
        disabled.authorize(None, owner()),
        Err(MpcError::Disabled)
    ));
    let service = MpcService::new(config(dir.path()), Some(TOKEN.into())).unwrap();
    for token in [None, Some("Bearer wrong"), Some(TOKEN)] {
        assert!(matches!(
            service.authorize(token, owner()),
            Err(MpcError::Unauthorized)
        ));
    }
    assert_eq!(
        service
            .authorize(Some(&format!("Bearer {TOKEN}")), owner())
            .unwrap(),
        owner()
    );
}

#[test]
fn a_provider_with_inconsistent_address_metadata_cannot_create_an_invoice() {
    use rgb_lib::{
        AssetSchema, Assignment,
        bitcoin::{Psbt, ScriptBuf, Transaction, absolute, transaction},
        mpc::MpcAddressInfo,
        wallet::{AssetFilter, DatabaseType, MpcWallet, RgbWalletOpsOffline, WalletData},
    };
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    struct Provider {
        inner: RegistryProvider,
        broken: Arc<AtomicBool>,
    }
    impl MpcWalletProvider for Provider {
        fn create_address(
            &self,
            network: BitcoinNetwork,
            keychain: KeychainKind,
            index: u32,
        ) -> Result<MpcAddressInfo, rgb_lib::Error> {
            assert_eq!(
                index, 0,
                "A failed invoice must not consume an address index"
            );
            let mut address = self.inner.create_address(network, keychain, index)?;
            if self.broken.load(Ordering::SeqCst) {
                address.script_pubkey = ScriptBuf::new();
            }
            Ok(address)
        }
        fn sign_psbt(&self, psbt: Psbt, keys: Vec<String>) -> Result<Psbt, rgb_lib::Error> {
            self.inner.sign_psbt(psbt, keys)
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let inner = RegistryProvider::new(BitcoinNetwork::Regtest, registration(37).addresses).unwrap();
    let psbt = Psbt::from_unsigned_tx(Transaction {
        version: transaction::Version::TWO,
        lock_time: absolute::LockTime::ZERO,
        input: vec![],
        output: vec![],
    })
    .unwrap();
    assert!(matches!(
        inner.sign_psbt(psbt, vec![]),
        Err(rgb_lib::Error::WatchOnly)
    ));
    let broken = Arc::new(AtomicBool::new(true));
    let provider = Provider {
        inner,
        broken: broken.clone(),
    };
    let mut wallet = MpcWallet::new(
        WalletData {
            data_dir: dir.path().to_string_lossy().into(),
            bitcoin_network: BitcoinNetwork::Regtest,
            database_type: DatabaseType::Sqlite,
            max_allocations_per_utxo: 5,
            supported_schemas: vec![AssetSchema::Nia],
            reuse_addresses: true,
        },
        uuid::Uuid::new_v4().to_string(),
        Box::new(provider),
    )
    .unwrap();
    let deadline = witness().expiration_timestamp;
    let endpoints = config(dir.path()).proxy_address;
    assert!(matches!(
        wallet.witness_receive(
            None,
            Assignment::Fungible(25),
            deadline,
            endpoints.clone(),
            1
        ),
        Err(rgb_lib::Error::MpcProvider { .. })
    ));
    assert!(
        wallet
            .list_transfers(AssetFilter::AnyOrNone, None)
            .unwrap()
            .is_empty()
    );
    broken.store(false, Ordering::SeqCst);
    wallet
        .witness_receive(None, Assignment::Fungible(25), deadline, endpoints, 1)
        .unwrap();
    assert_eq!(
        wallet
            .list_transfers(AssetFilter::AnyOrNone, None)
            .unwrap()
            .len(),
        1
    );
}

// DYNAMIC_EMBEDDED_POC: send ownership/provider boundary; no network/signing.
#[tokio::test]
async fn dynamic_return_cannot_access_another_owner_or_a_vault_wallet() {
    use rgb_node_api::mpc::SendRequest;
    let dir = tempfile::tempdir().unwrap();
    let service = MpcService::new(config(dir.path()), Some(TOKEN.into())).unwrap();
    let wallet = registration(17);
    let id = wallet.wallet_id;
    service.register(owner(), wallet).await.unwrap();
    let request = SendRequest {
        request_id: uuid::Uuid::new_v4(),
        invoice: "untrusted".into(),
        amount: "1".into(),
    };
    let foreign = Owner {
        user_id: "bob".into(),
        ..owner()
    };
    assert!(matches!(
        service
            .prepare_send(foreign.clone(), id, request.clone())
            .await,
        Err(MpcError::NotFound)
    ));
    assert!(matches!(
        service
            .finish_send(foreign.clone(), id, request.request_id, "untrusted".into())
            .await,
        Err(MpcError::NotFound)
    ));
    assert!(matches!(
        service.send_status(foreign, id, request.request_id).await,
        Err(MpcError::NotFound)
    ));
    assert!(matches!(
        service.prepare_send(owner(), id, request).await,
        Err(MpcError::Invalid(_))
    ));
}

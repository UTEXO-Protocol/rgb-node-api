mod common;
use common::*;
use rgb_lib::BitcoinNetwork;
use rgb_node_api::mpc::{MpcService, RegistryProvider, SendRequest};

#[test]
fn two_distinct_tweaked_roles_are_required() {
    let pair = registration(24).addresses;
    assert!(RegistryProvider::new(BitcoinNetwork::Regtest, pair.clone()).is_ok());
    assert!(RegistryProvider::new(BitcoinNetwork::Signet, pair.clone()).is_err());
    assert!(RegistryProvider::new(BitcoinNetwork::Regtest, vec![pair[0].clone()]).is_err());
    let mut wrong = pair.clone();
    wrong[1].signing_key_id = wrong[0].signing_key_id.clone();
    assert!(RegistryProvider::new(BitcoinNetwork::Regtest, wrong).is_err());
    let mut wrong = pair;
    wrong[0].internal_key = wrong[0].public_key.clone();
    assert!(RegistryProvider::new(BitcoinNetwork::Regtest, wrong).is_err());
}

#[tokio::test]
async fn invalid_prepare_is_durably_rejected_without_reserving_funds() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path());
    let request = registration(25);
    let id = request.wallet_id;
    let service = MpcService::new(cfg.clone(), Some(TOKEN.into())).unwrap();
    service.register(owner(), request).await.unwrap();
    let request = SendRequest {
        request_id: uuid::Uuid::new_v4(),
        invoice: "invalid-invoice".into(),
        amount: "25".into(),
        asset_id: "rgb:invalid".into(),
    };
    let rejected = service
        .prepare_send(owner(), id, request.clone())
        .await
        .unwrap();
    assert_eq!(rejected.state, "FAILED");
    assert!(rejected.psbt.is_none());
    assert!(rejected.txid.is_none());
    drop(service);
    let service = MpcService::new(cfg, Some(TOKEN.into())).unwrap();
    assert_eq!(
        service
            .prepare_send(owner(), id, request.clone())
            .await
            .unwrap()
            .state,
        "FAILED"
    );
    let mut changed = request;
    changed.amount = "24".into();
    assert!(service.prepare_send(owner(), id, changed).await.is_err());
}

#[tokio::test]
async fn registration_survives_restart_and_preserves_owner_isolation() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path());
    let request = registration(26);
    let id = request.wallet_id;
    let service = MpcService::new(cfg.clone(), Some(TOKEN.into())).unwrap();
    service.register(owner(), request.clone()).await.unwrap();
    drop(service);
    let service = MpcService::new(cfg, Some(TOKEN.into())).unwrap();
    let view = service.wallet(owner(), id).await.unwrap();
    assert!(view.blind_receive);
    assert!(!view.witness_receive);
    assert_eq!(view.addresses.len(), 2);
    let other = rgb_node_api::mpc::Owner {
        tenant_id: "poc".into(),
        user_id: "bob".into(),
    };
    assert!(service.wallet(other, id).await.is_err());
}

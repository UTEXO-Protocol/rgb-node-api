mod common;
use common::*;
use rgb_lib::BitcoinNetwork;
use rgb_node_api::mpc::{MpcService, RegistryProvider, SendRequest};
use serde_json::Value;

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
async fn first_asset_invoice_is_bound_and_recovered_after_lost_response() {
    const ASSET: &str = "rgb:7LVhcazJ-nasAUZr-82RcIkB-OMFsRsS-X5~BbjR-KMkOKdc";
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path());
    let request = registration(26);
    let id = request.wallet_id;
    let service = MpcService::new(cfg.clone(), Some(TOKEN.into())).unwrap();
    service.register(owner(), request).await.unwrap();
    let mut request = witness();
    request.asset_id = Some(ASSET.into());
    let first = service.witness(owner(), id, request.clone()).await.unwrap();
    let data = rgb_lib::wallet::Invoice::new(first.invoice.clone())
        .unwrap()
        .invoice_data();
    assert_eq!(data.asset_id.as_deref(), Some(ASSET));
    assert_eq!(data.asset_schema, Some(rgb_lib::AssetSchema::Nia));
    drop(service);
    let path = dir.path().join(format!("mpc/registrations/{id}.json"));
    let mut saved: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    saved["invoices"][request.request_id.to_string()]["result"] = Value::Null;
    std::fs::write(path, serde_json::to_vec(&saved).unwrap()).unwrap();
    let service = MpcService::new(cfg, Some(TOKEN.into())).unwrap();
    assert_eq!(
        service.witness(owner(), id, request).await.unwrap().invoice,
        first.invoice
    );
    let other = rgb_node_api::mpc::Owner {
        tenant_id: "poc".into(),
        user_id: "bob".into(),
    };
    assert!(service.wallet(other, id).await.is_err());
}

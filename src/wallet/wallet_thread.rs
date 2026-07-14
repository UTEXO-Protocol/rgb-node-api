use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::RwLock;

use rgb_lib::wallet;
use rgb_lib::wallet::{RgbWalletOpsOffline, RgbWalletOpsOnline};
use tokio::sync::{mpsc, oneshot};

use super::Config;
use super::RgbWalletState;
use super::models::*;

type ResultChan<T> = oneshot::Sender<RResult<T>>;

pub(crate) enum WalletCmd {
    Refresh {},
    RefreshWallet {
        resp: ResultChan<()>,
    },
    DropWallet {
        resp: ResultChan<()>,
    },
    Stop {
        resp: ResultChan<()>,
    },
    WalletInfo {
        resp: ResultChan<Info>,
    },
    Backup {
        password: String,
        resp: ResultChan<BackupPath>,
    },
    Address {
        resp: ResultChan<AddressRes>,
    },
    Assets {
        resp: ResultChan<wallet::Assets>,
    },
    Transfers {
        resp: ResultChan<Vec<wallet::Transfer>>,
    },
    FailTransfer {
        batch_transfer_idx: Option<i32>,
        no_asset_only: bool,
        skip_sync: bool,
        resp: ResultChan<bool>,
    },
    TransfersByAsset {
        asset_id: String,
        resp: ResultChan<Vec<wallet::Transfer>>,
    },
    TransfersByRecipient {
        recipient_id: String,
        resp: ResultChan<Option<wallet::Transfer>>,
    },
    Transactions {
        resp: ResultChan<Vec<wallet::Transaction>>,
    },
    ListUnspends {
        resp: ResultChan<Vec<wallet::Unspent>>,
    },
    AssetInfo {
        asset_id: String,
        resp: ResultChan<wallet::Metadata>,
    },
    BtcBalance {
        resp: ResultChan<wallet::BtcBalance>,
    },
    TokenBalance {
        asset_id: String,
        resp: ResultChan<wallet::Balance>,
    },
    Receive {
        req: ReceiveReq,
        resp: ResultChan<wallet::ReceiveData>,
    },
    IssueNiaToken {
        req: IssueNiaReq,
        resp: ResultChan<wallet::AssetNIA>,
    },
    CreateUtxoBegin {
        req: CreateUtxoBeginReq,
        resp: ResultChan<String>,
    },
    CreateUtxoEnd {
        psbt: String,
        resp: ResultChan<usize>,
    },
    SendBegin {
        req: SendBeginReq,
        resp: ResultChan<String>,
    },
    SendEnd {
        psbt: String,
        resp: ResultChan<wallet::OperationResult>,
    },
    SendBtcBegin {
        req: SendBtcReq,
        resp: ResultChan<String>,
    },
    SendBtcEnd {
        psbt: String,
        resp: ResultChan<String>,
    },
    AddReadOnlyWallet {
        xpub_colored: String,
        xpub_vanilla: String,
        master_fingerprint: String, // wallet id
        resp: ResultChan<bool>,
    },
}

impl std::fmt::Display for WalletCmd {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            WalletCmd::Refresh { .. } => "Refresh",
            WalletCmd::RefreshWallet { .. } => "RefreshWallet",
            WalletCmd::DropWallet { .. } => "DropWallet",
            WalletCmd::Stop { .. } => "Stop",
            WalletCmd::WalletInfo { .. } => "WalletInfo",
            WalletCmd::Backup { .. } => "Backup",
            WalletCmd::Address { .. } => "Address",
            WalletCmd::Assets { .. } => "Assets",
            WalletCmd::Transfers { .. } => "Transfers",
            WalletCmd::FailTransfer { .. } => "FailTransfer",
            WalletCmd::TransfersByAsset { .. } => "TransfersByAsset",
            WalletCmd::TransfersByRecipient { .. } => "TransfersByRecipient",
            WalletCmd::Transactions { .. } => "Transactions",
            WalletCmd::ListUnspends { .. } => "ListUnspends",
            WalletCmd::AssetInfo { .. } => "AssetInfo",
            WalletCmd::BtcBalance { .. } => "BtcBalance",
            WalletCmd::TokenBalance { .. } => "TokenBalance",
            WalletCmd::Receive { .. } => "Receive",
            WalletCmd::IssueNiaToken { .. } => "IssueNiaToken",
            WalletCmd::CreateUtxoBegin { .. } => "CreateUtxoBegin",
            WalletCmd::CreateUtxoEnd { .. } => "CreateUtxoEnd",
            WalletCmd::SendBegin { .. } => "SendBegin",
            WalletCmd::SendEnd { .. } => "SendEnd",
            WalletCmd::SendBtcBegin { .. } => "SendBtcBegin",
            WalletCmd::SendBtcEnd { .. } => "SendBtcEnd",
            WalletCmd::AddReadOnlyWallet { .. } => "AddReadOnlyWallet",
        };
        write!(f, "{}", name)
    }
}

pub(crate) struct WalletThread {
    pub(crate) cfg: Config,
    pub(crate) rx: mpsc::Receiver<(Option<String>, WalletCmd)>,
}

impl WalletThread {
    pub(crate) fn new(cfg: Config) -> (Self, mpsc::Sender<(Option<String>, WalletCmd)>) {
        let (tx, rx) = mpsc::channel::<(Option<String>, WalletCmd)>(256);

        let wt = WalletThread { cfg, rx };
        (wt, tx)
    }

    pub(crate) fn spawn(mut self) {
        log::info!("Starting wallet manager");

        let multi_wstate = Arc::new(RwLock::new(
            BTreeMap::<String, Arc<Mutex<RgbWalletState>>>::new(),
        ));

        while let Some((ido, cmd)) = self.rx.blocking_recv() {
            let cfg_base = self.cfg.clone();
            log::debug!("new command: wid={:?} cmd={}", ido, cmd);

            match cmd {
                // --- Commands that MODIFY the Map (Write Lock) ---
                WalletCmd::AddReadOnlyWallet {
                    xpub_colored,
                    xpub_vanilla,
                    master_fingerprint,
                    resp,
                } => {
                    if multi_wstate
                        .read()
                        .unwrap()
                        .contains_key(&master_fingerprint)
                    {
                        let _ = resp.send(Ok(true));
                        continue;
                    }

                    let w = RgbWalletState::new_ro_wallet(
                        &cfg_base,
                        xpub_colored,
                        xpub_vanilla,
                        master_fingerprint.clone(),
                        None,
                    );
                    match w {
                        Ok(wallet) => {
                            multi_wstate
                                .write()
                                .unwrap()
                                .insert(master_fingerprint, Arc::new(Mutex::new(wallet)));
                            let _ = resp.send(Ok(true));
                        }
                        Err(err) => {
                            let _ = resp.send(Err(err));
                        }
                    }
                }

                WalletCmd::WalletInfo { resp } => {
                    // Clone the Arcs out of the map to release the Read lock immediately
                    let wallets: Vec<_> = multi_wstate
                        .read()
                        .unwrap()
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect();

                    let mut info = Info {
                        status: true,
                        wallets: Vec::new(),
                    };
                    for (id, wallet_lock) in wallets {
                        let wallet = wallet_lock.lock().unwrap();

                        info.wallets.push(WalletInfo {
                            wallet_id: id.clone(),
                            master_xpub: wallet.master_xpub.clone(),
                            fingerprint: wallet.id.clone(),
                            // this service only holds watch-only wallets
                            read_only: true,
                        });
                    }
                    let _ = resp.send(Ok(info));
                }

                WalletCmd::DropWallet { resp } => {
                    if let Some(id) = &ido {
                        multi_wstate.write().unwrap().remove(id);
                        log::info!("dropped wallet: wid={id}");
                    }
                    let _ = resp.send(Ok(()));
                }

                WalletCmd::Stop { resp } => {
                    multi_wstate.write().unwrap().clear();
                    let _ = resp.send(Ok(()));
                    return; // Shutdown loop
                }

                WalletCmd::Refresh {} => {
                    // Clone the Arcs out of the map to release the Read lock immediately
                    let wallets: Vec<_> = multi_wstate
                        .read()
                        .unwrap()
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect();

                    for (id, wallet_lock) in wallets {
                        let mut wallet = wallet_lock.lock().unwrap();
                        if let Err(err) = wallet.refresh() {
                            log::error!("Refresh failed for {id}: {err:?}");
                        }
                    }
                }

                // --- All other commands (Address, etc.) ---
                _ => {
                    let id = match ido {
                        Some(id) => id,
                        None => {
                            log::error!("Command {} requires a wallet ID", cmd);
                            continue;
                        }
                    };

                    let wallet_opt = multi_wstate.read().unwrap().get(&id).cloned();
                    if let Some(wallet_state) = wallet_opt {
                        handle_wallet_cmd(wallet_state, id.clone(), cmd);
                    } else {
                        log::warn!("Wallet not found: {id}");
                    }
                }
            }
        }
    }
}

fn handle_wallet_cmd(wallet_state: Arc<Mutex<RgbWalletState>>, id: String, cmd: WalletCmd) {
    log::debug!("handle cmd: wid={}, cmd={}", id, cmd);
    match cmd {
        WalletCmd::Backup { password, resp } => {
            let filename = format!("rgb-wallet.{}.rgb-lib_backup", id);
            let backup_path = std::env::temp_dir().join(&filename);
            let backup_path = backup_path.to_str().unwrap_or_default();

            let _ = std::fs::remove_file(backup_path);

            log::info!("backup path: {}", backup_path);

            match wallet_state
                .lock()
                .unwrap()
                .wallet
                .backup(backup_path, &password)
            {
                Ok(_) => resp
                    .send(Ok(BackupPath {
                        filename,
                        path: backup_path.into(),
                    }))
                    .is_ok(),
                Err(err) => {
                    log::error!("get wallet backup : wid={id} error={:?}", err);
                    resp.send(Err(err)).is_ok()
                }
            }
        }
        WalletCmd::Address { resp } => match wallet_state.lock().unwrap().wallet.get_address() {
            Ok(a) => resp.send(Ok(AddressRes { address: a })).is_ok(),
            Err(err) => {
                log::error!("get address: wid={id} error={:?}", err);
                resp.send(Err(err)).is_ok()
            }
        },
        WalletCmd::FailTransfer {
            batch_transfer_idx,
            no_asset_only,
            skip_sync,
            resp,
        } => {
            let mut w = wallet_state.lock().unwrap();
            let wo = w.wallet_online;
            match w
                .wallet
                .fail_transfers(wo, batch_transfer_idx, no_asset_only, skip_sync)
            {
                Ok(val) => resp.send(Ok(val)).is_ok(),
                Err(err) => {
                    log::error!("fail transfer: wid={id} error={:?}", err);
                    resp.send(Err(err)).is_ok()
                }
            }
        }
        WalletCmd::Transfers { resp } => {
            match wallet_state.lock().unwrap().wallet.list_transfers(None) {
                Ok(val) => resp.send(Ok(val)).is_ok(),
                Err(err) => {
                    log::error!("list transfers: wid={id} error={:?}", err);
                    resp.send(Err(err)).is_ok()
                }
            }
        }
        WalletCmd::TransfersByAsset { asset_id, resp } => {
            match wallet_state
                .lock()
                .unwrap()
                .wallet
                .list_transfers(Some(asset_id.clone()))
            {
                Ok(val) => resp.send(Ok(val)).is_ok(),
                Err(err) => {
                    log::error!(
                        "list transfers by asset: wid={id} asset={asset_id} error={:?}",
                        err
                    );
                    resp.send(Err(err)).is_ok()
                }
            }
        }

        WalletCmd::TransfersByRecipient { recipient_id, resp } => {
            match wallet_state
                .lock()
                .unwrap()
                .wallet
                .list_transfers(None)
                .map(|transfers| {
                    transfers
                        .into_iter()
                        .find(|t| t.recipient_id.as_deref() == Some(recipient_id.as_str()))
                }) {
                Ok(val) => resp.send(Ok(val)).is_ok(),
                Err(err) => {
                    log::error!(
                        "list transfers by asset: wid={id} recipient={recipient_id} error={:?}",
                        err
                    );
                    resp.send(Err(err)).is_ok()
                }
            }
        }

        WalletCmd::Transactions { resp } => {
            let mut w = wallet_state.lock().unwrap();
            let wo = w.wallet_online;
            match w.wallet.list_transactions(Some(wo), false) {
                Ok(val) => resp.send(Ok(val)).is_ok(),
                Err(err) => {
                    log::error!("list transactions: wid={id} error={:?}", err);
                    resp.send(Err(err)).is_ok()
                }
            }
        }

        WalletCmd::ListUnspends { resp } => {
            let mut w = wallet_state.lock().unwrap();
            let wo = w.wallet_online;
            match w.wallet.list_unspents(Some(wo), false, false) {
                Ok(val) => resp.send(Ok(val)).is_ok(),
                Err(err) => {
                    log::error!("list unspends: wid={id} error={:?}", err);
                    resp.send(Err(err)).is_ok()
                }
            }
        }
        WalletCmd::Assets { resp } => {
            match wallet_state.lock().unwrap().wallet.list_assets(Vec::new()) {
                Ok(val) => resp.send(Ok(val)).is_ok(),
                Err(err) => {
                    log::error!("list assets: wid={id} error={:?}", err);
                    resp.send(Err(err)).is_ok()
                }
            }
        }
        WalletCmd::AssetInfo { asset_id, resp } => {
            match wallet_state
                .lock()
                .unwrap()
                .wallet
                .get_asset_metadata(asset_id.clone())
            {
                Ok(val) => resp.send(Ok(val)).is_ok(),
                Err(err) => {
                    log::error!("get asset: wid={id} asset={asset_id} error={:?}", err);
                    resp.send(Err(err)).is_ok()
                }
            }
        }
        WalletCmd::BtcBalance { resp } => {
            let mut w = wallet_state.lock().unwrap();

            match w.balance_btc() {
                Ok(val) => resp.send(Ok(val)).is_ok(),
                Err(err) => {
                    log::error!("btc balance: wid={id} error={:?}", err);
                    resp.send(Err(err)).is_ok()
                }
            }
        }
        WalletCmd::TokenBalance { asset_id, resp } => {
            match wallet_state.lock().unwrap().balance_token(&asset_id) {
                Ok(val) => resp.send(Ok(val)).is_ok(),
                Err(err) => {
                    log::error!("token balance: wid={id} asset={asset_id} error={:?}", err);
                    resp.send(Err(err)).is_ok()
                }
            }
        }
        WalletCmd::Receive { req, resp } => {
            let assignment = match req.amount {
                Some(amount) => rgb_lib::Assignment::Fungible(amount),
                None => rgb_lib::Assignment::Any,
            };
            match wallet_state.lock().unwrap().blind_receive_token(
                req.asset_id,
                assignment,
                req.duration_seconds,
                req.min_confirmations,
            ) {
                Ok(val) => resp.send(Ok(val)).is_ok(),
                Err(err) => {
                    log::error!("blind recieve: wid={id} error={:?}", err);
                    resp.send(Err(err)).is_ok()
                }
            }
        }
        WalletCmd::CreateUtxoBegin { req, resp } => {
            let mut w = wallet_state.lock().unwrap();
            let wo = w.wallet_online;
            match w.wallet.create_utxos_begin(
                wo,
                req.up_to,
                req.num,
                req.size,
                req.fee_rate,
                false,
                false,
            ) {
                Ok(val) => resp.send(Ok(val)).is_ok(),
                Err(err) => {
                    log::error!("create utxo begin: wid={id} error={:?}", err);
                    resp.send(Err(err)).is_ok()
                }
            }
        }
        WalletCmd::CreateUtxoEnd { psbt, resp } => {
            let mut w = wallet_state.lock().unwrap();
            let wo = w.wallet_online;
            match w.wallet.create_utxos_end(wo, psbt) {
                Ok(val) => resp.send(Ok(val as usize)).is_ok(),
                Err(err) => {
                    log::error!("create utxo end: wid={id} error={:?}", err);
                    resp.send(Err(err)).is_ok()
                }
            }
        }
        WalletCmd::SendBegin { req, resp } => {
            let recipient_map: std::collections::HashMap<String, Vec<wallet::Recipient>> = req
                .recipient_map
                .into_iter()
                .map(|(asset_id, recipients)| {
                    (asset_id, recipients.into_iter().map(Into::into).collect())
                })
                .collect();
            let mut w = wallet_state.lock().unwrap();
            let wo = w.wallet_online;
            match w.wallet.send_begin(
                wo,
                recipient_map,
                req.donation,
                req.fee_rate,
                req.min_confirmations,
                None,
                false,
                None,
            ) {
                Ok(val) => resp.send(Ok(val.psbt)).is_ok(),
                Err(err) => {
                    log::error!("send begin: wid={id} error={:?}", err);
                    resp.send(Err(err)).is_ok()
                }
            }
        }
        WalletCmd::SendEnd { psbt, resp } => {
            let mut w = wallet_state.lock().unwrap();
            let wo = w.wallet_online;
            match w.wallet.send_end(wo, psbt) {
                Ok(val) => resp.send(Ok(val)).is_ok(),
                Err(err) => {
                    log::error!("send end: wid={id} error={:?}", err);
                    resp.send(Err(err)).is_ok()
                }
            }
        }
        WalletCmd::SendBtcBegin { req, resp } => {
            let mut w = wallet_state.lock().unwrap();
            let wo = w.wallet_online;
            match w.wallet.send_btc_begin(
                wo,
                req.address,
                req.amount,
                req.fee_rate,
                false,
                false,
                None,
            ) {
                Ok(val) => resp.send(Ok(val)).is_ok(),
                Err(err) => {
                    log::error!("send btc begin: wid={id} error={:?}", err);
                    resp.send(Err(err)).is_ok()
                }
            }
        }
        WalletCmd::SendBtcEnd { psbt, resp } => {
            let mut w = wallet_state.lock().unwrap();
            let wo = w.wallet_online;
            match w.wallet.send_btc_end(wo, psbt) {
                Ok(val) => resp.send(Ok(val)).is_ok(),
                Err(err) => {
                    log::error!("send btc end: wid={id} error={:?}", err);
                    resp.send(Err(err)).is_ok()
                }
            }
        }
        WalletCmd::IssueNiaToken { req, resp } => {
            let w = wallet_state.lock().unwrap();
            match w
                .wallet
                .issue_asset_nia(req.ticker, req.name, req.precision, req.amounts)
            {
                Ok(val) => resp.send(Ok(val)).is_ok(),
                Err(err) => {
                    log::error!("issue token: wid={id} error={:?}", err);
                    resp.send(Err(err)).is_ok()
                }
            }
        }
        WalletCmd::RefreshWallet { resp } => {
            let mut w = wallet_state.lock().unwrap();
            let wo = w.wallet_online;
            match w.wallet.refresh(wo, None, Vec::new(), false) {
                Ok(_) => resp.send(Ok(())).is_ok(),
                Err(err) => {
                    log::error!("refresh wallet: wid={id} error={:?}", err);
                    resp.send(Err(err)).is_ok()
                }
            }
        }
        _ => return, // TODO: response error or warning
    };
}

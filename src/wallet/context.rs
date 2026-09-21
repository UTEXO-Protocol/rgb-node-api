use rgb_lib::wallet;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use super::Config;
use super::models::*;
use super::wallet_thread::*;

#[derive(Clone)]
pub struct WalletCtx {
    id: Option<String>,
    handle: mpsc::Sender<(Option<String>, WalletCmd)>,
}

impl WalletCtx {
    pub fn new(cfg: Config, tasker: &TaskTracker) -> Self {
        let (wt, tx) = WalletThread::new(cfg);

        tasker.spawn_blocking(move || {
            wt.spawn();
        });

        WalletCtx {
            id: None,
            handle: tx,
        }
    }

    pub async fn refresh_task(self, cancel: CancellationToken) {
        loop {
            let _ = self.handle.send((None, WalletCmd::Refresh {})).await;

            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(10)) => {
                    continue;
               }

                _ = cancel.cancelled() => {
                    log::info!("refresh wallet task cancelled");
                    break;
                }
            };
        }
    }

    async fn send_cmd<T>(
        &self,
        cmd: WalletCmd,
        recv: oneshot::Receiver<Result<T, rgb_lib::Error>>,
    ) -> Result<T, rgb_lib::Error> {
        let _ = self.handle.send((self.id.clone(), cmd)).await.is_ok();
        match recv.await {
            Ok(Ok(val)) => Ok(val),
            Ok(Err(e)) => Err(e),
            Err(e) => {
                log::error!("send wallet cmd: error={e:#}");
                Err(err_dead_thread())
            }
        }
    }

    pub fn with_opt_id(&self, id: Option<String>) -> Self {
        let mut c = self.clone();
        c.id = id;
        c
    }

    pub fn with_id(&self, id: String) -> Self {
        let mut c = self.clone();
        c.id = Some(id);
        c
    }

    pub async fn for_ro_wallet(
        &self,
        xpub_colored: String,
        xpub_vanilla: String,
        master_fingerprint: String,
    ) -> Result<Self, rgb_lib::Error> {
        let id = master_fingerprint.clone();
        let (resp, recv) = oneshot::channel();
        let cmd = WalletCmd::AddReadOnlyWallet {
            xpub_colored,
            xpub_vanilla,
            master_fingerprint,
            resp,
        };

        self.send_cmd(cmd, recv).await?;

        let mut c = self.clone();
        c.id = Some(id);
        Ok(c)
    }

    pub async fn wallet_info(&self) -> Result<Info, rgb_lib::Error> {
        let (resp, recv) = oneshot::channel();
        let cmd = WalletCmd::WalletInfo { resp };

        self.send_cmd(cmd, recv).await
    }

    pub async fn backup(&self, password: String) -> Result<BackupPath, rgb_lib::Error> {
        let (resp, recv) = oneshot::channel();
        let cmd = WalletCmd::Backup { password, resp };

        self.send_cmd(cmd, recv).await
    }
    pub async fn address(&self) -> Result<AddressRes, rgb_lib::Error> {
        let (resp, recv) = oneshot::channel();
        let cmd = WalletCmd::Address { resp };

        self.send_cmd(cmd, recv).await
    }

    pub async fn transfers(&self) -> Result<Vec<rgb_lib::wallet::Transfer>, rgb_lib::Error> {
        let (resp, recv) = oneshot::channel();
        let cmd = WalletCmd::Transfers { resp };
        self.send_cmd(cmd, recv).await
    }

    pub async fn transfer_by_recipient_id(
        &self,
        recipient_id: &str,
    ) -> Result<Option<rgb_lib::wallet::Transfer>, rgb_lib::Error> {
        let (resp, recv) = oneshot::channel();
        let cmd = WalletCmd::TransfersByRecipient {
            recipient_id: recipient_id.to_owned(),
            resp,
        };
        self.send_cmd(cmd, recv).await
    }

    pub async fn transfers_by_asset(
        &self,
        asset_id: &str,
    ) -> Result<Vec<rgb_lib::wallet::Transfer>, rgb_lib::Error> {
        let (resp, recv) = oneshot::channel();
        let cmd = WalletCmd::TransfersByAsset {
            asset_id: asset_id.to_owned(),
            resp,
        };
        self.send_cmd(cmd, recv).await
    }

    pub async fn transactions(&self) -> Result<Vec<rgb_lib::wallet::Transaction>, rgb_lib::Error> {
        let (resp, recv) = oneshot::channel();
        let cmd = WalletCmd::Transactions { resp };
        self.send_cmd(cmd, recv).await
    }

    pub async fn list_unspends(&self) -> Result<Vec<rgb_lib::wallet::Unspent>, rgb_lib::Error> {
        let (resp, recv) = oneshot::channel();
        let cmd = WalletCmd::ListUnspends { resp };
        self.send_cmd(cmd, recv).await
    }

    pub async fn assets(&self) -> Result<rgb_lib::wallet::Assets, rgb_lib::Error> {
        let (resp, recv) = oneshot::channel();
        let cmd = WalletCmd::Assets { resp };
        self.send_cmd(cmd, recv).await
    }

    pub async fn asset_info(
        &self,
        asset_id: &str,
    ) -> Result<rgb_lib::wallet::Metadata, rgb_lib::Error> {
        let (resp, recv) = oneshot::channel();
        let cmd = WalletCmd::AssetInfo {
            asset_id: asset_id.to_owned(),
            resp,
        };

        self.send_cmd(cmd, recv).await
    }

    pub async fn balance_btc(&self) -> Result<rgb_lib::wallet::BtcBalance, rgb_lib::Error> {
        let (resp, recv) = oneshot::channel();
        let cmd = WalletCmd::BtcBalance { resp };
        self.send_cmd(cmd, recv).await
    }

    pub async fn balance_token(
        &self,
        asset_id: &str,
    ) -> Result<rgb_lib::wallet::Balance, rgb_lib::Error> {
        let (resp, recv) = oneshot::channel();
        let cmd = WalletCmd::TokenBalance {
            asset_id: asset_id.to_owned(),
            resp,
        };

        self.send_cmd(cmd, recv).await
    }

    pub async fn receive_token(
        &self,
        req: ReceiveReq,
    ) -> Result<rgb_lib::wallet::ReceiveData, rgb_lib::Error> {
        let (resp, recv) = oneshot::channel();
        let cmd = WalletCmd::Receive {
            req: req.to_owned(),
            resp,
        };
        self.send_cmd(cmd, recv).await
    }

    pub async fn issue_nia_token(
        &self,
        req: IssueNiaReq,
    ) -> Result<rgb_lib::wallet::AssetNIA, rgb_lib::Error> {
        let (resp, recv) = oneshot::channel();
        let cmd = WalletCmd::IssueNiaToken {
            req: req.to_owned(),
            resp,
        };
        self.send_cmd(cmd, recv).await
    }
    pub async fn create_utxo_begin(
        &self,
        req: CreateUtxoBeginReq,
    ) -> Result<String, rgb_lib::Error> {
        let (resp, recv) = oneshot::channel();
        let cmd = WalletCmd::CreateUtxoBegin { req, resp };
        self.send_cmd(cmd, recv).await
    }

    pub async fn create_utxo_end(&self, signed_psbt: String) -> Result<usize, rgb_lib::Error> {
        let (resp, recv) = oneshot::channel();
        let cmd = WalletCmd::CreateUtxoEnd {
            psbt: signed_psbt,
            resp,
        };
        self.send_cmd(cmd, recv).await
    }

    pub async fn fail_transfer(
        &self,
        batch_transfer_idx: Option<i32>,
        no_asset_only: bool,
        skip_sync: bool,
    ) -> Result<bool, rgb_lib::Error> {
        let (resp, recv) = oneshot::channel();
        let cmd = WalletCmd::FailTransfer {
            batch_transfer_idx,
            no_asset_only,
            skip_sync,
            resp,
        };
        self.send_cmd(cmd, recv).await
    }

    pub async fn send_btc_begin(&self, req: SendBtcReq) -> Result<String, rgb_lib::Error> {
        let (resp, recv) = oneshot::channel();
        let cmd = WalletCmd::SendBtcBegin { req, resp };
        self.send_cmd(cmd, recv).await
    }

    pub async fn send_btc_end(&self, signed_psbt: String) -> Result<String, rgb_lib::Error> {
        let (resp, recv) = oneshot::channel();
        let cmd = WalletCmd::SendBtcEnd {
            psbt: signed_psbt,
            resp,
        };
        self.send_cmd(cmd, recv).await
    }

    pub async fn send_begin(&self, req: SendBeginReq) -> Result<String, rgb_lib::Error> {
        req.validate_expiration()?;
        let (resp, recv) = oneshot::channel();
        let cmd = WalletCmd::SendBegin { req, resp };
        self.send_cmd(cmd, recv).await
    }

    pub async fn send_end(
        &self,
        signed_psbt: String,
    ) -> Result<wallet::OperationResult, rgb_lib::Error> {
        let (resp, recv) = oneshot::channel();
        let cmd = WalletCmd::SendEnd {
            psbt: signed_psbt,
            resp,
        };
        self.send_cmd(cmd, recv).await
    }

    pub async fn refresh_wallet(&self) -> Result<(), rgb_lib::Error> {
        let (resp, recv) = oneshot::channel();
        let cmd = WalletCmd::RefreshWallet { resp };
        self.send_cmd(cmd, recv).await
    }

    pub async fn drop_wallet(&self) -> Result<(), rgb_lib::Error> {
        let (resp, recv) = oneshot::channel();
        let cmd = WalletCmd::DropWallet { resp };
        self.send_cmd(cmd, recv).await
    }

    pub async fn stop(&self) -> Result<(), rgb_lib::Error> {
        let (resp, recv) = oneshot::channel();
        let cmd = WalletCmd::Stop { resp };
        self.send_cmd(cmd, recv).await
    }
}

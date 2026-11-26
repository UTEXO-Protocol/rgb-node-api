use std::sync::Arc;

use rgb_lib::wallet;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use super::Config;
use super::models::*;
use super::wallet_thread::*;

#[derive(Clone)]
pub struct WalletCtx {
    #[cfg_attr(not(feature = "server-signing"), allow(dead_code))]
    cfg: Arc<Config>,
    id: Option<String>,
    handle: mpsc::Sender<(Option<String>, WalletCmd)>,
}

impl WalletCtx {
    pub fn new(cfg: Config, tasker: &TaskTracker) -> Self {
        let cfg_arc = Arc::new(cfg.clone());
        let (wt, tx) = WalletThread::new(cfg);

        tasker.spawn_blocking(move || {
            wt.spawn();
        });

        WalletCtx {
            cfg: cfg_arc,
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

    pub async fn transfers_filtered(
        &self,
        asset_id: Option<String>,
        batch_transfer_idx: Option<i32>,
    ) -> Result<Vec<rgb_lib::wallet::Transfer>, rgb_lib::Error> {
        let (resp, recv) = oneshot::channel();
        let cmd = WalletCmd::TransfersFiltered {
            asset_id,
            batch_transfer_idx,
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
        kind: ReceiveKind,
        req: ReceiveReq,
    ) -> Result<rgb_lib::wallet::ReceiveData, rgb_lib::Error> {
        let (resp, recv) = oneshot::channel();
        let cmd = WalletCmd::Receive {
            kind,
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
    pub async fn issue_ifa_token(
        &self,
        req: IssueIfaReq,
    ) -> Result<wallet::AssetIFA, rgb_lib::Error> {
        let (resp, recv) = oneshot::channel();
        let cmd = WalletCmd::IssueIfaToken { req, resp };
        self.send_cmd(cmd, recv).await
    }

    pub async fn issue_bfa_token(
        &self,
        req: IssueBfaReq,
    ) -> Result<wallet::AssetBFA, rgb_lib::Error> {
        let (resp, recv) = oneshot::channel();
        self.send_cmd(WalletCmd::IssueBfaToken { req, resp }, recv)
            .await
    }

    pub async fn mint_begin(
        &self,
        req: MintBeginReq,
    ) -> Result<wallet::MintBeginResult, rgb_lib::Error> {
        let (resp, recv) = oneshot::channel();
        self.send_cmd(WalletCmd::MintBegin { req, resp }, recv)
            .await
    }

    pub async fn mint_end(&self, psbt: String) -> Result<wallet::OperationResult, rgb_lib::Error> {
        let (resp, recv) = oneshot::channel();
        self.send_cmd(WalletCmd::MintEnd { psbt, resp }, recv).await
    }

    pub async fn link_begin(
        &self,
        req: LinkBeginReq,
    ) -> Result<wallet::LinkBeginResult, rgb_lib::Error> {
        let (resp, recv) = oneshot::channel();
        self.send_cmd(WalletCmd::LinkBegin { req, resp }, recv)
            .await
    }

    pub async fn link_end(&self, psbt: String) -> Result<wallet::OperationResult, rgb_lib::Error> {
        let (resp, recv) = oneshot::channel();
        self.send_cmd(WalletCmd::LinkEnd { psbt, resp }, recv).await
    }

    pub async fn inflate_begin(&self, req: InflateBeginReq) -> Result<String, rgb_lib::Error> {
        let (resp, recv) = oneshot::channel();
        let cmd = WalletCmd::InflateBegin { req, resp };
        self.send_cmd(cmd, recv).await
    }

    pub async fn inflate_end(
        &self,
        signed_psbt: String,
    ) -> Result<wallet::OperationResult, rgb_lib::Error> {
        let (resp, recv) = oneshot::channel();
        let cmd = WalletCmd::InflateEnd {
            psbt: signed_psbt,
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
        req.expiration()?;
        let (resp, recv) = oneshot::channel();
        let cmd = WalletCmd::SendBegin { req, resp };
        self.send_cmd(cmd, recv).await
    }

    pub async fn burn_begin(&self, req: BurnBeginReq) -> Result<String, rgb_lib::Error> {
        let (resp, recv) = oneshot::channel();
        let cmd = WalletCmd::BurnBegin { req, resp };
        self.send_cmd(cmd, recv).await
    }

    pub async fn burn_end(
        &self,
        signed_psbt: String,
    ) -> Result<wallet::OperationResult, rgb_lib::Error> {
        let (resp, recv) = oneshot::channel();
        let cmd = WalletCmd::BurnEnd {
            psbt: signed_psbt,
            resp,
        };
        self.send_cmd(cmd, recv).await
    }

    pub async fn get_received_mint(
        &self,
        recipient_id: String,
    ) -> Result<Option<wallet::ReceivedMint>, rgb_lib::Error> {
        wallet::RecipientInfo::new(recipient_id.clone())?;
        let (resp, recv) = oneshot::channel();
        let cmd = WalletCmd::GetReceivedMint { recipient_id, resp };
        self.send_cmd(cmd, recv).await
    }

    pub async fn inspect_psbt(
        &self,
        psbt: String,
    ) -> Result<wallet::PsbtInspection, rgb_lib::Error> {
        let (resp, recv) = oneshot::channel();
        let cmd = WalletCmd::InspectPsbt { psbt, resp };
        self.send_cmd(cmd, recv).await
    }

    pub async fn get_consignment(
        &self,
        req: GetConsignmentReq,
    ) -> Result<GetConsignmentRes, rgb_lib::Error> {
        let (resp, recv) = oneshot::channel();
        let cmd = WalletCmd::GetConsignment { req, resp };
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
}

#[cfg(feature = "server-signing")]
impl WalletCtx {
    /// Whether the server may keep keys and sign (`allow_server_signing`).
    pub fn server_signing_enabled(&self) -> bool {
        self.cfg.allow_server_signing
    }

    pub fn signing_token_matches(&self, token: &str) -> bool {
        self.cfg.signing_token_matches(token)
    }

    /// Make a wallet that holds its keys in memory and signs inside the node.
    /// With a mnemonic it restores that wallet, without one it makes new keys.
    pub async fn init_full_wallet(
        &self,
        mnemonic: Option<String>,
    ) -> Result<rgb_lib::keys::Keys, rgb_lib::Error> {
        let (resp, recv) = oneshot::channel();
        let cmd = WalletCmd::AddFullWallet { mnemonic, resp };
        self.send_cmd(cmd, recv).await
    }

    /// `send_begin`, sign and `send_end` in one call.
    pub async fn send_signed(
        &self,
        req: SendBeginReq,
    ) -> Result<wallet::OperationResult, rgb_lib::Error> {
        let (resp, recv) = oneshot::channel();
        self.send_cmd(WalletCmd::SendSigned { req, resp }, recv)
            .await
    }

    /// `burn_begin`, sign and `burn_end` in one call.
    pub async fn burn_signed(
        &self,
        req: BurnBeginReq,
    ) -> Result<wallet::OperationResult, rgb_lib::Error> {
        let (resp, recv) = oneshot::channel();
        self.send_cmd(WalletCmd::BurnSigned { req, resp }, recv)
            .await
    }

    /// `inflate_begin`, sign and `inflate_end` in one call.
    pub async fn inflate_signed(
        &self,
        req: InflateBeginReq,
    ) -> Result<wallet::OperationResult, rgb_lib::Error> {
        let (resp, recv) = oneshot::channel();
        self.send_cmd(WalletCmd::InflateSigned { req, resp }, recv)
            .await
    }

    /// `send_btc_begin`, sign and `send_btc_end` in one call. Returns the TXID.
    pub async fn send_btc_signed(&self, req: SendBtcReq) -> Result<String, rgb_lib::Error> {
        let (resp, recv) = oneshot::channel();
        self.send_cmd(WalletCmd::SendBtcSigned { req, resp }, recv)
            .await
    }

    /// `create_utxos_begin`, sign and `create_utxos_end` in one call.
    pub async fn create_utxos_signed(&self, req: CreateUtxoBeginReq) -> Result<u8, rgb_lib::Error> {
        let (resp, recv) = oneshot::channel();
        self.send_cmd(WalletCmd::CreateUtxosSigned { req, resp }, recv)
            .await
    }
}

impl WalletCtx {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn received_mint_rejects_invalid_ids_before_wallet_lookup() {
        let tasks = TaskTracker::new();
        let ctx = WalletCtx::new(Config::default(), &tasks);
        for recipient_id in [
            "",
            "invalid",
            "../../other-wallet/transfers/mint",
            "/tmp/mint",
        ] {
            assert!(matches!(
                ctx.get_received_mint(recipient_id.into()).await,
                Err(rgb_lib::Error::InvalidRecipientID)
            ));
        }
        ctx.stop().await.unwrap();
        tasks.close();
        tasks.wait().await;
    }
}

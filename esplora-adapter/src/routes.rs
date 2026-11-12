//! Esplora REST routes, implemented on top of Electrum calls.
//!
//! Known limits (Electrum cannot answer these):
//! - `/block/:hash/*` returns 501: Electrum has no lookup by block hash.
//! - A confirmed tx's height is found through the history of its first
//!   output script, so txs paying huge addresses are slow to look up.
//! - Address listings return at most 50 txs and have no pagination.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use actix_web::{HttpResponse, web};
use electrum_client::bitcoin::consensus::{deserialize, encode::serialize_hex};
use electrum_client::bitcoin::hashes::{Hash, sha256};
use electrum_client::bitcoin::{
    Address, Network, OutPoint, Script, ScriptBuf, Transaction, TxOut as TxOutput_, Txid,
};
use electrum_client::{ElectrumApi, GetHistoryRes, Param};
use serde::Serialize;
use tokio::sync::Semaphore;

use crate::electrum::{AdapterError, Backend};
use crate::types::*;

pub struct State {
    pub backend: Backend,
    pub net: Network,
    /// Caps requests served at once.
    pub permits: Arc<Semaphore>,
    pub timeout: Duration,
}

type St = web::Data<State>;
type Res = Result<Reply, AdapterError>;

/// Serialize a typed response body.
fn reply<T: Serialize>(v: &T) -> Res {
    serde_json::to_string(v)
        .map(Reply::Json)
        .map_err(|e| AdapterError::Upstream(e.to_string()))
}

pub enum Reply {
    /// already serialized JSON
    Json(String),
    Text(String),
    Bytes(Vec<u8>),
}

async fn run(st: St, f: impl FnOnce(&State) -> Res + Send + 'static) -> HttpResponse {
    // Too many requests in flight: refuse instead of queueing without limit.
    let Ok(permit) = st.permits.clone().try_acquire_owned() else {
        return HttpResponse::ServiceUnavailable()
            .insert_header(("Retry-After", "1"))
            .body("server busy");
    };
    let limit = st.timeout;
    // The permit moves into the worker, so a timed-out job still counts
    // against the cap until it really finishes.
    let job = web::block(move || {
        let _permit = permit;
        f(&st)
    });
    let out = match tokio::time::timeout(limit, job).await {
        Ok(out) => out,
        Err(_) => return HttpResponse::GatewayTimeout().body("request timed out"),
    };
    match out {
        Ok(Ok(Reply::Json(v))) => HttpResponse::Ok().content_type("application/json").body(v),
        Ok(Ok(Reply::Text(s))) => HttpResponse::Ok().content_type("text/plain").body(s),
        Ok(Ok(Reply::Bytes(b))) => HttpResponse::Ok()
            .content_type("application/octet-stream")
            .body(b),
        Ok(Err(e)) => error_response(e),
        Err(e) => HttpResponse::InternalServerError().body(e.to_string()),
    }
}

fn error_response(e: AdapterError) -> HttpResponse {
    let text =
        |mut r: actix_web::HttpResponseBuilder, m: String| r.content_type("text/plain").body(m);
    match e {
        AdapterError::NotFound(m) => text(HttpResponse::NotFound(), m),
        AdapterError::BadRequest(m) => text(HttpResponse::BadRequest(), m),
        AdapterError::Unsupported(m) => text(HttpResponse::NotImplemented(), m),
        AdapterError::Upstream(m) => {
            log::error!("upstream error: {m}");
            text(HttpResponse::BadGateway(), m)
        }
    }
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route("/health", web::get().to(|| async { "ok" }))
        .route("/blocks/tip/height", web::get().to(tip_height))
        .route("/blocks/tip/hash", web::get().to(tip_hash))
        .route("/block-height/{height}", web::get().to(block_height))
        .route("/blocks", web::get().to(blocks_list))
        .route("/blocks/{start}", web::get().to(blocks_list_from))
        .route("/block/{hash}/header", web::get().to(block_header))
        .route("/block/{hash}/status", web::get().to(block_status))
        .route("/block/{tail:.*}", web::get().to(block_unsupported))
        .route("/tx", web::post().to(broadcast))
        .route("/tx/{txid}", web::get().to(tx_info))
        .route("/tx/{txid}/status", web::get().to(tx_status))
        .route("/tx/{txid}/hex", web::get().to(tx_hex))
        .route("/tx/{txid}/raw", web::get().to(tx_raw))
        .route("/tx/{txid}/merkle-proof", web::get().to(merkle_proof))
        .route("/tx/{txid}/outspend/{vout}", web::get().to(outspend))
        .route("/tx/{txid}/outspends", web::get().to(outspends))
        .route("/address/{addr}/txs", web::get().to(address_txs))
        .route(
            "/address/{addr}/txs/mempool",
            web::get().to(address_txs_mempool),
        )
        .route(
            "/address/{addr}/txs/chain",
            web::get().to(address_txs_chain),
        )
        .route(
            "/address/{addr}/txs/chain/{last}",
            web::get().to(address_txs_chain_from),
        )
        .route("/address/{addr}/utxo", web::get().to(address_utxo))
        .route("/scripthash/{hash}/txs", web::get().to(scripthash_txs))
        .route(
            "/scripthash/{hash}/txs/mempool",
            web::get().to(scripthash_txs_mempool),
        )
        .route(
            "/scripthash/{hash}/txs/chain",
            web::get().to(scripthash_txs_chain),
        )
        .route(
            "/scripthash/{hash}/txs/chain/{last}",
            web::get().to(scripthash_txs_chain_from),
        )
        .route("/scripthash/{hash}/utxo", web::get().to(scripthash_utxo))
        .route("/fee-estimates", web::get().to(fee_estimates));
}

// ---------- helpers ----------

fn bad(m: impl ToString) -> AdapterError {
    AdapterError::BadRequest(m.to_string())
}

fn parse_txid(s: &str) -> Result<Txid, AdapterError> {
    Txid::from_str(s).map_err(|e| bad(format!("invalid txid: {e}")))
}

fn scripthash_of(script: &Script) -> String {
    let mut h = sha256::Hash::hash(script.as_bytes()).to_byte_array();
    h.reverse();
    hex::encode(h)
}

/// Esplora scripthash is sha256(script) as plain hex; Electrum's is byte-reversed.
fn from_esplora_scripthash(h: &str) -> Result<String, AdapterError> {
    let mut b = hex::decode(h).map_err(|_| bad("invalid scripthash"))?;
    if b.len() != 32 {
        return Err(bad("invalid scripthash"));
    }
    b.reverse();
    Ok(hex::encode(b))
}

fn script_of_addr(st: &State, a: &str) -> Result<ScriptBuf, AdapterError> {
    let addr = Address::from_str(a)
        .map_err(|e| bad(format!("invalid address: {e}")))?
        .require_network(st.net)
        .map_err(|e| bad(format!("address on wrong network: {e}")))?;
    Ok(addr.script_pubkey())
}

fn history(st: &State, scripthash: &str) -> Result<Vec<GetHistoryRes>, AdapterError> {
    if scripthash.len() != 64 || hex::decode(scripthash).is_err() {
        return Err(bad("invalid scripthash"));
    }
    let v = st.backend.call(|c| {
        c.raw_call(
            "blockchain.scripthash.get_history",
            vec![Param::String(scripthash.to_string())],
        )
    })?;
    serde_json::from_value(v).map_err(|e| AdapterError::Upstream(e.to_string()))
}

fn get_tx(st: &State, txid: &Txid) -> Result<Transaction, AdapterError> {
    st.backend.tx(txid)
}

fn spk_type(s: &Script) -> &'static str {
    if s.is_p2pkh() {
        "p2pkh"
    } else if s.is_p2sh() {
        "p2sh"
    } else if s.is_p2wpkh() {
        "v0_p2wpkh"
    } else if s.is_p2wsh() {
        "v0_p2wsh"
    } else if s.is_p2tr() {
        "v1_p2tr"
    } else if s.is_op_return() {
        "op_return"
    } else {
        "unknown"
    }
}

fn tx_output(st: &State, o: &TxOutput_) -> TxOutput {
    let s = &o.script_pubkey;
    TxOutput {
        scriptpubkey: hex::encode(s.as_bytes()),
        scriptpubkey_asm: s.to_asm_string(),
        scriptpubkey_type: spk_type(s),
        scriptpubkey_address: Address::from_script(s, st.net).ok().map(|a| a.to_string()),
        value: o.value.to_sat(),
    }
}

/// Esplora `status` object. `height <= 0` means unconfirmed.
fn status_of(st: &State, height: i32) -> Result<Status, AdapterError> {
    if height <= 0 {
        return Ok(Status {
            confirmed: false,
            block_height: None,
            block_hash: None,
            block_time: None,
        });
    }
    let h = st.backend.header(height as usize)?;
    Ok(Status {
        confirmed: true,
        block_height: Some(height as u32),
        block_hash: Some(h.block_hash().to_string()),
        block_time: Some(h.time),
    })
}

/// Find a tx's confirmation height through the history of one of its
/// scripts. OP_RETURN outputs are not indexed by Electrum, so those are
/// skipped; input prevout scripts are the fallback.
fn find_height(st: &State, tx: &Transaction) -> Result<i32, AdapterError> {
    let txid = tx.compute_txid();
    let lookup = |script: &Script| -> Result<Option<i32>, AdapterError> {
        let hist = history(st, &scripthash_of(script))?;
        Ok(hist.iter().find(|h| h.tx_hash == txid).map(|h| h.height))
    };
    if let Some(o) = tx.output.iter().find(|o| !o.script_pubkey.is_op_return())
        && let Some(h) = lookup(&o.script_pubkey)?
    {
        return Ok(h);
    }
    if !tx.is_coinbase() {
        for i in tx.input.iter().take(3) {
            let prev = get_tx(st, &i.previous_output.txid)?;
            if let Some(o) = prev.output.get(i.previous_output.vout as usize)
                && let Some(h) = lookup(&o.script_pubkey)?
            {
                return Ok(h);
            }
        }
    }
    Ok(0)
}

/// Build the full Esplora tx object (with prevouts and fee).
fn build_tx(st: &State, tx: &Transaction, height: Option<i32>) -> Result<Tx, AdapterError> {
    let height = match height {
        Some(h) => h,
        None => find_height(st, tx)?,
    };
    let coinbase = tx.is_coinbase();

    let mut prev: HashMap<Txid, Transaction> = HashMap::new();
    if !coinbase {
        let mut ids: Vec<Txid> = tx.input.iter().map(|i| i.previous_output.txid).collect();
        ids.sort();
        ids.dedup();
        let txs = st.backend.txs_batch(&ids)?;
        prev = ids.into_iter().zip(txs).collect();
    }

    let mut total_in: u64 = 0;
    let vin: Vec<Vin> = tx
        .input
        .iter()
        .map(|i| {
            let prevout = prev
                .get(&i.previous_output.txid)
                .and_then(|t| t.output.get(i.previous_output.vout as usize));
            if let Some(p) = prevout {
                total_in += p.value.to_sat();
            }
            Vin {
                txid: i.previous_output.txid.to_string(),
                vout: i.previous_output.vout,
                prevout: prevout.map(|p| tx_output(st, p)),
                scriptsig: hex::encode(i.script_sig.as_bytes()),
                scriptsig_asm: i.script_sig.to_asm_string(),
                witness: i.witness.iter().map(hex::encode).collect(),
                is_coinbase: coinbase,
                sequence: i.sequence.0,
            }
        })
        .collect();
    let total_out: u64 = tx.output.iter().map(|o| o.value.to_sat()).sum();

    Ok(Tx {
        txid: tx.compute_txid().to_string(),
        version: tx.version.0,
        locktime: tx.lock_time.to_consensus_u32(),
        vin,
        vout: tx.output.iter().map(|o| tx_output(st, o)).collect(),
        size: tx.total_size(),
        weight: tx.weight().to_wu(),
        fee: if coinbase {
            0
        } else {
            total_in.saturating_sub(total_out)
        },
        status: status_of(st, height)?,
    })
}

enum Page {
    /// mempool (up to 50) + first 25 confirmed
    First,
    Mempool,
    /// confirmed, 25 per page, after `last_seen` when given
    Chain(Option<Txid>),
}

fn txs_for_history(st: &State, hist: Vec<GetHistoryRes>, page: Page) -> Res {
    let (mem, conf): (Vec<_>, Vec<_>) = hist.into_iter().partition(|h| h.height <= 0);
    // Electrum lists confirmed txs oldest first; Esplora wants newest first.
    let conf: Vec<_> = conf.into_iter().rev().collect();
    let picked: Vec<GetHistoryRes> = match page {
        Page::Mempool => mem.into_iter().take(50).collect(),
        Page::First => mem
            .into_iter()
            .take(50)
            .chain(conf.into_iter().take(25))
            .collect(),
        Page::Chain(last) => {
            let start = match last {
                Some(l) => conf
                    .iter()
                    .position(|h| h.tx_hash == l)
                    .map(|i| i + 1)
                    .unwrap_or(conf.len()),
                None => 0,
            };
            conf.into_iter().skip(start).take(25).collect()
        }
    };
    let out: Result<Vec<Tx>, AdapterError> = picked
        .iter()
        .map(|h| build_tx(st, &get_tx(st, &h.tx_hash)?, Some(h.height)))
        .collect();
    reply(&out?)
}

fn utxos(st: &State, scripthash: &str) -> Res {
    if scripthash.len() != 64 || hex::decode(scripthash).is_err() {
        return Err(bad("invalid scripthash"));
    }
    let v = st.backend.call(|c| {
        c.raw_call(
            "blockchain.scripthash.listunspent",
            vec![Param::String(scripthash.to_string())],
        )
    })?;
    let list: Vec<electrum_client::ListUnspentRes> =
        serde_json::from_value(v).map_err(|e| AdapterError::Upstream(e.to_string()))?;
    let out: Result<Vec<Utxo>, AdapterError> = list
        .iter()
        .map(|u| {
            Ok(Utxo {
                txid: u.tx_hash.to_string(),
                vout: u.tx_pos,
                value: u.value,
                status: status_of(st, u.height as i32)?,
            })
        })
        .collect();
    reply(&out?)
}

// ---------- handlers ----------

async fn tip_height(st: St) -> HttpResponse {
    run(st, |s| Ok(Reply::Text(s.backend.tip()?.0.to_string()))).await
}

async fn tip_hash(st: St) -> HttpResponse {
    run(st, |s| Ok(Reply::Text(s.backend.tip()?.1))).await
}

async fn block_height(st: St, p: web::Path<usize>) -> HttpResponse {
    let h = p.into_inner();
    run(st, move |s| {
        let hdr = s
            .backend
            .header(h)
            .map_err(|_| AdapterError::NotFound("block not found".into()))?;
        Ok(Reply::Text(hdr.block_hash().to_string()))
    })
    .await
}

async fn block_unsupported() -> HttpResponse {
    error_response(AdapterError::Unsupported(
        "block lookup by hash is not available through Electrum".into(),
    ))
}

async fn broadcast(st: St, body: String) -> HttpResponse {
    run(st, move |s| {
        let raw = hex::decode(body.trim()).map_err(|e| bad(format!("invalid hex: {e}")))?;
        let tx: Transaction = deserialize(&raw).map_err(|e| bad(format!("invalid tx: {e}")))?;
        let id = s.backend.call(|c| c.transaction_broadcast(&tx))?;
        Ok(Reply::Text(id.to_string()))
    })
    .await
}

async fn tx_info(st: St, p: web::Path<String>) -> HttpResponse {
    run(st, move |s| {
        let tx = get_tx(s, &parse_txid(&p)?)?;
        reply(&build_tx(s, &tx, None)?)
    })
    .await
}

async fn tx_status(st: St, p: web::Path<String>) -> HttpResponse {
    run(st, move |s| {
        let tx = get_tx(s, &parse_txid(&p)?)?;
        let h = find_height(s, &tx)?;
        reply(&status_of(s, h)?)
    })
    .await
}

async fn tx_hex(st: St, p: web::Path<String>) -> HttpResponse {
    run(st, move |s| {
        Ok(Reply::Text(serialize_hex(&get_tx(s, &parse_txid(&p)?)?)))
    })
    .await
}

async fn tx_raw(st: St, p: web::Path<String>) -> HttpResponse {
    run(st, move |s| {
        let id = parse_txid(&p)?;
        Ok(Reply::Bytes(
            s.backend.call(|c| c.transaction_get_raw(&id))?,
        ))
    })
    .await
}

async fn merkle_proof(st: St, p: web::Path<String>) -> HttpResponse {
    run(st, move |s| {
        let id = parse_txid(&p)?;
        let tx = get_tx(s, &id)?;
        let h = find_height(s, &tx)?;
        if h <= 0 {
            return Err(AdapterError::NotFound("tx is not confirmed".into()));
        }
        let m = s
            .backend
            .call(|c| c.transaction_get_merkle(&id, h as usize))?;
        reply(&MerkleProof {
            block_height: m.block_height,
            pos: m.pos,
            merkle: m.merkle.iter().map(hex::encode).collect(),
        })
    })
    .await
}

async fn outspend(st: St, p: web::Path<(String, u32)>) -> HttpResponse {
    let (txid_s, vout) = p.into_inner();
    run(st, move |s| {
        let txid = parse_txid(&txid_s)?;
        let tx = get_tx(s, &txid)?;
        let out = tx
            .output
            .get(vout as usize)
            .ok_or_else(|| AdapterError::NotFound("no such output".into()))?;
        let _ = out;
        reply(&outspend_of(s, &tx, vout)?)
    })
    .await
}

async fn address_txs(st: St, p: web::Path<String>) -> HttpResponse {
    run(st, move |s| {
        let sh = scripthash_of(&script_of_addr(s, &p)?);
        txs_for_history(s, history(s, &sh)?, Page::First)
    })
    .await
}

async fn address_utxo(st: St, p: web::Path<String>) -> HttpResponse {
    run(st, move |s| {
        utxos(s, &scripthash_of(&script_of_addr(s, &p)?))
    })
    .await
}

async fn scripthash_txs(st: St, p: web::Path<String>) -> HttpResponse {
    run(st, move |s| {
        txs_for_history(s, history(s, &from_esplora_scripthash(&p)?)?, Page::First)
    })
    .await
}

async fn scripthash_utxo(st: St, p: web::Path<String>) -> HttpResponse {
    run(st, move |s| utxos(s, &from_esplora_scripthash(&p)?)).await
}

async fn fee_estimates(st: St) -> HttpResponse {
    run(st, |s| {
        let v = Backend::cached(&s.backend.fees, Duration::from_secs(60), || {
            const TARGETS: [usize; 17] = [
                1, 2, 3, 4, 5, 6, 8, 10, 12, 14, 16, 18, 20, 25, 144, 504, 1008,
            ];
            let mut map = FeeEstimates::new();
            for t in TARGETS {
                // Electrum returns BTC/kB; Esplora wants sat/vB. -1 = unknown.
                let btc_kb = s.backend.call(|c| c.estimate_fee(t, None))?;
                if btc_kb > 0.0 {
                    map.insert(t.to_string(), btc_kb * 100_000.0);
                }
            }
            if map.is_empty() {
                // fresh regtest has no estimates: fall back to the relay fee
                let relay = s.backend.call(|c| c.relay_fee())?;
                let rate = (relay * 100_000.0).max(1.0);
                for t in TARGETS {
                    map.insert(t.to_string(), rate);
                }
            }
            Ok(map)
        })?;
        reply(&v)
    })
    .await
}

/// Esplora outspend object for one output of `tx`.
fn outspend_of(st: &State, tx: &Transaction, vout: u32) -> Result<Outspend, AdapterError> {
    let txid = tx.compute_txid();
    let out = tx
        .output
        .get(vout as usize)
        .ok_or_else(|| AdapterError::NotFound("no such output".into()))?;
    let target = OutPoint { txid, vout };
    let hist = history(st, &scripthash_of(&out.script_pubkey))?;
    for h in hist.iter().filter(|h| h.tx_hash != txid).take(200) {
        let t = get_tx(st, &h.tx_hash)?;
        if let Some(vin) = t.input.iter().position(|i| i.previous_output == target) {
            return Ok(Outspend {
                spent: true,
                txid: Some(h.tx_hash.to_string()),
                vin: Some(vin),
                status: Some(status_of(st, h.height)?),
            });
        }
    }
    Ok(Outspend {
        spent: false,
        txid: None,
        vin: None,
        status: None,
    })
}

async fn outspends(st: St, p: web::Path<String>) -> HttpResponse {
    run(st, move |s| {
        let tx = get_tx(s, &parse_txid(&p)?)?;
        let out: Result<Vec<Outspend>, AdapterError> = (0..tx.output.len() as u32)
            .map(|v| outspend_of(s, &tx, v))
            .collect();
        reply(&out?)
    })
    .await
}

async fn address_txs_mempool(st: St, p: web::Path<String>) -> HttpResponse {
    run(st, move |s| {
        let sh = scripthash_of(&script_of_addr(s, &p)?);
        txs_for_history(s, history(s, &sh)?, Page::Mempool)
    })
    .await
}

async fn address_txs_chain(st: St, p: web::Path<String>) -> HttpResponse {
    run(st, move |s| {
        let sh = scripthash_of(&script_of_addr(s, &p)?);
        txs_for_history(s, history(s, &sh)?, Page::Chain(None))
    })
    .await
}

async fn address_txs_chain_from(st: St, p: web::Path<(String, String)>) -> HttpResponse {
    let (a, last) = p.into_inner();
    run(st, move |s| {
        let sh = scripthash_of(&script_of_addr(s, &a)?);
        txs_for_history(s, history(s, &sh)?, Page::Chain(Some(parse_txid(&last)?)))
    })
    .await
}

async fn scripthash_txs_mempool(st: St, p: web::Path<String>) -> HttpResponse {
    run(st, move |s| {
        txs_for_history(s, history(s, &from_esplora_scripthash(&p)?)?, Page::Mempool)
    })
    .await
}

async fn scripthash_txs_chain(st: St, p: web::Path<String>) -> HttpResponse {
    run(st, move |s| {
        txs_for_history(
            s,
            history(s, &from_esplora_scripthash(&p)?)?,
            Page::Chain(None),
        )
    })
    .await
}

async fn scripthash_txs_chain_from(st: St, p: web::Path<(String, String)>) -> HttpResponse {
    let (h, last) = p.into_inner();
    run(st, move |s| {
        txs_for_history(
            s,
            history(s, &from_esplora_scripthash(&h)?)?,
            Page::Chain(Some(parse_txid(&last)?)),
        )
    })
    .await
}

/// Recent block infos, newest first. `tx_count` is found by probing Electrum.
/// Electrum has no block size or weight, so those are 0; `mediantime` is computed from headers.
fn block_summaries(st: &State, from: usize) -> Res {
    let lo = from.saturating_sub(9);
    let lo_ext = lo.saturating_sub(10);
    let hs = st
        .backend
        .call(|c| c.batch_block_header(lo_ext as u32..=from as u32))?;
    let mut out = Vec::new();
    for height in (lo..=from).rev() {
        let i = height - lo_ext;
        let h = &hs[i];
        st.backend.remember(&h.block_hash().to_string(), height);
        let mut times: Vec<u32> = hs[i.saturating_sub(10)..=i]
            .iter()
            .map(|x| x.time)
            .collect();
        times.sort();
        let hash = h.block_hash().to_string();
        out.push(BlockInfo {
            tx_count: st.backend.tx_count(&hash, height)?,
            id: hash,
            height,
            version: h.version.to_consensus(),
            timestamp: h.time,
            size: 0,
            weight: 0,
            merkle_root: h.merkle_root.to_string(),
            previousblockhash: (height > 0).then(|| h.prev_blockhash.to_string()),
            mediantime: times[times.len() / 2],
            nonce: h.nonce,
            bits: h.bits.to_consensus(),
            difficulty: h.difficulty_float(),
        });
    }
    reply(&out)
}

async fn blocks_list(st: St) -> HttpResponse {
    run(st, |s| block_summaries(s, s.backend.tip()?.0)).await
}

async fn blocks_list_from(st: St, p: web::Path<usize>) -> HttpResponse {
    let start = p.into_inner();
    run(st, move |s| {
        let tip = s.backend.tip()?.0;
        block_summaries(s, start.min(tip))
    })
    .await
}

async fn block_header(st: St, p: web::Path<String>) -> HttpResponse {
    run(st, move |s| {
        let h = s.backend.height_of(&p)?;
        Ok(Reply::Text(serialize_hex(&s.backend.header(h)?)))
    })
    .await
}

async fn block_status(st: St, p: web::Path<String>) -> HttpResponse {
    run(st, move |s| {
        let h = s.backend.height_of(&p)?;
        let (tip, _) = s.backend.tip()?;
        let next_best = if h < tip {
            Some(s.backend.header(h + 1)?.block_hash().to_string())
        } else {
            None
        };
        reply(&BlockStatus {
            in_best_chain: true,
            height: h,
            next_best,
        })
    })
    .await
}

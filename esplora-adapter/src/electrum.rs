use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use electrum_client::bitcoin::{Transaction, Txid};
use electrum_client::{Client, ElectrumApi, Param};

/// Error returned to HTTP handlers.
#[derive(Debug)]
pub enum AdapterError {
    NotFound(String),
    BadRequest(String),
    Unsupported(String),
    Upstream(String),
}

impl From<electrum_client::Error> for AdapterError {
    fn from(e: electrum_client::Error) -> Self {
        use electrum_client::Error as E;
        match e {
            E::Protocol(v) => {
                let msg = v.to_string();
                if msg.contains("No such mempool or blockchain transaction")
                    || msg.contains("not found")
                {
                    AdapterError::NotFound(msg)
                } else {
                    AdapterError::BadRequest(msg)
                }
            }
            other => AdapterError::Upstream(other.to_string()),
        }
    }
}

type Cached<T> = Mutex<Option<(Instant, T)>>;

/// Pool of Electrum connections. Each reconnects once on transport errors.
pub struct Backend {
    url: String,
    clients: Vec<Mutex<Option<Arc<Client>>>>,
    next: AtomicUsize,
    pub tip: Cached<(usize, String)>,
    pub fees: Cached<crate::types::FeeEstimates>,
    /// block hash -> height, filled from every header we see
    pub heights: Mutex<HashMap<String, usize>>,
    /// block hash -> number of txs
    pub tx_counts: Mutex<HashMap<String, u64>>,
    /// txid -> tx. Txs are immutable, so this never goes stale.
    txs: Mutex<HashMap<Txid, Transaction>>,
    tx_cache_size: usize,
}

impl Backend {
    pub fn new(url: String, connections: usize, tx_cache_size: usize) -> Self {
        Self {
            url,
            clients: (0..connections.max(1)).map(|_| Mutex::new(None)).collect(),
            next: AtomicUsize::new(0),
            tip: Mutex::new(None),
            fees: Mutex::new(None),
            heights: Mutex::new(HashMap::new()),
            tx_counts: Mutex::new(HashMap::new()),
            txs: Mutex::new(HashMap::new()),
            tx_cache_size,
        }
    }

    fn slot(&self) -> usize {
        self.next.fetch_add(1, Ordering::Relaxed) % self.clients.len()
    }

    fn get(&self, i: usize) -> Result<Arc<Client>, AdapterError> {
        let mut guard = self.clients[i].lock().unwrap();
        if let Some(c) = guard.as_ref() {
            return Ok(c.clone());
        }
        let c = Arc::new(Client::new(&self.url)?);
        *guard = Some(c.clone());
        Ok(c)
    }

    fn reset(&self, i: usize) {
        *self.clients[i].lock().unwrap() = None;
    }

    /// Tx by id, from the cache when possible.
    pub fn tx(&self, txid: &Txid) -> Result<Transaction, AdapterError> {
        if let Some(t) = self.txs.lock().unwrap().get(txid) {
            return Ok(t.clone());
        }
        let t = self.call(|c| c.transaction_get(txid))?;
        self.cache_tx(*txid, &t);
        Ok(t)
    }

    /// Many txs at once; only the cache misses go to Electrum, in one batch.
    pub fn txs_batch(&self, ids: &[Txid]) -> Result<Vec<Transaction>, AdapterError> {
        let mut found: HashMap<Txid, Transaction> = HashMap::new();
        {
            let cache = self.txs.lock().unwrap();
            for id in ids {
                if let Some(t) = cache.get(id) {
                    found.insert(*id, t.clone());
                }
            }
        }
        let missing: Vec<Txid> = ids
            .iter()
            .filter(|i| !found.contains_key(*i))
            .copied()
            .collect();
        if !missing.is_empty() {
            let got = self.call(|c| c.batch_transaction_get(missing.iter()))?;
            for (id, t) in missing.iter().zip(got) {
                self.cache_tx(*id, &t);
                found.insert(*id, t);
            }
        }
        Ok(ids.iter().map(|i| found[i].clone()).collect())
    }

    fn cache_tx(&self, id: Txid, t: &Transaction) {
        let mut m = self.txs.lock().unwrap();
        if m.len() >= self.tx_cache_size {
            m.clear(); // simple bound; the cache refills on demand
        }
        m.insert(id, t.clone());
    }

    /// Number of txs in a block, found by probing `id_from_pos` (cached).
    pub fn tx_count(&self, hash: &str, height: usize) -> Result<u64, AdapterError> {
        if let Some(n) = self.tx_counts.lock().unwrap().get(hash) {
            return Ok(*n);
        }
        let exists = |pos: usize| -> Result<bool, AdapterError> {
            // electrs needs the explicit `merkle = false` third parameter
            let params = || vec![Param::Usize(height), Param::Usize(pos), Param::Bool(false)];
            match self.call(|c| c.raw_call("blockchain.transaction.id_from_pos", params())) {
                Ok(_) => Ok(true),
                Err(AdapterError::BadRequest(_)) | Err(AdapterError::NotFound(_)) => Ok(false),
                Err(e) => Err(e),
            }
        };
        // pos 0 (coinbase) always exists: grow, then bisect for the first miss
        let (mut lo, mut hi) = (0usize, 1usize);
        while exists(hi)? {
            lo = hi;
            hi *= 2;
        }
        while hi - lo > 1 {
            let mid = (lo + hi) / 2;
            if exists(mid)? {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        let n = hi as u64;
        let mut m = self.tx_counts.lock().unwrap();
        if m.len() > 10_000 {
            m.clear();
        }
        m.insert(hash.to_string(), n);
        Ok(n)
    }

    /// Run a call. On a transport error, reconnect and try once more.
    pub fn call<T>(
        &self,
        f: impl Fn(&Client) -> Result<T, electrum_client::Error>,
    ) -> Result<T, AdapterError> {
        let i = self.slot();
        let c = self.get(i)?;
        match f(&c) {
            Err(electrum_client::Error::Protocol(v)) => {
                Err(electrum_client::Error::Protocol(v).into())
            }
            Err(e) => {
                log::warn!("electrum error, reconnecting: {e}");
                self.reset(i);
                let c = self.get(i)?;
                f(&c).map_err(Into::into)
            }
            Ok(v) => Ok(v),
        }
    }

    pub fn cached<T: Clone>(
        slot: &Cached<T>,
        ttl: Duration,
        load: impl FnOnce() -> Result<T, AdapterError>,
    ) -> Result<T, AdapterError> {
        if let Some((at, v)) = slot.lock().unwrap().as_ref()
            && at.elapsed() < ttl
        {
            return Ok(v.clone());
        }
        let v = load()?;
        *slot.lock().unwrap() = Some((Instant::now(), v.clone()));
        Ok(v)
    }

    /// (height, block hash hex) of the chain tip. Cached for 2 seconds.
    pub fn tip(&self) -> Result<(usize, String), AdapterError> {
        Self::cached(&self.tip, Duration::from_secs(2), || {
            let n = self.call(|c| c.block_headers_subscribe())?;
            let hash = n.header.block_hash().to_string();
            self.remember(&hash, n.height);
            Ok((n.height, hash))
        })
    }

    pub fn remember(&self, hash: &str, height: usize) {
        let mut m = self.heights.lock().unwrap();
        if m.len() > 100_000 {
            m.clear();
        }
        m.insert(hash.to_string(), height);
    }

    /// Header at `height`, remembering its hash.
    pub fn header(
        &self,
        height: usize,
    ) -> Result<electrum_client::bitcoin::block::Header, AdapterError> {
        let h = self.call(|c| c.block_header(height))?;
        self.remember(&h.block_hash().to_string(), height);
        Ok(h)
    }

    /// Height of a block hash. Uses the cache, then scans back from the tip.
    pub fn height_of(&self, hash: &str) -> Result<usize, AdapterError> {
        if let Some(h) = self.heights.lock().unwrap().get(hash) {
            return Ok(*h);
        }
        let (tip, _) = self.tip()?;
        let mut top = tip as i64;
        let floor = (tip as i64 - 5000).max(0);
        while top >= floor {
            let start = (top - 99).max(floor);
            let hs = self.call(|c| c.batch_block_header(start as u32..=top as u32))?;
            for (i, h) in hs.iter().enumerate() {
                self.remember(&h.block_hash().to_string(), start as usize + i);
            }
            if let Some(h) = self.heights.lock().unwrap().get(hash) {
                return Ok(*h);
            }
            top = start - 1;
        }
        Err(AdapterError::NotFound("block not found".into()))
    }
}

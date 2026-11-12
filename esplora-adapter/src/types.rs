//! Esplora JSON response shapes. Field names and types follow the Esplora
//! REST API, which `esplora-client` (what rgb-lib uses) parses.

use std::collections::BTreeMap;

use serde::Serialize;

fn is_none<T>(o: &Option<T>) -> bool {
    o.is_none()
}

#[derive(Debug, Serialize)]
pub struct Status {
    pub confirmed: bool,
    #[serde(skip_serializing_if = "is_none")]
    pub block_height: Option<u32>,
    #[serde(skip_serializing_if = "is_none")]
    pub block_hash: Option<String>,
    #[serde(skip_serializing_if = "is_none")]
    pub block_time: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct TxOutput {
    pub scriptpubkey: String,
    pub scriptpubkey_asm: String,
    pub scriptpubkey_type: &'static str,
    #[serde(skip_serializing_if = "is_none")]
    pub scriptpubkey_address: Option<String>,
    pub value: u64,
}

#[derive(Debug, Serialize)]
pub struct Vin {
    pub txid: String,
    pub vout: u32,
    pub prevout: Option<TxOutput>,
    pub scriptsig: String,
    pub scriptsig_asm: String,
    pub witness: Vec<String>,
    pub is_coinbase: bool,
    pub sequence: u32,
}

#[derive(Debug, Serialize)]
pub struct Tx {
    pub txid: String,
    pub version: i32,
    pub locktime: u32,
    pub vin: Vec<Vin>,
    pub vout: Vec<TxOutput>,
    pub size: usize,
    pub weight: u64,
    pub fee: u64,
    pub status: Status,
}

#[derive(Debug, Serialize)]
pub struct Utxo {
    pub txid: String,
    pub vout: usize,
    pub value: u64,
    pub status: Status,
}

#[derive(Debug, Serialize)]
pub struct MerkleProof {
    pub block_height: usize,
    pub pos: usize,
    pub merkle: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct Outspend {
    pub spent: bool,
    #[serde(skip_serializing_if = "is_none")]
    pub txid: Option<String>,
    #[serde(skip_serializing_if = "is_none")]
    pub vin: Option<usize>,
    #[serde(skip_serializing_if = "is_none")]
    pub status: Option<Status>,
}

#[derive(Debug, Serialize)]
pub struct BlockInfo {
    pub id: String,
    pub height: usize,
    pub version: i32,
    pub timestamp: u32,
    pub tx_count: u64,
    pub size: u64,
    pub weight: u64,
    pub merkle_root: String,
    #[serde(skip_serializing_if = "is_none")]
    pub previousblockhash: Option<String>,
    pub mediantime: u32,
    pub nonce: u32,
    pub bits: u32,
    pub difficulty: f64,
}

#[derive(Debug, Serialize)]
pub struct BlockStatus {
    pub in_best_chain: bool,
    pub height: usize,
    pub next_best: Option<String>,
}

/// target (blocks) -> fee rate (sat/vB)
pub type FeeEstimates = BTreeMap<String, f64>;

#[cfg(test)]
mod tests {
    //! Our output must parse with `esplora-client`, the client rgb-lib uses.
    use super::*;

    const H: &str = "00000000000000000008a0b7a3ae17a1f0cb5a1ac4c4e5f1b5d53a4e32d0a8b1";
    const T: &str = "bc7956af94a1370d6c70d8437444a8751d04be08c16d5d9b2f6ae7947f3fd8c9";

    fn confirmed() -> Status {
        Status {
            confirmed: true,
            block_height: Some(102),
            block_hash: Some(H.into()),
            block_time: Some(1_794_481_206),
        }
    }

    fn out() -> TxOutput {
        TxOutput {
            scriptpubkey: "00142e0b78c7fb245e31bcd258c1ffaff7c790a80594".into(),
            scriptpubkey_asm: "OP_0 OP_PUSHBYTES_20 2e0b78c7fb245e31bcd258c1ffaff7c790a80594"
                .into(),
            scriptpubkey_type: "v0_p2wpkh",
            scriptpubkey_address: Some("bcrt1q9c9h33lmy30rr0xjtrqllta8c7g2spv5uhd7n3".into()),
            value: 100_000_000,
        }
    }

    fn parse<T: serde::de::DeserializeOwned>(v: &impl Serialize) -> T {
        serde_json::from_str(&serde_json::to_string(v).unwrap()).unwrap()
    }

    #[test]
    fn tx_parses() {
        let tx = Tx {
            txid: T.into(),
            version: 2,
            locktime: 50,
            vin: vec![Vin {
                txid: T.into(),
                vout: 0,
                prevout: Some(out()),
                scriptsig: String::new(),
                scriptsig_asm: String::new(),
                witness: vec!["aabb".into()],
                is_coinbase: false,
                sequence: 4_294_967_293,
            }],
            vout: vec![out()],
            size: 222,
            weight: 561,
            fee: 1410,
            status: confirmed(),
        };
        let t: esplora_client::Tx = parse(&tx);
        assert_eq!(t.fee, 1410);
        assert_eq!(t.vin[0].prevout.as_ref().unwrap().value, 100_000_000);
        assert_eq!(t.status.block_height, Some(102));
    }

    #[test]
    fn unconfirmed_status_and_missing_prevout_parse() {
        let st = Status {
            confirmed: false,
            block_height: None,
            block_hash: None,
            block_time: None,
        };
        let s: esplora_client::TxStatus = parse(&st);
        assert!(!s.confirmed);
        let v = Vin {
            txid: T.into(),
            vout: 1,
            prevout: None,
            scriptsig: String::new(),
            scriptsig_asm: String::new(),
            witness: vec![],
            is_coinbase: true,
            sequence: 0,
        };
        let _: esplora_client::api::Vin = parse(&v);
    }

    #[test]
    fn utxo_merkle_outspend_blockstatus_parse() {
        let _: esplora_client::Utxo = parse(&Utxo {
            txid: T.into(),
            vout: 1,
            value: 5,
            status: confirmed(),
        });
        let m: esplora_client::MerkleProof = parse(&MerkleProof {
            block_height: 102,
            pos: 1,
            merkle: vec![T.into()],
        });
        assert_eq!(m.pos, 1);
        let o: esplora_client::OutputStatus = parse(&Outspend {
            spent: true,
            txid: Some(T.into()),
            vin: Some(0),
            status: Some(confirmed()),
        });
        assert!(o.spent);
        let unspent: esplora_client::OutputStatus = parse(&Outspend {
            spent: false,
            txid: None,
            vin: None,
            status: None,
        });
        assert!(!unspent.spent);
        let b: esplora_client::BlockStatus = parse(&BlockStatus {
            in_best_chain: true,
            height: 5,
            next_best: None,
        });
        assert_eq!(b.height, Some(5));
    }

    #[test]
    fn block_info_and_fees_parse() {
        let b: esplora_client::BlockInfo = parse(&BlockInfo {
            id: H.into(),
            height: 7,
            version: 536_870_912,
            timestamp: 1_794_481_206,
            tx_count: 2,
            size: 0,
            weight: 0,
            merkle_root: T.into(),
            previousblockhash: Some(H.into()),
            mediantime: 1_794_481_000,
            nonce: 1,
            bits: 545_259_519,
            difficulty: 4.65e-10,
        });
        assert_eq!(b.tx_count, 2);
        let mut f = FeeEstimates::new();
        f.insert("1".into(), 2.5);
        let parsed: std::collections::HashMap<String, f64> = parse(&f);
        assert_eq!(parsed["1"], 2.5);
    }
}

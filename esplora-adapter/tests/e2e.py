#!/usr/bin/env python3
"""End-to-end check: rgb-node-api wallet flows through esplora-adapter.

Run via \ (starts the services). Needs the regtest stack up (\Waiting for bitcoind RPC...
Pre-mining 101 blocks to bcrt1q3wzzyqcha5s6qfsagdmydyevyvr628zqjqhc0a...
Miner balance: 50.00000000 BTC
Local stack ready. Run: just run).
Env: E2E_DIR = scratch dir holding wallet keys and signer state.
"""
import json, subprocess, time, urllib.request, urllib.error, os, sys
S = os.environ["E2E_DIR"]
R = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".."))
def sh(cmd, inp=None, env=None):
    p=subprocess.run(cmd,shell=True,capture_output=True,text=True,cwd=R,input=inp,env={**os.environ,**(env or {})})
    if p.returncode: raise SystemExit(f"CMD FAIL {cmd}\n{p.stderr}")
    return p.stdout.strip()
btc=lambda a: sh(f"docker compose exec -T bitcoind bitcoin-cli -regtest -rpcuser=dev -rpcpassword=dev -rpcwallet=miner {a}")
def mine(n=1):
    addr=btc("getnewaddress"); btc(f"generatetoaddress {n} {addr}"); time.sleep(4)
keys={w:json.loads(sh("./target/debug/wallet-cli new -f json")) for w in "AB"}
def api(w,path,body=None):
    k=keys[w]
    r=urllib.request.Request("http://127.0.0.1:34000/wallet/"+path,data=json.dumps(body or {}).encode(),method="POST",headers={"content-type":"application/json","xpub-van":k["account_xpub_vanilla"],"xpub-col":k["account_xpub_colored"],"master-fingerprint":k["master_fingerprint"]})
    try: return json.load(urllib.request.urlopen(r,timeout=300))
    except urllib.error.HTTPError as e: raise SystemExit(f"HTTP {e.code} {path}: {e.read().decode()}")
def sign(w,psbt): return sh("./target/debug/wallet-cli sign",inp=psbt,env={"RGB_MNEMONIC":keys[w]["mnemonic"],"RGB_CLI_DATA_DIR":f"{S}/sign{w}"})
ok=[]
def step(name,cond,detail=""):
    print(("PASS " if cond else "FAIL ")+name, detail if not cond else ""); ok.append(cond)
    if not cond: print("ABORT"); sys.exit(1)

# 1 register + fund
addr={}
for w in "AB":
    r=api(w,"register"); addr[w]=r["address"]
step("register A,B (go_online via esplora)",all(addr.values()))
for w in "AB": btc(f"sendtoaddress {addr[w]} 1")
mine(1)
for w in "AB": api(w,"refresh")
bal={w:api(w,"btcbalance") for w in "AB"}
step("btc balance synced (address utxos+tx lookups)",all(b["vanilla"]["settled"]>=100000000 for b in bal.values()),bal)

# 2 create utxos
for w in "AB":
    p=api(w,"createutxosbegin",{"num":5,"size":1000,"fee_rate":2})["psbt"]
    n=api(w,"createutxosend",{"signed_psbt":sign(w,p)})
    step(f"createutxos {w} (broadcast)",n["created"]>0,n)
mine(1)
for w in "AB": api(w,"refresh")

# 3 issue
a=api("A","issueassetnia",{"amounts":[1000],"ticker":"TST","name":"Test","precision":0})
aid=a["asset_id"]; step("issue NIA",bool(aid),a)


def post(path,body,w=None):
    return api(w,path,body) if w else json.loads(urllib.request.urlopen(urllib.request.Request("http://127.0.0.1:34000/wallet/"+path,data=json.dumps(body).encode(),method="POST",headers={"content-type":"application/json"})).read())
def recipient(inv,amount):
    d=post("decodeinvoice",{"invoice":inv["invoice"]})
    return {"recipient_id":inv["recipient_id"],"assignment":{"Fungible":amount},"transport_endpoints":d["transport_endpoints"],"public_key":d.get("pubkey") or d.get("public_key") or d.get("pk"),"encryption":d.get("encryption","Required"),"expiration_timestamp":d.get("expiration_timestamp") or inv.get("expiration_timestamp")},d
def transfer(frm,to,aid,amount,kind):
    inv=api(to,kind,{"assignment":{"Fungible":amount}} if kind=="witnessreceive" else {"assignment":{"Fungible":amount}})
    rcp,d=recipient(inv,amount)
    if kind=="witnessreceive": rcp["witness_data"]={"amount_sat":1000,"blinding":None}
    p=api(frm,"sendbegin",{"recipient_map":{aid:[rcp]},"fee_rate":2,"donation":False})["psbt"]
    r=api(frm,"sendend",{"signed_psbt":sign(frm,p)})
    return r
def settle(aid):
    for _ in range(3):
        mine(1)
        for w in "BA": api(w,"refresh")
        for w in "AB": pass
bal=lambda w,aid: api(w,"assetbalance",{"asset_id":aid})

# 4 blind A->B 100
r=transfer("A","B",aid,100,"blindreceive"); step("send A->B (blind) broadcast",bool(r["txid"]),r)
settle(aid)
bb,ba=bal("B",aid),bal("A",aid)
step("B received 100 (consignment+witness lookups)",bb["settled"]==100,(bb,ba))
step("A has 900",ba["settled"]==900,(bb,ba))

# 5 witness B->A 40
r=transfer("B","A",aid,40,"witnessreceive"); step("send B->A (witness) broadcast",bool(r["txid"]),r)
settle(aid)
bb,ba=bal("B",aid),bal("A",aid)
step("B 60 / A 940 after witness send",bb["settled"]==60 and ba["settled"]==940,(bb,ba))

# 6 transfer statuses
for w in "AB":
    ts=api(w,"listtransfers",{"asset_id":aid})
    step(f"all transfers settled {w}",all(t["status"]=="Settled" for t in ts),[t["status"] for t in ts])

# 7 send btc
r=api("A","sendbtcbegin",{"address":addr["B"],"amount":50000,"fee_rate":2})["psbt"]
x=api("A","sendbtcend",{"signed_psbt":sign("A",r)}); step("send BTC A->B",bool(x),x); mine(1)
tx=api("B","listtransactions"); step("B listtransactions sees btc",len(tx)>=2,len(tx))
print("ALL",sum(ok),"/",len(ok),"PASSED")

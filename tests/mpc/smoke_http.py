"""Start the built API twice and verify real HTTP registration/invoice recovery.

Uses only unfunded public fixtures. Build the API and mpc_registration_fixture
example first. No Docker services are needed for this offline receive check.
"""
import json
import os
from pathlib import Path
import secrets
import socket
import subprocess
import tempfile
import time
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen
import uuid

ROOT = Path(__file__).resolve().parents[2]


def main(provider=None, network="regtest"):
    fixture = json.loads(subprocess.check_output([
        str(ROOT / "target/debug/examples/mpc_registration_fixture"), "vault", network
    ]))
    if provider is not None:
        fixture["provider"] = provider
        fixture["addresses"] = [address for address in fixture["addresses"] if address["role"] == "rgb"]
    with tempfile.TemporaryDirectory(prefix="utexo-mpc-http-") as folder:
        directory = Path(folder)
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            port = listener.getsockname()[1]
        config = directory / "config.toml"
        config.write_text(
            (ROOT / "tests/mpc/local.toml").read_text()
            .replace("port = 31011", f"port = {port}")
            .replace('network = "regtest"', f'network = "{network}"')
            .replace('data_dir = "./.mpc-poc-data"', f'data_dir = "{directory / "state"}"')
            .replace('indexer_address = "127.0.0.1:51011"', 'indexer_address = "invalid-indexer-url"')
        )
        token = secrets.token_hex(32)
        environment = dict(os.environ, RGB_MPC_SERVICE_TOKEN=token, RUST_LOG="error")
        base = f"http://127.0.0.1:{port}"

        def request(path, body=None, owner="alice", authenticated=True):
            headers = {"X-Tenant-Id": "poc", "X-User-Id": owner}
            if authenticated:
                headers["Authorization"] = f"Bearer {token}"
            if body is not None:
                headers["Content-Type"] = "application/json"
            query = Request(base + path, headers=headers,
                            data=None if body is None else json.dumps(body).encode())
            try:
                response = urlopen(query, timeout=10)
            except HTTPError as error:
                response = error
            with response:
                payload = response.read()
                return response.status, json.loads(payload) if payload else None

        invoice_request = {"request_id": str(uuid.uuid4()), "asset_id": None,
                           "amount": "25", "expiration_timestamp": int(time.time()) + 3600}
        path = f'/internal/mpc/wallets/{fixture["wallet_id"]}'
        invoice = None
        for restart in [False, True]:
            with (directory / "server.log").open("ab") as log:
                process = subprocess.Popen([str(ROOT / "target/debug/rgb-node-api"), "-c", str(config)],
                                           env=environment, stdout=log, stderr=log)
                try:
                    deadline = time.monotonic() + 15
                    while True:
                        assert process.poll() is None, "API process exited during startup"
                        try:
                            if request("/healthcheck")[0] == 200:
                                break
                        except (URLError, TimeoutError):
                            pass
                        assert time.monotonic() < deadline, "API startup timed out"
                        time.sleep(0.1)
                    assert request(path, authenticated=False)[0] == 401
                    if not restart:
                        status, wallet = request("/internal/mpc/wallets", fixture)
                        assert status == 200, wallet
                        assert wallet["witness_receive"] and not wallet["signing"]
                        assert wallet["external_send"] == ("p2wpkh_blind" if provider else None)
                        invalid = dict(fixture, provider="../invalid")
                        # The server maps JSON deserialization failures to 422.
                        assert request("/internal/mpc/wallets", invalid)[0] == 422
                        changed = dict(fixture, provider="replacement_provider")
                        assert request("/internal/mpc/wallets", changed)[0] == 409
                        assert request(path, owner="bob")[0] == 404
                        assert request(path + "/assets") == (200, {"assets": [], "btc_balance": None})
                    status, current_wallet = request(path)
                    assert status == 200 and current_wallet["provider"] == fixture["provider"]
                    assert current_wallet["bitcoin_network"] == fixture["bitcoin_network"]
                    assert current_wallet["genesis_hash"] == fixture["genesis_hash"]
                    status, current = request(path + "/witness-invoices", invoice_request)
                    assert status == 200, current
                    if invoice:
                        assert current == invoice, "Restart issued a different invoice"
                    invoice = current
                    assert request(path + "/witness-invoices", invoice_request) == (200, invoice)
                    history = request(path + "/transfers")[1]["transfers"]
                    assert len(history) == 1
                    assert "consignment_path" not in history[0] and "psbt_path" not in history[0]
                finally:
                    process.terminate()
                    try:
                        process.wait(timeout=10)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait()
                        raise AssertionError("API did not shut down gracefully")
        print(f"PASS ({network}): HTTP provider registration, capabilities, authorization, invoice idempotency, ownership and restart recovery.")


if __name__ == "__main__":
    main()
    for selected_network in ["mainnet", "testnet3", "testnet4", "signet", "regtest"]:
        main("adapter_" + uuid.uuid4().hex, selected_network)

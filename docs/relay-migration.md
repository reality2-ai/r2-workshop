# Relay migration — point r2-workshop at an r2-hive relay

The dashboard's off-network viewer path (`--relay-url`,
`dashboard/src/relay.rs`) speaks the legacy relay WebSocket protocol on
path `/r2`. That protocol is served identically by the original
`r2-relay` **and** by its successor **r2-hive** (whose `compat/` layer
was verified byte-for-byte compatible with this client: signed HELLO,
`{type:catchup}`, and the `0xFF` JOIN_REQUEST/RESPONSE fan-out). So
migrating is **configuration, not code** — point `--relay-url` at an
r2-hive instance.

r2-hive's crypto trust-group join is stubbed; r2-workshop's own
`r2-trust` + `dashboard/src/access.rs` remain the authority. The relay
only forwards encrypted bytes by `tg_hash` bucket — it never sees
plaintext or issues certs.

## Topology

```
 phone / viewer  ──wss──┐                       ┌── workshop dashboard
 Notekeeper      ──wss──┤   Caddy :443 (TLS)    │   (relay client: KeyHolder HELLO)
                        └──► 127.0.0.1:21042 ◄──┘   r2-hive (bucket fan-out
                                                      + /word-code + catchup)
```

r2-hive has **no native TLS** — it binds plain HTTP/WS on port 21042. A
reverse proxy terminates `wss://`. Bind r2-hive to loopback so only the
proxy reaches it.

## A. Build & install r2-hive (on the relay host)

```sh
cd /path/to/r2-hive
cargo build --release --features systemd     # drop the feature → use Type=simple below
sudo install -m0755 target/release/r2-hive /usr/local/bin/r2-hive
sudo install -m0755 target/release/r2hive  /usr/local/bin/r2hive
```

## B. systemd unit (system-level relay, loopback-bound)

Adapted from `r2-hive/crates/r2-hive-bin/packaging/systemd/r2-hive.service`:

```ini
# /etc/systemd/system/r2-hive.service
[Unit]
Description=Reality2 hive relay
After=network-online.target
Wants=network-online.target

[Service]
Type=notify
Environment=RUST_LOG=info
ExecStart=/usr/local/bin/r2-hive --bind 127.0.0.1 --port 21042 --no-usb \
          --max-connections 2000 --buffer-size 1000 --name relay
DynamicUser=yes
StateDirectory=r2
RuntimeDirectory=r2
ConfigurationDirectory=r2
Restart=on-failure
RestartSec=2s
WatchdogSec=30s
NoNewPrivileges=true
ProtectSystem=strict
ProtectKernelTunables=true
ProtectKernelModules=true
RestrictRealtime=true
LockPersonality=true
SystemCallArchitectures=native

[Install]
WantedBy=multi-user.target
```

```sh
sudo systemctl daemon-reload && sudo systemctl enable --now r2-hive
journalctl -u r2-hive -f
# expect: "r2-hive listening on 127.0.0.1:21042" and "Plugins: word-codes, dashboard"
```

> Built **without** `--features systemd`? Change `Type=notify` →
> `Type=simple` and remove `WatchdogSec=`.

## C. TLS reverse proxy (Caddy — automatic certificates)

```caddy
# /etc/caddy/Caddyfile
relay.example.org {
    reverse_proxy 127.0.0.1:21042   # WebSocket upgrade on /r2 and /r2/mgmt handled automatically
}
```

```sh
sudo systemctl reload caddy
```

nginx works too — add the standard `Upgrade`/`Connection` header pair on
the `location /` block. The relay path is `/r2`; clients use
`wss://relay.example.org/r2`.

## D. Point the dashboard at it

```sh
r2-workshop-dashboard --bind 0.0.0.0 --port 21042 \
                      --relay-url wss://relay.example.org/r2
```

On boot `relay::spawn_relay_session` connects, signs the KeyHolder
HELLO, runs catchup, and fans sensor frames + JOIN_RESPONSEs through the
bucket. Invite QRs auto-include the relay path when `--relay-url` is set
(`access.rs`). The relay session only spawns when `--relay-url` **and** a
loaded TG key are both present — generate the key first:

```sh
cargo run -p r2-workshop-tg -- keygen      # writes ~/.config/r2-workshop/tg_signer/tg_priv.bin
```

## E. Smoke test

Liveness through TLS:

```sh
curl -s https://relay.example.org/health   # {"status":"ok","class":"ai.reality2.wayfinder"}
curl -s https://relay.example.org/stats     # connection + frame counters
```

End-to-end two-peer broadcast + catchup (Python, needs `cryptography` +
`websockets`):

```python
# relay_smoke.py — usage: RELAY_URL=wss://relay.example.org/r2 python3 relay_smoke.py
import asyncio, json, time, hashlib, os, websockets
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
from cryptography.hazmat.primitives import serialization

URL = os.environ.get("RELAY_URL", "ws://127.0.0.1:21042/r2")
TG  = hashlib.sha256(b"smoke-test-tg").hexdigest()[:16]

def dev():
    sk = Ed25519PrivateKey.generate()
    pk = sk.public_key().public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw)
    return sk, pk.hex()

def hello(sk, did):
    ts = int(time.time()); sig = sk.sign(f"{TG}:{did}:{ts}".encode())
    return json.dumps({"type":"hello","version":1,"trust_group":TG,
                       "device_id":did,"timestamp":ts,"signature":sig.hex()})

async def peer():
    sk, did = dev(); ws = await websockets.connect(URL, max_size=None)
    await ws.send(hello(sk, did)); await asyncio.wait_for(ws.recv(), 5); return ws

async def main():
    a, b = await peer(), await peer(); await asyncio.sleep(0.3)
    frame = bytes([0xFF,0x01]) + b"\x11"*32 + b"\x22"*16 + b"hi"
    await a.send(frame)
    got = await asyncio.wait_for(b.recv(), 5)
    print("broadcast:", "PASS" if got == frame else "FAIL")
    c = await peer(); await c.send(json.dumps({"type":"catchup","since":int(time.time())-60}))
    cg = await asyncio.wait_for(c.recv(), 5)
    print("catchup:  ", "PASS" if cg == frame else "FAIL")
    for w in (a,b,c): await w.close()

asyncio.run(main())
```

## F. Operational notes

- **wss only** for the public path — Notekeeper rejects non-`wss` off
  localhost.
- Keep the dashboard's **10 Hz/sensor throttle** (`relay.rs`,
  `RELAY_FRAME_MIN_INTERVAL_MS`). r2-hive applies mpsc backpressure
  rather than the `ECONNRESET` the public relay gave under flood, but the
  throttle stays.
- **`--max-connections`** is the viewer ceiling; past it r2-hive closes
  with WS code `4429`.
- One controller per TG is fine. Running **two** controllers on one TG
  through the relay would collapse them to one `hive_id` (the dashboard
  signs HELLO with the TG key, not a per-device key) — give them
  per-device HELLO keys if that case arises.

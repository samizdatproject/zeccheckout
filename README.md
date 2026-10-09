# ZecCheckout

A self-hosted Zcash checkout service for merchants who want to accept shielded payments. The server holds only a viewing key.

- **The server holds only a Unified Full Viewing Key (UFVK).** It never sees a seed or a spending key, so if it is compromised, no funds can be stolen.
- **Every invoice gets its own shielded address.** ZecCheckout derives a fresh diversified Unified Address (Orchard + Sapling receivers, never transparent) for each invoice and presents it as a ZIP-321 payment URI and QR code.
- **Payments are detected locally.** The server downloads compact blocks from lightwalletd and trial-decrypts every Sapling output and every Orchard and Ironwood action with the viewing key (Ironwood is the NU6.3 pool; its notes go to the same Orchard receiver). It maps each decrypted note back to its invoice by diversifier index, marks the invoice **paid** after *N* confirmations, and sends the merchant an **HMAC-signed webhook**.
- **No third-party code.** There is an embeddable checkout widget and a small demo shop, and neither loads anything from a third party: no CDN, no fonts, no analytics, and a strict CSP enforces this.

```
 customer wallet ──(shielded tx)──► Zcash network ──► lightwalletd
                                                          │ compact blocks (all of them)
                                                          ▼
 shop backend ──POST /api/v1/invoices──► ZecCheckout (UFVK only) ──trial-decrypt──► invoice paid
      ▲                                     │   │
      └──── signed webhook invoice.paid ────┘   └── /pay/{id} checkout page + widget.js ──► customer browser
```

Status: working prototype. It is tested on Zcash testnet with a real wallet payment ([testnet run](docs/demo/TESTNET_RUN.md)), with live lightwalletd scanning, and with an in-process end-to-end test. Read [Limitations](#limitations) before using it with real money.

## Why per-invoice diversified addresses instead of memos?

Compact blocks carry only the first 52 bytes of each note ciphertext. That is enough to learn the recipient and the value, but not the memo. Reading a memo means asking lightwalletd for the full transaction (`GetTransaction(txid)`), and that request tells the operator exactly which transaction belongs to the merchant.

ZecCheckout therefore never fetches individual transactions. Each invoice gets a unique diversified address, and the diversifier index recovered from the decrypted note identifies the invoice directly. To any outside observer, diversified addresses derived from the same key cannot be linked to each other, so customers cannot connect two invoices to one merchant wallet just by comparing addresses.

You can optionally add a ZIP-321 memo (`ZC_INCLUDE_MEMO=true`). It contains only your `order_id` ("Order ord_42"), never the invoice ID, so the merchant's own wallet shows which order a payment was for. ZecCheckout does not use the memo for matching. Note that the memo is also stored in the customer's wallet history.

## Quickstart (testnet)

You need Rust 1.88 or newer. Nothing else is required: SQLite is bundled, and no proving parameters are needed because nothing is ever spent.

```bash
# 1. Create a throwaway TESTNET wallet. The seed goes to a file; only the UFVK is printed.
cargo run --bin zc-keygen -- ~/zc-testnet-seed.txt
#    -> UFVK: uviewtest1...

# 2. Run the checkout server and the demo shop against public testnet lightwalletd
ZC_UFVK=uviewtest1... scripts/demo.sh

# 3. Open http://127.0.0.1:8081 and buy a coffee. Pay the QR code with a testnet
#    wallet holding TAZ. The order flips to "PAID — order fulfilled" once the
#    signed webhook arrives (after 3 confirmations).
```

To use a production wallet, export the UFVK from that wallet (for example `zcash-devtool`, Zallet, YWallet, or any wallet that exports a UFVK). Do not use `zc-keygen` for production. Mainnet: `ZC_NETWORK=mainnet` (default lightwalletd `https://zec.rocks:443`).

Other commands:

```bash
zeccheckout tip              # print lightwalletd chain tip
zeccheckout scan FROM TO     # scan a block range with your UFVK and list incoming outputs
```

## Merchant API

All `/api/v1` calls need the header `Authorization: Bearer $ZC_API_KEY`.

```http
POST /api/v1/invoices
{"amount_zec": "0.0025", "order_id": "ord_42", "description": "Flat White", "expires_in_secs": 1800}

201 Created
{"id": "1d81f04e…", "status": "pending", "amount_zat": 250000, "amount_zec": "0.0025",
 "address": "utest1…", "payment_uri": "zcash:utest1…?amount=0.0025&label=…",
 "checkout_url": "https://pay.example.com/pay/1d81f04e…", "expires_at": 1791485645, ...}
```

| Endpoint | Auth | Purpose |
|---|---|---|
| `POST /api/v1/invoices` | bearer | Create an invoice. Pass `amount_zec` (decimal string) or `amount_zat`. |
| `GET /api/v1/invoices[?limit=]` | bearer | List invoices. |
| `GET /api/v1/invoices/{id}` | bearer | Invoice detail. |
| `GET /pay/{id}` | id is the capability | Hosted checkout page (`?embed=1` for iframe mode). |
| `GET /i/{id}` | id is the capability | Minimal public status, used by the widget. It contains no address, txid or order data. |
| `GET /i/{id}/qr.svg` | id is the capability | QR code of the ZIP-321 URI, rendered server-side. |
| `GET /assets/widget.js` | — | Embeddable widget. |
| `GET /healthz` | — / bearer | Public: liveness and a `synced` flag only. With the API key it also returns the chain tip, scanned height and webhook counters. The counters are hidden from the public because they would reveal sales volume. |

Invoice IDs are random 128-bit values.

**Invoice statuses:**

| Status | Meaning |
|---|---|
| `pending` | Nothing received yet. |
| `underpaid` | Some funds received, but less than the amount. |
| `detected` | The full amount has been mined, with fewer than N confirmations. |
| `paid` | The full amount has at least N confirmations. This status is final. |
| `expired` | The invoice expired with nothing received. Late payments are still credited. |

The checkout page also shows "payment seen in mempool" as a hint for the customer. It never affects the invoice status.

## Embedding the widget

```html
<div data-zeccheckout-invoice="INVOICE_ID"></div>
<script src="https://pay.example.com/assets/widget.js" async></script>
<script>
  document.querySelector('[data-zeccheckout-invoice]')
    .addEventListener('zeccheckout:status', e => console.log(e.detail.status));
</script>
```

The widget loads an iframe from your own ZecCheckout server. Add your shop's origin to `ZC_FRAME_ANCESTORS`. Only fulfil orders on the signed webhook, never on the browser event.

## Webhooks

ZecCheckout sends a `POST` to `ZC_WEBHOOK_URL` on every status transition: `invoice.underpaid`, `invoice.detected`, `invoice.paid`, `invoice.expired`, and `invoice.pending` (sent when a chain reorg removes an unconfirmed payment).

```
ZecCheckout-Signature: t=1791483900,v1=hex(HMAC_SHA256(ZC_WEBHOOK_SECRET, "1791483900." + raw_body))
{"event":"invoice.paid","event_id":"1d81f04e…-3","seq":3,"created":1791483900,
 "invoice":{"id":"…","order_id":"ord_42","status":"paid","amount_zat":250000,"amount_zec":"0.0025",
            "received_zat":250000,"confirmed_zat":250000,"paid_at":1791483900}}
```

**How to verify a webhook:**
- Compute the HMAC over the **raw** body and compare it in constant time.
- Reject a webhook whose `t` is more than 5 minutes old.
- Use `event_id` as the idempotency key. Ignore any event whose `seq` is not greater than the last `seq` you applied for that invoice.

**Delivery:**
- Webhooks go through a persistent SQLite outbox, so they survive restarts.
- Failed deliveries are retried with exponential backoff (10 s, then doubling, up to 1 h) for 12 attempts.
- Events for one invoice are delivered strictly in order. A later event is held back while an earlier one is still being retried.
- Redirects are not followed.
- After the last retry an event is dropped. Reconcile with `GET /api/v1/invoices` if your endpoint was down for hours.

**Contents:** the payload contains no addresses, txids or memos.

Python verification:

```python
import hmac, hashlib, time
def verify(secret: bytes, header: str, body: bytes, tolerance=300) -> bool:
    parts = dict(p.split("=", 1) for p in header.split(","))
    t = int(parts["t"])
    if abs(time.time() - t) > tolerance: return False
    mac = hmac.new(secret, f"{t}.".encode() + body, hashlib.sha256).hexdigest()
    return hmac.compare_digest(mac, parts["v1"])
```

The demo shop (`src/bin/demo-shop.rs`) shows a complete integration in Rust in about 230 lines.

## Configuration

All configuration is through environment variables; see [`.env.example`](.env.example).

| Variable | Default | Notes |
|---|---|---|
| `ZC_NETWORK` | `testnet` | `testnet` or `mainnet`. |
| `ZC_UFVK` / `ZC_UFVK_FILE` | required | Viewing key only. The server refuses input that looks like a seed phrase or spending key. |
| `ZC_LIGHTWALLETD` | `https://testnet.zec.rocks:443` (testnet) / `https://zec.rocks:443` (mainnet) | Use your own instance; see the threat model. |
| `ZC_API_KEY` | required | At least 24 characters. |
| `ZC_WEBHOOK_URL`, `ZC_WEBHOOK_SECRET` | optional | The secret must be at least 24 characters. |
| `ZC_MIN_CONFIRMATIONS` | `3` | Use 10 or more on mainnet for large amounts. |
| `ZC_INVOICE_TTL_SECS` | `3600` | Per-invoice override: `expires_in_secs`. |
| `ZC_BIND`, `ZC_PUBLIC_URL` | `127.0.0.1:8080` | Bind to localhost and put an onion service or a TLS reverse proxy in front. Turn the proxy's access log off; see the threat model. |
| `ZC_FRAME_ANCESTORS` | `'self'` | Origins allowed to embed the checkout. |
| `ZC_DIVERSIFIER_START` | `1000000` | Invoice addresses start here, so they never collide with the low indices your normal wallet hands out. |
| `ZC_BIRTHDAY` | current tip | First block to scan on first start. |
| `ZC_INCLUDE_MEMO` | `false` | Adds a ZIP-321 memo `Order <order_id>`. |

## Threat model

### What the server is trusted with

The server holds a UFVK, which can **see** all incoming and outgoing shielded activity of the account: amounts, memos (if they were fetched), and counterparties' change. It **cannot spend**.

If the server is compromised, the attacker learns the merchant's payment history and can create fake "paid" states or webhooks if they also steal `ZC_WEBHOOK_SECRET`. They cannot steal funds.

To limit exposure:
- Use a **dedicated account** for the shop, so the UFVK does not reveal your personal wallet.
- Keep the SQLite database on encrypted storage.

### What the lightwalletd operator can learn

**Learns:**
- Your server's IP address, unless you use Tor or your own instance.
- That a light client is following the chain, and how often it polls (every `ZC_POLL_SECS`).
- That it fetches the whole mempool periodically.
- The gRPC user agent, which is the generic `grpc tonic/x` with no product name.

**Does not learn:**
- Which outputs belong to you. Trial decryption happens locally on *all* compact outputs, just as in a light wallet.
- Your addresses or the viewing key.
- Which transactions you care about. ZecCheckout never calls `GetTransaction`, `GetTaddressTxids`, or any other per-transaction or per-address RPC.
- Invoice amounts.

**Can do:**
- **Withhold or delay blocks**, which delays or blocks payment detection.
- **Serve a fabricated chain.** A malicious lightwalletd could serve fabricated compact blocks containing notes encrypted to your addresses. ZecCheckout checks hash-chain continuity (`prev_hash`) and handles reorgs. Confirmations count only blocks that have actually been fetched and linked, not the tip height the server reports. It does **not** verify proof-of-work or note commitment tree roots. So a malicious lightwalletd can make an unpaid invoice look paid.

**Mitigation: run your own node.** For real money, run your own `zebrad` + lightwalletd (or Zaino). Self-hosting removes both the metadata exposure and the trust in someone else's chain view. If you must use a public server, reach it over Tor, for example through a local Tor client and an onion lightwalletd endpoint, or a transparent Tor proxy for the process. Built-in SOCKS support is on the roadmap.

ZecCheckout also guards against a hostile or buggy server crashing the scanner: compact outputs of the wrong length are skipped instead of crashing. The upstream conversion panics on them, and a unit test covers this case.

### What customers and the public can learn

- **Customers** see only their own invoice's address. Two customers cannot tell that their invoices go to the same merchant, because diversified addresses are unlinkable.
- **Invoices are shielded-only.** Address generation never includes a transparent receiver, and the ZIP-321 builder refuses transparent-only addresses.
- **On chain** the payment is an ordinary shielded transaction.
- **The checkout page:**
  - **Loads no third-party resources.** CSP `default-src 'none'; script-src 'self'; …`.
  - **Leaks no referrer.** `Referrer-Policy: no-referrer`.
  - **Sets no cookies.**
- **The public status endpoint** reveals only status and amounts, to holders of the 128-bit invoice ID.

### What ZecCheckout stores and logs

**Logs contain:**
- Invoice IDs.
- Status transitions.
- The pool (Sapling, Orchard or Ironwood) of a detected payment.

Logs contain no amounts and no block heights, so a log reader cannot pin a payment to a block.

**Logs never contain:**
- Requests. The Rust process has no access log, so it records no client IPs or user agents.
- Addresses.
- Txids.
- Memos. ZecCheckout never even downloads memos.

**The database stores:**
- Invoices.
- Matched outputs. Txids are kept for de-duplication.
- The last 1000 block hashes.
- The webhook outbox.

**Your reverse proxy will log customer IPs unless you stop it.** If you put nginx or Caddy in front, their default access logs record client IP, user agent and path. The checkout page polls `GET /i/<invoice id>`, so such a log becomes a table of customer IP → invoice → order. Disable it:

```nginx
location / { proxy_pass http://127.0.0.1:8080; access_log off; }
```

In Caddy, simply do not add a `log` directive. An onion service avoids the problem entirely, and customers don't need to reveal their IP to you either.

**The embedded checkout iframe reports status only to the embedding shop.** It sends its status messages (invoice ID and status) via `postMessage` only to the embedding origin. That origin must appear in `ZC_FRAME_ANCESTORS`; otherwise nothing is sent.

Outputs to other diversifier indices are ignored and not stored. This covers, for example, payments to your regular wallet address.

### Out of scope / known gaps

- No proof-of-work or commitment-root validation (see above).
- No Tor transport built in.
- A reorg deeper than `ZC_MIN_CONFIRMATIONS` after an invoice is `paid` is not reverted. `paid` is final by design.
- No fiat pricing. Amounts are in ZEC.

## Testing

```bash
cargo test                                              # 26 unit + end-to-end tests, offline
cargo test --test testnet_live -- --ignored --nocapture # live: scans 1500 real testnet blocks
cargo clippy --all-targets                              # clean
```

### Offline tests

**Unit tests:**
- UFVK parsing and network checks.
- Shielded-only address derivation, and mapping a diversified address back to its index.
- ZEC decimal parsing without floats.
- ZIP-321 round trip through the `zip321` parser.
- The confirmation and state machine: underpaid, detected, paid, expiry, late payments, and paid being final.
- Webhook signing against a known-answer vector computed independently with Python `hmac`.
- Signature verification: tamper, replay and wrong-secret cases.
- Backoff.
- HTML escaping and single-pass templating.

**Scanner tests:** the scanner tests build real encrypted Sapling outputs, Orchard actions and Ironwood actions addressed to invoice addresses, and to a stranger's addresses. They check that exactly the right outputs are detected, and that malformed outputs are skipped.

**Service tests:**
- A partial Sapling payment plus an Orchard top-up, through to paid.
- Reorg rollback.
- Expiry.
- Payments to the merchant's own non-invoice address being ignored.

**HTTP end-to-end test:**
1. API auth and validation.
2. Invoice creation.
3. Checkout page escaping, CSP headers, and no external URLs.
4. QR code.
5. A payment in synthetic blocks.
6. Confirmation.
7. Signed webhooks delivered to a verifying receiver.
8. Retry backoff on rejection.

### Live test

The live test connects to `testnet.zec.rocks` and downloads the last 1500 blocks. It checks real hash-chain continuity through the reorg detector, trial-decrypts everything, and fetches the mempool.

### Real wallet payment (manual)

On 2026-10-09 the Valar testnet faucet paid 0.125 TAZ to a demo-shop invoice address. ZecCheckout marked the invoice paid and the shop fulfilled the order on the signed webhook. The run, txid and timings are in [`docs/demo/TESTNET_RUN.md`](docs/demo/TESTNET_RUN.md). That run showed that the scanner did not read the Ironwood pool yet; it does now. Since NU6.3, consensus allows an Orchard bundle to pay only the sender's own change, so every payment to an invoice's Orchard receiver arrives as an Ironwood output. NU6.3 activated on mainnet at height 3,428,143, and the mainnet tip was 3,511,638 on 2026-10-09, so this applies to mainnet too. On testnet it activated at 4,134,000.

## Limitations

- The prototype uses a single process and a single SQLite database. That is fine for a small shop, but it is not horizontally scalable.
- The first sync starts at the current tip, or at `ZC_BIRTHDAY`, which is intended. Scanning about 40k testnet blocks takes about 20 s.
- There is no admin UI. Use the API or `sqlite3`.

## How this was built (AI-assisted development disclosure)

This project was written during ZECATHON (after 2026-09-28) with substantial help from an AI coding assistant (Anthropic's Claude, via Claude Code). The assistant did the following:
- Researched the librustzcash crate APIs.
- Drafted most of the code, tests and documentation.
- Ran the builds, tests and live testnet scans.

A human chose the project and the constraints, reviewed the work, and is responsible for the submission.

All code in this repository is new for the hackathon. It depends only on normal open-source crates:
- `zcash_client_backend` (proto and gRPC client)
- `zcash_keys`
- `zip321`
- `orchard`
- `sapling-crypto`
- `zcash_note_encryption`
- `tonic`
- `axum`
- `rusqlite`
- `qrcode`
- `hmac`/`sha2`
- `reqwest`
- `bip39` (keygen helper only)

## License

MIT

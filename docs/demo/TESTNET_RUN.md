# Testnet run: a real wallet payment, end to end

Date: 2026-10-09. Network: Zcash testnet (public, NU6.3 active). Lightwalletd: `testnet.zec.rocks:443`.
Payer: the [Valar testnet faucet](https://faucet.testnet.valargroup.dev), a zecd wallet that pays exactly 0.125 TAZ per claim. It cannot attach a memo, so the payment could only be matched by the invoice's diversified address.

## Result

The payment was received and matched. The invoice became `paid`, and the demo shop fulfilled the order on the HMAC-signed webhook.

| | |
|---|---|
| txid | `24dcd0354daac6af940e63bcf8275e2cbbbf6bf3c8c9f0be111a0338863f5404` |
| Block | 4481341 (`00000da6468d17eb61dc60ded01748dba12c50f1e24836dc1e7e026485ff2921`) |
| Transaction | v6, one Ironwood bundle (2 actions), no Sapling or Orchard data |
| Invoice | `3267a523d2a6415660d58d14f38e2659`: 0.01 ZEC, "Bag of Sapling Beans", diversifier index 1000004 |
| Shop order | `ord_8b351c3058e81f75` |
| Received | 0.125 TAZ. That is more than the 0.01 asked, and an overpayment still counts as `paid`. |

The invoice is for 0.01 because the demo shop has fixed prices and the faucet always pays 0.125. ZecCheckout marks an invoice `paid` once the confirmed total is at least the amount, so the overpayment is reported as `received_zat: 12500000`.

## Timeline (UTC)

| Time | Event | Source |
|---|---|---|
| 09:33:30 | Shop order created; ZecCheckout derives the invoice address | `testnet-demo-shop.log`, `testnet-zeccheckout.log` |
| 09:33:32 | Faucet claim accepted (`202`, claim `c9f22387…`) | `testnet-faucet-claim.log` |
| 09:33:39 | Faucet reports `sent`, txid above, broadcast to 3/3 nodes | `testnet-faucet-claim.log` |
| 09:35:03 | Mined in block 4481341 (1 confirmation), about 1.5 min after broadcast | block time |
| 09:35:45 | Block 4481343: 3 confirmations, the `paid` threshold | block time |
| 09:35–09:46 | **Not detected.** The tx was in the Ironwood pool, which the scanner did not read. | `testnet-timeline.log` |
| 09:47:17 | Restarted with the fix (commit `2165490`); scan height rolled back to 4481329 | `testnet-zeccheckout.log` |
| 09:47:18.50 | `payment output detected pool="ironwood"` | `testnet-zeccheckout.log` |
| 09:47:18.54 | `status change from="pending" to="paid"` (by then about 25 confirmations) | `testnet-zeccheckout.log` |
| 09:47:20.32 | Shop: `signature verified; payment confirmed; order fulfilled` (1.8 s after `paid`) | `testnet-demo-shop.log` |

The mempool hint ("payment seen") and the `detected` state were not shown live in this run. The transaction was mined and confirmed before the fixed scanner was running, so ZecCheckout went straight from `pending` to `paid` on the rescan. The fixed binary also sees Ironwood actions in the mempool stream. See run 2 for why the hint still did not appear.

## What the run found and fixed

NU6.3 is active on testnet (from height 4,134,000) and on mainnet (from height 3,428,143; the mainnet tip was 3,511,638 on 2026-10-09). Since NU6.3, consensus restricts Orchard bundles so that they cannot pay other addresses, only the sender's own change. So a payment to a Unified Address's Orchard receiver is always an Ironwood output, from any wallet. This is a protocol rule, not a quirk of the faucet. Ironwood notes use the Orchard key structure with a v3 note plaintext. Before the fix, ZecCheckout:

- read only `outputs` (Sapling) and `actions` (Orchard) from compact transactions, ignoring `ironwood_actions`, and
- asked lightwalletd only for the Sapling and Orchard pools.

The fix (commit `2165490`, `src/scanner.rs`) trial-decrypts `ironwood_actions` with the same Orchard IVK under `IronwoodDomain`, records hits as pool `ironwood`, and requests every shielded pool from lightwalletd for both blocks and the mempool. It sends an empty `pool_types` list, which the protocol defines as Sapling + Orchard + Ironwood, so older lightwalletd servers that don't know the Ironwood pool type do not reject the request. A new unit test encrypts a real v3 note to an invoice address and checks that it is detected. It also checks that a v2 note placed in the Ironwood list, or a v3 note in the Orchard list, is rejected. `zeccheckout scan 4481341 4481341` with the fixed binary prints:

```
height=4481341 pool=ironwood diversifier_index=1000004 value_zat=12500000
```

## Run 2 (2026-10-10): live, with the fixed scanner

This time the server was already running with the Ironwood fix (commit `34daa27`, `testnet.zec.rocks`, min_conf 3, poll 15 s). The invoice was created directly through the API: `248f0c90f441e999e68e390d3d198d4d`, 0.1 TAZ, "Espresso (live faucet demo)". The faucet paid 0.125.

| Time (UTC) | Event | Source |
|---|---|---|
| 11:07:20 | Faucet claim `839573b8…` accepted; checkout page shows "Waiting for payment" | `live/faucet-claim.log`, `live/checkout-pending.png` |
| (seconds later) | Faucet `sent`, txid `98510eb24fcd5eae6aa93e6e2cf1ae718d25d8ad143894bb2096e36e22ba57d8`, 3/3 nodes | `live/faucet-claim.log` |
| 11:07:22 | Header time of block 4485031, which contains the tx | `live/chain-analysis.txt` |
| 11:07:55 / 11:07:58 | Header times of 4485032 and 4485033 (3 confirmations) | `live/chain-analysis.txt` |
| 11:08:17.306 | `payment output detected … pool="ironwood"`, with no restart or rescan | `live/zeccheckout.log` |
| 11:08:17.313 | `status change from="pending" to="paid"` | `live/zeccheckout.log` |
| 11:08:18 | Checkout page shows "Paid — thank you! 0.125 ZEC confirmed." | `live/poller-timeline.log`, `live/checkout-paid.png` |

The payment was detected live, 57 s after the claim, and needed no intervention. Two things did not show up, and neither is a ZecCheckout bug:

- **No `detected` state.** Testnet produced blocks 4485031–4485033 within about 36 s of header time. Header times run 8–80 s ahead of arrival (measured in `live/chain-analysis.txt`), so the three blocks reached lightwalletd close together. One 15 s scan step therefore saw the payment already at 3 confirmations. Commit `19c7cbc` now records `detected` and then `paid` in that case. The webhooks are `invoice.detected` (seq n) then `invoice.paid` (seq n+1), with the same timestamp. A checkout page that polls every 2 s still jumps from pending to paid.
- **No mempool hint.** The faucet pushes its transaction straight to the Zakura nodes, which mine it, and the transaction was in a block within seconds. `testnet.zec.rocks` also misses most mempool transactions: in a sample taken just after the run, it held 4 of the 18 transactions a Zakura node held at the same moments. The mempool request does return Ironwood (33 actions seen in one snapshot), so the code path works; the faucet's transaction simply never reached that mempool while it was being polled.

## Run 3 (2026-10-11): full shop flow, every state live

Build `19c7cbc`, min_conf 6, poll 3 s. Faucet → shop invoice `6d7de3b8…`, txid `858db20bfaa2948fc0d170769bf4d801fc3803ba4653530258c16ac57c6b6580`.

| Time (UTC) | Event |
|---|---|
| 11:09:06 | Mempool hint shown, 3 s after the claim |
| 11:09:55 | `detected` |
| 11:11:08 | `paid` (6 confirmations) |

Webhook fulfilment was delayed 10 min by a webhook-secret mismatch in the start script (an operator configuration error). After the fix, `detected` and then `paid` were delivered in order.

Details, screenshots and the captioned demo video are in [`live2/`](live2/README.md).

## Files

- `live/`: run 2 files: `checkout-pending.png`, `checkout-paid.png`, `zeccheckout.log`, `faucet-claim.log`, `poller-timeline.log`, `chain-analysis.txt` (block header times, an arrival probe, a mempool comparison).
- `testnet-checkout-paid.png`: hosted checkout page `/pay/{id}` showing "Paid — thank you! 0.125 ZEC confirmed." (headless Chrome).
- `testnet-shop-order-fulfilled.png`: demo shop order page, with "PAID — order fulfilled ✓ (verified webhook)" next to the embedded widget.
- `testnet-zeccheckout.log`: server log for both runs (ANSI stripped; repeated DNS-retry warnings collapsed).
- `testnet-demo-shop.log`, `testnet-faucet-claim.log`, `testnet-timeline.log`: shop log, claim poller, and a 2-second poll of `/i/{id}`, `/healthz` and the shop order status.
- `testnet-webhook-paid.json`: the `invoice.paid` webhook body the shop verified (the signature header is not stored).
- `testnet-invoice.json`, `testnet-healthz.json`: final invoice record and health counters (3 webhooks delivered: two `invoice.expired` for earlier unpaid orders, one `invoice.paid`).

No seed, UFVK, API key or webhook secret appears in these files.

## Reproduce

```bash
ZC_UFVK_FILE=/path/to/ufvk.txt scripts/demo.sh          # testnet by default
curl -s -X POST 127.0.0.1:8081/buy -d product=beans -o /dev/null -w '%{redirect_url}\n'
# take the invoice address from GET /api/v1/invoices (bearer ZC_API_KEY), then:
curl -s -X POST https://faucet.testnet.valargroup.dev/api/claim \
     -H 'content-type: application/json' -d '{"address":"utest1…"}'
```

Faucet limits: one claim per IP and one per address per 24 h, and a global cap of 150 claims per 24 h. On this run the global cap was exhausted, and the claim succeeded 53 minutes later, when `Retry-After` expired. Buy right before you claim, because demo-shop invoices expire after 30 minutes. A late payment is still credited.

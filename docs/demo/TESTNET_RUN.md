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

The mempool hint ("payment seen") and the `detected` state were not shown live in this run. The transaction was mined and confirmed before the fixed scanner was running, so ZecCheckout went straight from `pending` to `paid` on the rescan. The fixed binary now also requests the Ironwood pool in `GetMempoolStream`, so the next run should show both.

## What the run found and fixed

NU6.3 is active on testnet. The faucet's wallet holds its funds in the new Ironwood pool, so its payment to a Unified Address went to the address's Orchard receiver inside an Ironwood bundle. Ironwood notes use the Orchard key structure with a v3 note plaintext. Before the fix, ZecCheckout:

- read only `outputs` (Sapling) and `actions` (Orchard) from compact transactions, ignoring `ironwood_actions`, and
- asked lightwalletd only for the Sapling and Orchard pools.

The fix (commit `2165490`, `src/scanner.rs`) trial-decrypts `ironwood_actions` with the same Orchard IVK under `IronwoodDomain`, records hits as pool `ironwood`, and requests `PoolType::Ironwood` for both blocks and the mempool. A new unit test encrypts a real v3 note to an invoice address and checks that it is detected. It also checks that a v2 note placed in the Ironwood list, or a v3 note in the Orchard list, is rejected. `zeccheckout scan 4481341 4481341` with the fixed binary prints:

```
height=4481341 pool=ironwood diversifier_index=1000004 value_zat=12500000
```

## Files

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

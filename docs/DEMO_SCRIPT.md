# ZecCheckout — 2-minute demo video script

Format: screen recording only (no face), 1080p, captions burned in or as an
.srt. Narration optional; the captions below carry the story. Target length
1:55–2:00.

## Before recording (setup, not shown)

Pick ONE payment path:

- **A. Testnet (preferred if you can get TAZ):** `cargo run --bin zc-keygen -- ~/demo-seed.txt`,
  run `ZC_UFVK=uviewtest1... ZC_MIN_CONFIRMATIONS=1 scripts/demo.sh`. Pay from a testnet-capable
  wallet holding TAZ (e.g. zcash-devtool / Zingo testnet build). Zashi store builds are mainnet-only.
- **A2. Testnet with the Valar faucet as the "customer" (no wallet needed, proven on 2026-10-09, see `docs/demo/TESTNET_RUN.md`):**
  run `ZC_UFVK_FILE=ufvk.txt ZC_MIN_CONFIRMATIONS=1 scripts/demo.sh`. Buy "Bag of Sapling Beans" (0.01) right before
  recording the payment shot. In a second browser tab, open https://faucet.testnet.valargroup.dev and paste the
  invoice address ("Pay manually" on the checkout). Alternatively, `curl -X POST …/api/claim -d '{"address":"utest1…"}'`.
  The faucet pays 0.125 TAZ, so the invoice shows as overpaid and still `paid`. Limits: one claim per IP and per address per
  24 h, and a global cap of 150/24 h. Check `GET /api/status` → `capRemaining` first, and do a dry run on a different day
  or IP from the recording. The faucet pays from the Ironwood pool, so you need ZecCheckout at commit `2165490` or later.
  For the video, swap shot 0:47–1:05 for the faucet tab and use the A2 caption 5 below.
- **B. Mainnet tiny payment (simplest with Zashi):** `cargo run --bin zc-keygen -- --mainnet ~/demo-mainnet-seed.txt`
  (24 words; restore later in Zashi to recover the coins), then
  `ZC_NETWORK=mainnet ZC_UFVK=uview1... ZC_MIN_CONFIRMATIONS=1 scripts/demo.sh`.
  Pay 0.001 ZEC ("Shielded Espresso") from Zashi. Cost: ~0.001 ZEC + fee.

Use `ZC_MIN_CONFIRMATIONS=1` for the video so "paid" lands ~75 s after mining; say so in a caption.
Have three windows ready: browser (shop), terminal (server logs), phone mirror or Zashi screenshot area.
Do a full dry run first. Blur nothing — there are no secrets on screen (the UFVK is not shown; never show the seed file).

## Shot list

| Time | Screen | Caption |
|---|---|---|
| 0:00–0:08 | Title card: "ZecCheckout — shielded checkout you host yourself" | "Accept private Zcash payments. Your server never holds a spending key." |
| 0:08–0:20 | Terminal: `zeccheckout serve` startup log ("viewing key only", lightwalletd, synced) | "The server is configured with a Unified *Full Viewing Key* only. Compromise it and no funds can move." |
| 0:20–0:32 | Browser: demo shop menu → click "Buy with shielded ZEC" on Espresso | "A demo shop creates an invoice server-to-server via the merchant API." |
| 0:32–0:47 | Order page with the embedded checkout (QR, amount, "Fully shielded") | "Each invoice gets a fresh diversified shielded address — Orchard + Sapling, never transparent — as a ZIP-321 QR." |
| 0:47–1:05 | Phone/Zashi: scan QR, amount pre-filled, send | "Customer scans with Zashi. Amount and address come from the ZIP-321 URI." |
| 1:05–1:15 | Checkout status flips to "Payment seen — waiting to be mined" | "Detected in the mempool by trial-decrypting compact transactions locally." |
| 1:15–1:35 | Status → "Payment received — confirming" → "Paid — thank you!"; terminal shows `payment output detected` / `status change … paid` | "Matched by diversifier index — no memo fetch, no GetTransaction call, so lightwalletd never learns which tx is ours." |
| 1:35–1:47 | Shop panel turns green: "PAID — order fulfilled ✓ (verified webhook)"; shop log "signature verified" | "The shop fulfils only on an HMAC-signed webhook, never on the browser." |
| 1:47–1:57 | README threat-model section scroll; DevTools Network tab showing only same-origin requests | "No CDNs, no trackers, strict CSP, no request logs. Threat model in the README: run your own lightwalletd, or use Tor." |
| 1:57–2:00 | End card: repo URL, "MIT · Rust · librustzcash" | "ZecCheckout — open source. Built for ZECATHON (Shielded Payments)." |

Faucet version (A2), caption 5: "A testnet faucet plays the customer: a real zecd wallet sends to the invoice address. No memo is needed, because the address itself identifies the invoice."
Faucet version (A2), caption 7: also show `payment output detected pool="ironwood"` in the log. Ironwood is the NU6.3 pool, and it is scanned with the same viewing key.

## Captions (.srt)

```
1
00:00:00,000 --> 00:00:08,000
Accept private Zcash payments. Your server never holds a spending key.

2
00:00:08,000 --> 00:00:20,000
The server is configured with a Unified Full Viewing Key only. Compromise it and no funds can move.

3
00:00:20,000 --> 00:00:32,000
A demo shop creates an invoice server-to-server via the merchant API.

4
00:00:32,000 --> 00:00:47,000
Each invoice gets a fresh diversified shielded address (Orchard + Sapling, never transparent) as a ZIP-321 QR code.

5
00:00:47,000 --> 00:01:05,000
The customer scans with Zashi. Amount and address come from the ZIP-321 URI.

6
00:01:05,000 --> 00:01:15,000
Seen in the mempool: compact transactions are trial-decrypted locally with the viewing key.

7
00:01:15,000 --> 00:01:35,000
Matched by diversifier index. No memo fetch, no GetTransaction call: lightwalletd never learns which transaction is ours. (1 confirmation for this demo; default is 3.)

8
00:01:35,000 --> 00:01:47,000
The shop fulfils the order only on an HMAC-signed webhook, never on what the browser says.

9
00:01:47,000 --> 00:01:57,000
No CDNs, no trackers, strict CSP, no request logs. Threat model in the README: self-host lightwalletd or use Tor.

10
00:01:57,000 --> 00:02:00,000
ZecCheckout. Open source, MIT. Built for ZECATHON, Shielded Payments track.
```

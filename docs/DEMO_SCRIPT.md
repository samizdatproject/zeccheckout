# ZecCheckout — 2-minute demo video script

Format: screen recording only (no face), 1080p, captions burned in or as an
.srt. Narration optional; the captions below carry the story. Target length
1:55–2:00.

## Before recording (setup, not shown)

Pick ONE payment path:

- **A. Testnet (preferred if you can get TAZ):** `cargo run --bin zc-keygen -- ~/demo-seed.txt`,
  run `ZC_UFVK=uviewtest1... ZC_MIN_CONFIRMATIONS=1 scripts/demo.sh`. Pay from a testnet-capable
  wallet holding TAZ (e.g. zcash-devtool / Zingo testnet build). Zashi store builds are mainnet-only.
- **A2. Testnet with the Valar faucet as the "customer" (no wallet needed; proven twice, see `docs/demo/TESTNET_RUN.md`):**
  Needs ZecCheckout at commit `19c7cbc` or later. Since NU6.3, payments to Orchard receivers arrive as Ironwood outputs (on mainnet too).
  1. **Pick the window.** The faucet allows one claim per IP per 24 h and one per address per 24 h, with a global cap of 150/24 h.
     The last claim from this machine was 2026-10-10 11:07:20Z, so the next one opens about **2026-10-11 11:08Z**.
     Check `curl -s https://faucet.testnet.valargroup.dev/api/status` → `capRemaining` > 0 just before recording. On 2026-10-09 the cap was empty for 53 min.
     You get one attempt per day, so rehearse every other shot first.
  2. **Start the stack.** Make `detected` visible on screen, and make the logs show a quick poll:
     `ZC_UFVK_FILE=ufvk.txt ZC_MIN_CONFIRMATIONS=6 ZC_POLL_SECS=3 ZC_LOG=info scripts/demo.sh`.
     Testnet blocks arrive in bursts (3 blocks in about 36 s on 2026-10-10). With N=3, `detected` and `paid` can land in the same poll,
     and then the page jumps straight to Paid; the webhooks still show `invoice.detected` then `invoice.paid`. With N=6, the
     "Payment received — confirming" state usually stays on screen for 1–3 minutes. Say "6 confirmations for this demo" in a caption.
  3. **Shop flow on camera.** At http://127.0.0.1:8081, buy "Bag of Sapling Beans" (0.01) no more than 30 min before the claim, because shop invoices expire after 30 min.
     Open "Pay manually" and copy the address.
  4. **Pay.** In a second tab, open https://faucet.testnet.valargroup.dev and paste the address, or run
     `curl -s -X POST https://faucet.testnet.valargroup.dev/api/claim -H 'content-type: application/json' -d '{"address":"utest1…"}'`.
     The faucet pays 0.125 TAZ: the invoice is overpaid and ends `paid` showing "0.125 ZEC confirmed". Mention it in a caption.
  5. **Expect:**
     - **No mempool hint.** The faucet's tx is mined within seconds, and `testnet.zec.rocks`'s mempool misses most txs, so cut shot 1:05–1:15.
     - **Payment received — confirming** about 20–90 s after the claim, then **Paid** after N blocks.
     - **Shop panel turns green** about 2 s after Paid (signed webhook).
     - In the terminal, `payment output detected pool="ironwood"`, then `pending→detected`, then `detected→paid`.
  6. **Backup footage.** If the claim fails (cap or IP limit), use the stills in `docs/demo/` and `docs/demo/live/` together with the logs. Don't fake a live payment.
  For the video, swap shot 0:47–1:05 for the faucet tab and use the A2 captions below.
- **Deterministic alternative (regtest).** A local zebrad regtest chain, with lightwalletd or Zaino, a wallet that can send Ironwood, and blocks mined on demand, would make every state reproducible.
  It would cost a few GB of disk and an afternoon of setup, and no tested recipe exists here. Use it only if A2 fails twice. The faucet path already shows a real public-network payment, which is the stronger demo.
- **B. Mainnet tiny payment (simplest with Zashi):** `cargo run --bin zc-keygen -- --mainnet ~/demo-mainnet-seed.txt`
  (24 words; restore later in Zashi to recover the coins), then
  `ZC_NETWORK=mainnet ZC_UFVK=uview1... ZC_MIN_CONFIRMATIONS=1 scripts/demo.sh`.
  Pay 0.001 ZEC ("Shielded Espresso") from Zashi. Cost: ~0.001 ZEC + fee.

Use `ZC_MIN_CONFIRMATIONS=1` for the mainnet video (B) so "paid" lands ~75 s after mining; for testnet (A2) use 6 (see above). Say which in a caption.
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
Faucet version (A2), caption 6: drop the mempool caption. Use "Mined: the payment is found by trial-decrypting the new block locally (Ironwood pool, same viewing key)."
Faucet version (A2), caption 7: also show `payment output detected pool="ironwood"` and `detected → paid` in the log. Use "(6 confirmations for this demo; default is 3.)"

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

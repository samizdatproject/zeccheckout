# Run 3 (2026-10-11): the full shop flow, live, on testnet

This is the recording run for DEMO_SCRIPT option A2. Build `19c7cbc`, `testnet.zec.rocks`, `ZC_MIN_CONFIRMATIONS=6`, `ZC_POLL_SECS=3`.
The customer was the Valar testnet faucet. The demo shop created the order and verified the webhooks.

| | |
|---|---|
| Order / invoice | `ord_359ae3d03222ed63` / `6d7de3b8d556ec65b18d1a2156c19d90`: "Bag of Sapling Beans", 0.01 ZEC |
| txid | `858db20bfaa2948fc0d170769bf4d801fc3803ba4653530258c16ac57c6b6580` (Ironwood), block 4488510 |
| Received | 0.125 TAZ. The faucet's fixed amount is an overpayment, which still counts as `paid`. |

## Timeline (UTC)

| Time | Event | Screenshot |
|---|---|---|
| 11:08:43 | Shop order created; invoice with a fresh diversified address | `0-shop-menu.png`, `1-pending.png` |
| 11:09:03 | Faucet claim accepted (`202`) | |
| 11:09:06 | Faucet `sent`. The checkout page shows **"Payment seen — waiting to be mined", 0.125 ZEC in the mempool** | `2-mempool-seen.png` |
| 11:09:28 | Block 4488510, which contains the tx (header time) | |
| 11:09:55 | `payment output detected pool="ironwood"`, then `pending → detected`: **"Payment received — confirming"** | `3-detected.png` |
| 11:09:58–11:10:55 | Confirmations 1, 2, 4, 5 seen by the poller. Header times: 4488511 11:09:40, 4488512 11:10:01, 4488513 11:10:05, 4488514 11:10:16, 4488515 11:10:53 | (the page reads "Waiting for 6 confirmations" throughout) |
| 11:11:08 | `detected → paid` (6 confirmations), 2 min 5 s after the claim | `4-paid-webhook-pending.png` |
| 11:21:13 | Shop: `signature verified; payment confirmed; order fulfilled` | `5-paid-fulfilled.png`, `6-checkout-paid.png` |

This run showed every state live: the mempool hint, `detected`, `paid`, and fulfilment.

**Why fulfilment took 10 min:** the server's start script generated a fresh random `ZC_WEBHOOK_SECRET` on every start, so the shop rejected the webhooks (`401 signature mismatch` in both logs). The outbox kept retrying with backoff, as designed.
- At 11:21:13 the server was restarted with the shop's secret, and the two queued webhooks' retry time was reset to "now".
- `invoice.detected` (seq 1) and then `invoice.paid` (seq 2) were delivered in order within a second, and the shop fulfilled the order.
- This was an operator configuration error, not a code change. With matching secrets, delivery takes about 2 s, as in run 1.

## Files

- **Screenshots:** `0`–`6` `*.png` (1280×800, headless Chrome).
- **Logs:** `timeline.log` (2 s poller), `zeccheckout.log` (server log, transport retries omitted), `demo-shop.log`.
- **Webhooks:** `webhooks.json`, the two webhook bodies.
- **Video:**
  - `zeccheckout-demo.mp4`: 1920×1080, 98 s, 12 slides with captions burned in, plus the same captions as a soft subtitle track.
  - `zeccheckout-demo-narrated.mp4`: the same video with a macOS `say` narration track (system default voice, rate 185).
  - `zeccheckout-demo.srt`: the captions.
  - `slides.json`: the slide spec. Rebuild with `python3 scripts/mkvideo.py docs/demo/live2/slides.json OUTDIR /tmp/chrome-profile`. The narration was muxed separately with ffmpeg `adelay` + `amix`.

The video is a captioned slideshow of the real screenshots and logs from this run, not a screen recording.

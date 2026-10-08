#!/usr/bin/env bash
# Runs ZecCheckout + the demo shop locally against Zcash TESTNET.
# Usage: ZC_UFVK=uviewtest1... scripts/demo.sh
#    or: scripts/demo.sh /path/to/ufvk.txt
set -euo pipefail
cd "$(dirname "$0")/.."
if [[ $# -ge 1 ]]; then export ZC_UFVK_FILE="$1"; fi
if [[ -z "${ZC_UFVK:-}" && -z "${ZC_UFVK_FILE:-}" ]]; then
  echo "Provide a testnet UFVK: ZC_UFVK=... $0   (create one with: cargo run --bin zc-keygen -- seed.txt)" >&2
  exit 1
fi
export ZC_NETWORK="${ZC_NETWORK:-testnet}"
export ZC_API_KEY="${ZC_API_KEY:-$(openssl rand -hex 24)}"
export ZC_WEBHOOK_SECRET="${ZC_WEBHOOK_SECRET:-$(openssl rand -hex 24)}"
export ZC_WEBHOOK_URL="${ZC_WEBHOOK_URL:-http://127.0.0.1:8081/webhook}"
export ZC_FRAME_ANCESTORS="${ZC_FRAME_ANCESTORS:-http://127.0.0.1:8081}"
export ZC_MERCHANT_NAME="${ZC_MERCHANT_NAME:-Shielded Coffee}"
export ZC_DB="${ZC_DB:-demo.db}"
export SHOP_API_KEY="$ZC_API_KEY" SHOP_WEBHOOK_SECRET="$ZC_WEBHOOK_SECRET"
cargo build --bins
./target/debug/zeccheckout serve &
ZC=$!
./target/debug/demo-shop &
SHOP=$!
trap 'kill $ZC $SHOP 2>/dev/null' EXIT
echo
echo "  Demo shop:  http://127.0.0.1:8081"
echo "  Checkout:   http://127.0.0.1:8080/healthz"
echo
wait

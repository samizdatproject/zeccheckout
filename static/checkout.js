// ZecCheckout hosted checkout page. Polls only this server; no third parties.
(function () {
  "use strict";
  var root = document.getElementById("checkout");
  if (!root) return;
  var id = root.getAttribute("data-invoice");
  var minConf = root.getAttribute("data-min-conf");
  // Set by the server only if the embedder's origin is in ZC_FRAME_ANCESTORS.
  var parentOrigin = root.getAttribute("data-parent-origin");
  var statusBox = document.getElementById("status");
  var text = document.getElementById("status-text");
  var sub = document.getElementById("status-sub");
  var payArea = document.getElementById("pay-area");
  var expiry = document.getElementById("expiry");
  var last = null;
  var skew = 0;
  var expiresAt = null;

  document.querySelectorAll("[data-copy]").forEach(function (b) {
    b.addEventListener("click", function () {
      var el = document.getElementById(b.getAttribute("data-copy"));
      if (navigator.clipboard) {
        navigator.clipboard.writeText(el.textContent.trim()).then(function () {
          b.textContent = "Copied";
          setTimeout(function () { b.textContent = "Copy"; }, 1500);
        });
      }
    });
  });

  function notifyParent(status) {
    if (parentOrigin && window.parent && window.parent !== window) {
      // Only the invoice id and status leave the frame, and only to the
      // allow-listed embedding origin.
      window.parent.postMessage({ type: "zeccheckout:status", invoice: id, status: status }, parentOrigin);
    }
  }

  function render(s) {
    var cls = s.status;
    var mem = parseFloat(s.seen_in_mempool_zec);
    if (s.status === "pending" && mem > 0) cls = "mempool";
    statusBox.className = "status " + cls;
    switch (cls) {
      case "pending":
        text.textContent = "Waiting for payment…";
        sub.textContent = "Scan the QR code with a shielded Zcash wallet.";
        break;
      case "mempool":
        text.textContent = "Payment seen — waiting to be mined";
        sub.textContent = s.seen_in_mempool_zec + " ZEC in the mempool.";
        break;
      case "underpaid":
        text.textContent = "Partial payment received";
        sub.textContent = s.received_zec + " of " + s.amount_zec + " ZEC received. Please send the remainder to the same address.";
        break;
      case "detected":
        text.textContent = "Payment received — confirming";
        sub.textContent = "Waiting for " + minConf + " confirmations.";
        break;
      case "paid":
        text.textContent = "Paid — thank you!";
        sub.textContent = s.confirmed_zec + " ZEC confirmed.";
        payArea.classList.add("hidden");
        break;
      case "expired":
        text.textContent = "Invoice expired";
        sub.textContent = "Please request a new invoice. Do not send funds to this address.";
        payArea.classList.add("hidden");
        break;
    }
    if (s.status !== last) {
      last = s.status;
      notifyParent(s.status);
    }
  }

  function tickExpiry() {
    if (!expiresAt || last === "paid" || last === "detected" || last === "underpaid") {
      expiry.textContent = "";
      return;
    }
    var left = expiresAt - (Math.floor(Date.now() / 1000) + skew);
    if (left <= 0) { expiry.textContent = "expired"; return; }
    var m = Math.floor(left / 60), sec = left % 60;
    expiry.textContent = "expires in " + m + ":" + (sec < 10 ? "0" : "") + sec;
  }

  function poll() {
    fetch("/i/" + encodeURIComponent(id), { cache: "no-store", credentials: "omit", referrerPolicy: "no-referrer" })
      .then(function (r) { return r.ok ? r.json() : null; })
      .then(function (s) {
        if (!s) return;
        skew = s.now - Math.floor(Date.now() / 1000);
        expiresAt = s.expires_at;
        render(s);
        if (s.status !== "paid" && s.status !== "expired") setTimeout(poll, 4000);
      })
      .catch(function () { setTimeout(poll, 8000); });
  }
  poll();
  setInterval(tickExpiry, 1000);
})();

// Demo shop: polls the SHOP backend (which learns about payment only via the
// signed webhook) and reflects fulfilment.
(function () {
  "use strict";
  var box = document.getElementById("fulfil");
  if (!box) return;
  var oid = box.getAttribute("data-order");
  var text = document.getElementById("fulfil-text");
  var labels = { pending: "awaiting payment", underpaid: "partially paid", detected: "payment received, confirming",
                 paid: "PAID — order fulfilled ✓ (verified webhook)", expired: "expired" };
  function poll() {
    fetch("/order/" + encodeURIComponent(oid) + "/status", { cache: "no-store" })
      .then(function (r) { return r.json(); })
      .then(function (s) {
        text.textContent = labels[s.status] || s.status;
        if (s.fulfilled) { box.classList.add("done"); return; }
        setTimeout(poll, 3000);
      })
      .catch(function () { setTimeout(poll, 6000); });
  }
  poll();
})();

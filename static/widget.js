// ZecCheckout embeddable widget.
//
//   <div data-zeccheckout-invoice="INVOICE_ID"></div>
//   <script src="https://pay.example.com/assets/widget.js" async></script>
//
// Renders the hosted checkout in an iframe served by YOUR ZecCheckout server
// and re-dispatches status changes as a DOM event on the container:
//   el.addEventListener("zeccheckout:status", e => e.detail.status)
// No third-party code, cookies or tracking.
(function () {
  "use strict";
  var me = document.currentScript;
  var origin = me ? new URL(me.src).origin : "";
  function mount(el) {
    if (el.getAttribute("data-zeccheckout-mounted")) return;
    var id = el.getAttribute("data-zeccheckout-invoice");
    if (!/^[0-9a-f]{32}$/.test(id || "")) return;
    el.setAttribute("data-zeccheckout-mounted", "1");
    var f = document.createElement("iframe");
    f.src = origin + "/pay/" + id + "?embed=1";
    f.title = "Zcash payment";
    f.referrerPolicy = "no-referrer";
    f.setAttribute("sandbox", "allow-scripts allow-same-origin allow-popups allow-top-navigation-by-user-activation");
    f.style.cssText = "border:0;width:100%;max-width:420px;height:720px;display:block";
    el.appendChild(f);
    window.addEventListener("message", function (ev) {
      if (ev.origin !== origin || ev.source !== f.contentWindow) return;
      var d = ev.data || {};
      if (d.type !== "zeccheckout:status" || d.invoice !== id) return;
      el.dispatchEvent(new CustomEvent("zeccheckout:status", { detail: { invoice: id, status: d.status } }));
    });
  }
  function scan() {
    document.querySelectorAll("[data-zeccheckout-invoice]").forEach(mount);
  }
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", scan);
  else scan();
})();

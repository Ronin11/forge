// Local times for the portal: every <time data-ts="<unix seconds>"> the
// server renders carries its UTC text as a fallback (for a viewer without
// JavaScript); this replaces that text with the same shape in the viewer's
// own zone. The shape is "Nov 14, 2023, 22:13 UTC" -- month, day, year,
// 24-hour clock, zone name -- so the fallback and the local text read alike.
(function (root) {
  var MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

  // `zone` is an IANA name; left out, the viewer's own.
  function format(ts, zone) {
    var opts = {
      year: "numeric",
      month: "numeric",
      day: "numeric",
      hour: "2-digit",
      minute: "2-digit",
      hourCycle: "h23",
      timeZoneName: "short",
    };
    if (zone) opts.timeZone = zone;
    var parts = {};
    new Intl.DateTimeFormat("en-US", opts).formatToParts(new Date(ts * 1000)).forEach(function (p) {
      parts[p.type] = p.value;
    });
    return (
      MONTHS[Number(parts.month) - 1] + " " + Number(parts.day) + ", " + parts.year + ", " +
      parts.hour + ":" + parts.minute + " " + parts.timeZoneName
    );
  }

  function localize(doc, zone) {
    var els = doc.querySelectorAll("time[data-ts]");
    for (var i = 0; i < els.length; i++) {
      var ts = Number(els[i].getAttribute("data-ts"));
      if (!isFinite(ts)) continue;
      try {
        els[i].textContent = (els[i].getAttribute("data-prefix") || "") + format(ts, zone);
      } catch (e) {
        // Keep the server's UTC text.
      }
    }
  }

  var api = { format: format, localize: localize };
  if (typeof module !== "undefined" && module.exports) module.exports = api;
  if (root.document) localize(root.document);
})(typeof window !== "undefined" ? window : globalThis);

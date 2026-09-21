// The one place the page turns Unix seconds into text. The server sends
// seconds, never strings; every time app.js shows goes through here, in the
// viewer's zone (the browser's own; a test pins it with TZ).
(function (root, factory) {
  const api = factory();
  if (typeof module === 'object' && module.exports) module.exports = api;
  else root.ForgeTime = api;
})(globalThis, () => {
  const pad = n => String(n).padStart(2, '0');
  const known = secs => typeof secs === 'number' && Number.isFinite(secs);

  // Short absolute form in the viewer's zone: 2026-09-21 07:00. '' for a
  // time the server has not recorded (null while a task is still running).
  function fmtTime(secs) {
    if (!known(secs)) return '';
    const d = new Date(secs * 1000);
    return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
  }

  // A length of time in its two largest units: 45s, 5m, 2h 3m, 3d 4h.
  function fmtSpan(secs) {
    if (!known(secs)) return '';
    const s = Math.max(0, Math.floor(secs));
    if (s < 60) return `${s}s`;
    const m = Math.floor(s / 60);
    if (m < 60) return `${m}m`;
    const h = Math.floor(m / 60);
    if (h < 24) return m % 60 ? `${h}h ${m % 60}m` : `${h}h`;
    const d = Math.floor(h / 24);
    return h % 24 ? `${d}d ${h % 24}h` : `${d}d`;
  }

  // An instant relative to now: "5m ago", "in 2h 3m", "just now" inside ten
  // seconds. `now` is Unix seconds, defaulting to the browser's clock.
  function fmtAgo(secs, now = Date.now() / 1000) {
    if (!known(secs)) return '';
    const diff = Math.round(secs - now);
    if (Math.abs(diff) < 10) return 'just now';
    return diff < 0 ? `${fmtSpan(-diff)} ago` : `in ${fmtSpan(diff)}`;
  }

  return { fmtTime, fmtSpan, fmtAgo };
});

// The one shared shell every page in forge-web mounts into: the nav bar
// that names every page on the client contract, and the header strip
// (worker state, both rate windows as gauges, queued/running counts,
// today's spend, a last-updated stamp). Pure rendering, no DOM, no fetch
// — app.js wires the result into the page and keeps it live off
// `/api/snapshot` and `/api/doctor` — so `web/tests/shell.test.js` can
// exercise it under node exactly like time.js/graph.js/workflows.js.
// Every time this shows goes through a `fmtTime` the caller passes in
// (`web/src/time.js`'s), never formatted here — the viewer's local zone,
// keyed by docs/CLIENT.md's "Time".
(function (root, factory) {
  const api = factory();
  if (typeof module === 'object' && module.exports) module.exports = api;
  else root.ForgeShell = api;
})(globalThis, () => {
  const esc = s => String(s ?? '').replace(/[&<>"]/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));
  const usd = n => '$' + (Number(n) || 0).toFixed(2);

  // Every page the client contract names (docs/CLIENT.md), in the order
  // the task gave them. A page with no view built yet still gets an
  // entry here — and a route to a placeholder in app.js — so the nav
  // never claims a page that isn't reachable, and never omits one that's
  // simply not built yet.
  const NAV_PAGES = [
    { key: 'tasks', label: 'tasks', href: '/tasks' },
    { key: 'requests', label: 'requests', href: '/requests' },
    { key: 'projects', label: 'projects', href: '/projects' },
    { key: 'initiatives', label: 'initiatives', href: '/initiatives' },
    { key: 'workflows', label: 'workflows', href: '/workflows' },
    { key: 'jobs', label: 'jobs', href: '/jobs' },
    { key: 'deploys', label: 'deploys', href: '/deploys' },
    { key: 'stats', label: 'stats', href: '/stats' },
    { key: 'graph', label: 'graph', href: '/graph' },
    { key: 'plugins', label: 'plugins', href: '/plugins' },
    { key: 'activity', label: 'activity', href: '/activity' },
    { key: 'messages', label: 'messages', href: '/messages' },
    { key: 'doctor', label: 'doctor', href: '/doctor' },
  ];

  // `g` then a letter jumps to a page — one letter per nav entry, chosen
  // to be unique rather than always the label's own initial (projects
  // and plugins both start with p; deploys and doctor both start with d).
  const SHORTCUT_TARGETS = {
    t: '/tasks', r: '/requests', p: '/projects', i: '/initiatives',
    w: '/workflows', j: '/jobs', d: '/deploys', s: '/stats', g: '/graph',
    l: '/plugins', a: '/activity', m: '/messages', o: '/doctor',
  };

  // The nav bar's inner HTML: every named page as a link, the current
  // one bold. `extraHtml` is a page's own sub-nav (the task view's
  // "task N / workflow run" toggle, the graph view's "files / modules"
  // toggle) appended after the fixed list, exactly as it was before the
  // shell existed.
  function renderNav(activeKey, extraHtml) {
    const items = NAV_PAGES
      .map(p => `<a href="${p.href}" ${p.key === activeKey ? 'style="font-weight:600"' : ''}>${p.label}</a>`)
      .join(' ');
    return items + (extraHtml || '');
  }

  // Across every provider `forge doctor --json`'s `rate_limit` checks
  // name, the one using the most of a window — the single number a
  // header strip has room for. `null` when no check carries that window
  // at all (no attempt has run yet).
  function worstWindow(doctor, pctKey, resetKey) {
    let worst = null;
    for (const c of doctor || []) {
      if (c.name !== 'rate_limit' || c[pctKey] == null) continue;
      if (!worst || c[pctKey] > worst.pct) {
        worst = { pct: c[pctKey], resetsAt: c[resetKey] ?? null, provider: c.provider || '' };
      }
    }
    return worst;
  }

  function gaugeHtml(label, w, fmtTime) {
    if (!w) return `<span class="gauge mute">${esc(label)} —</span>`;
    const pct = Math.round(w.pct * 100);
    const cls = pct >= 100 ? 'gauge failed' : pct >= 80 ? 'gauge warn' : 'gauge mute';
    const reset = w.resetsAt ? ` · resets ${fmtTime(w.resetsAt)}` : '';
    const who = w.provider ? `${esc(w.provider)} ` : '';
    return `<span class="${cls}">${who}${esc(label)} ${pct}%${reset}</span>`;
  }

  // The header strip's inner HTML, from one `forge snapshot` and one
  // `forge doctor --json` read: the worker's state (`snapshot.worker`),
  // both rate windows as gauges with their reset times, queued/running
  // counts (the `queue` check's exact store counts when doctor ran one,
  // else counted from the snapshot's own task list), and today's spend
  // (the `spend` check). `now` is Unix seconds for the last-updated
  // stamp; the caller re-renders with a fresh `now` on every event the
  // live stream delivers, so the stamp tracks the event stream rather
  // than just the poll interval.
  function renderHeaderStrip({ worker, tasks, doctor, now }, fmtTime) {
    const w = worker || {};
    const workerHtml = `<span id="worker" class="${w.running ? 'mute' : 'failed'}">${
      w.running ? `worker pid ${w.pid}${w.stale_binary ? ' (stale binary)' : ''}` : 'worker not running'
    }</span>`;
    const queueCheck = (doctor || []).find(c => c.name === 'queue');
    const queued = queueCheck && queueCheck.queued != null
      ? queueCheck.queued
      : (tasks || []).filter(t => t.state === 'queued').length;
    const running = queueCheck && queueCheck.running != null
      ? queueCheck.running
      : (tasks || []).filter(t => t.state === 'running').length;
    const spendCheck = (doctor || []).find(c => c.name === 'spend');
    const spend = spendCheck && spendCheck.spend_usd != null
      ? `${usd(spendCheck.spend_usd)}${spendCheck.spend_cap_usd != null ? ' of ' + usd(spendCheck.spend_cap_usd) : ''}`
      : '—';
    const fiveHour = worstWindow(doctor, 'five_hour_pct', 'five_hour_resets_at');
    const sevenDay = worstWindow(doctor, 'seven_day_pct', 'seven_day_resets_at');
    return [
      workerHtml,
      gaugeHtml('5h', fiveHour, fmtTime),
      gaugeHtml('7d', sevenDay, fmtTime),
      `<span id="queue" class="mute">${queued} queued · ${running} running</span>`,
      `<span id="spend" class="mute">spend ${spend}</span>`,
      `<span id="live" class="mute">connecting</span>`,
      `<span id="updated" class="mute">${now != null ? `updated ${fmtTime(now)}` : ''}</span>`,
    ].join('\n');
  }

  return { NAV_PAGES, SHORTCUT_TARGETS, renderNav, renderHeaderStrip };
});

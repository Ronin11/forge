// Pure rendering for the /doctor page (web UI task 7, "doctor"): every
// check `forge doctor --json` reports as a row (its state and detail,
// with the fix line drawn beneath whenever a check carries one), plus
// dedicated sections for the checks a client can do something useful
// with beyond showing them: held initiatives get the same budget/
// stop-after control the initiative page uses (`POST
// /api/initiatives/<id>`, task 4's route), retained worktrees get a gc
// control (`POST /api/gc`, `forge gc`), and the rate-limit/spend/
// learning checks get their own structured readout instead of just
// their prose. No DOM, no fetch, so web/tests/doctor.test.js can run it
// under node exactly like initiative.js/deploys.js/stats.js, against a
// fixture built from tests/fixtures/doctor.json.
(function (root, factory) {
  const api = factory();
  if (typeof module === 'object' && module.exports) module.exports = api;
  else root.ForgeDoctor = api;
})(globalThis, () => {
  const esc = s => String(s ?? '').replace(/[&<>"]/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));
  const usd = n => '$' + (Number(n) || 0).toFixed(2);

  // `forge doctor --json`'s `status` (`"ok"`/`"warn"`/`"fail"`) drawn
  // through the same state colours the rest of the app already uses for
  // task states, rather than inventing a parallel palette.
  const STATUS_CLASS = { ok: 'succeeded', warn: 'blocked', fail: 'failed' };

  // One check as a row: its name, its state, its detail, and — whenever
  // it carries one — its fix line underneath (task 7's "every check as a
  // row with its state, detail and fix line").
  function renderCheckRow(c) {
    const cls = STATUS_CLASS[c.status] || 'mute';
    const fix = c.hint ? `<div class="mute">fix: ${esc(c.hint)}</div>` : '';
    return `<tr>
      <td>${esc(c.name)}</td>
      <td class="state ${cls}">${esc((c.status || '').toUpperCase())}</td>
      <td>${esc(c.detail)}${fix}</td>
    </tr>`;
  }

  function renderChecks(checks) {
    const rows = (checks || []).map(renderCheckRow).join('');
    return `<table><thead><tr><th>check</th><th>state</th><th>detail</th></tr></thead>
      <tbody>${rows || '<tr><td colspan="3" class="mute">no checks</td></tr>'}</tbody></table>`;
  }

  // Held initiatives: the same budget/stop-after control
  // `web/src/initiative.js`'s `renderControls` draws on the initiative
  // page, run through the same `POST /api/initiatives/<id>` route (task
  // 4), so raising a stuck initiative's budget or stop-after streak is
  // reachable straight from the doctor page rather than a trip through
  // `/initiatives/<id>`. `held` is a caller-filtered array of
  // `InitiativeRow`s (`state === "held"`), not the doctor document
  // itself — the `initiatives` check only carries prose.
  function renderHeldInitiatives(held) {
    if (!held || !held.length) return '';
    const items = held.map(i => `
      <div class="card">
        <div><b>initiative ${i.id}</b> <span class="mute">${esc(i.project)}</span>${i.held_rule ? ` — held: ${esc(i.held_rule)}` : ''}</div>
        <div class="mute">${esc(i.outcome || '')}</div>
        <form class="ini-set" data-id="${i.id}">
          <label>budget <input type="number" step="0.01" min="0.01" class="ini-budget" placeholder="unset" value="${i.budget_usd != null ? i.budget_usd : ''}"></label>
          <label>stop after <input type="number" step="1" min="1" class="ini-stop-after" value="${i.stop_after_same_rule}"></label>
          <button type="submit">set</button>
        </form>
      </div>`).join('');
    return `<h2>Held initiatives</h2>${items}`;
  }

  // Retained worktrees: the `worktrees` check's own `worktree_ids`
  // (`src/doctor.rs`'s structured field, alongside its prose) plus a gc
  // control — `POST /api/gc`, `forge gc` — that removes whatever gc
  // itself judges safe (clean, and every commit published) and explains
  // the rest; the page re-reads `/api/doctor` afterward the same as
  // every other write control here.
  function renderWorktrees(checks) {
    const c = (checks || []).find(x => x.name === 'worktrees');
    if (!c) return '';
    const ids = c.worktree_ids || [];
    const body = ids.length
      ? `<div>${esc(c.detail)}</div><form class="doctor-gc"><button type="submit">forge gc</button></form>`
      : `<div class="mute">${esc(c.detail)}</div>`;
    return `<h2>Worktrees</h2><div class="card">${body}</div>`;
  }

  // The rate windows, per provider, each `rate_limit` check carries as
  // structured fields (`five_hour_pct`/`five_hour_resets_at`,
  // `seven_day_pct`/`seven_day_resets_at`) — the reset time drawn
  // through the caller's own `fmtTime` (the viewer's local zone, the
  // shell/task convention), never formatted here.
  function renderRateLimits(checks, fmtTime) {
    const rows = (checks || []).filter(c => c.name === 'rate_limit');
    if (!rows.length) return '';
    const items = rows.map(c => {
      const five = c.five_hour_pct != null
        ? `5h ${Math.round(c.five_hour_pct * 100)}%${c.five_hour_resets_at ? ` (resets ${fmtTime(c.five_hour_resets_at)})` : ''}`
        : '';
      const seven = c.seven_day_pct != null
        ? `7d ${Math.round(c.seven_day_pct * 100)}%${c.seven_day_resets_at ? ` (resets ${fmtTime(c.seven_day_resets_at)})` : ''}`
        : '';
      const cls = STATUS_CLASS[c.status] || 'mute';
      return `<div class="card"><b>${esc(c.provider || 'provider')}</b> <span class="state ${cls}">${[five, seven].filter(Boolean).join(' · ')}</span>
        <div class="mute">${esc(c.detail)}</div></div>`;
    }).join('');
    return `<h2>Rate windows</h2>${items}`;
  }

  // Today's spend, from the `spend` check's structured fields when it
  // carries them (no dollar cap set still shows the spend on its own).
  function renderSpend(checks) {
    const c = (checks || []).find(x => x.name === 'spend');
    if (!c) return '';
    const line = c.spend_usd != null
      ? `${usd(c.spend_usd)}${c.spend_cap_usd != null ? ` of ${usd(c.spend_cap_usd)}` : ''}`
      : esc(c.detail);
    return `<h2>Spend</h2><div class="card">${line}</div>`;
  }

  // The `learning` check's regression/verification lines, one per line
  // rather than the single semicolon-joined string `forge doctor` prints
  // (`src/doctor.rs`'s `check_learning` joins them with `"; "` exactly
  // so a client that wants them separately can split on it) — each line
  // already names its own workflow and provider.
  function renderLearning(checks) {
    const c = (checks || []).find(x => x.name === 'learning');
    if (!c) return '';
    if (c.status !== 'warn') {
      return `<h2>Learning</h2><div class="card mute">${esc(c.detail)}</div>`;
    }
    const lines = c.detail.split('; ').map(l => `<div>${esc(l)}</div>`).join('');
    return `<h2>Learning</h2><div class="card">${lines}</div>`;
  }

  // The whole page: every check as a row, then the sections that carry
  // their own controls or structured readout. `held` is the caller's own
  // pre-filtered `initiative list --json` rows (`state === "held"`);
  // `fmtTime` the caller's own zone-aware formatter.
  function renderDoctorDoc(checks, held, fmtTime) {
    return `
      <h2>Doctor</h2>
      <form class="doctor-refresh"><button type="submit">refresh</button></form>
      ${renderChecks(checks)}
      ${renderHeldInitiatives(held)}
      ${renderWorktrees(checks)}
      ${renderRateLimits(checks, fmtTime)}
      ${renderSpend(checks)}
      ${renderLearning(checks)}`;
  }

  return {
    renderCheckRow, renderChecks, renderHeldInitiatives, renderWorktrees,
    renderRateLimits, renderSpend, renderLearning, renderDoctorDoc,
  };
});

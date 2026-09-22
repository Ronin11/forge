// Pure rendering for the /deploys page (task 534, "deploys"): every
// project's deploy targets — method, host, the last deploy's check,
// smoke and look verdicts — each with its own full deploy log
// (`forge deploy log --json`, embedded server-side by `/api/deploys` so
// no second fetch is needed) and, per deploy, the deploy-look step's
// screenshot shown inline through `GET /api/deploys/shot/<id>`. No DOM,
// no fetch, so web/tests/deploys.test.js can run it under node exactly
// like initiative.js/stats.js, against a fixture built from
// tests/fixtures/deploys.json.
(function (root, factory) {
  const api = factory();
  if (typeof module === 'object' && module.exports) module.exports = api;
  else root.ForgeDeploys = api;
})(globalThis, () => {
  const esc = s => String(s ?? '').replace(/[&<>"]/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));

  // One verdict badge: `ok` is `true`/`false`/`null` — `null` (never ran,
  // or the target declares no smoke url for smoke/look) draws nothing.
  function verdictBadge(label, ok) {
    if (ok === null || ok === undefined) return '';
    return ` <span class="state ${ok ? 'succeeded' : 'failed'}">${esc(label)} ${ok ? 'ok' : 'FAILED'}</span>`;
  }

  // One deploy in a target's log: its commit, when it ran, its three
  // verdicts, rollback information, and — whenever the smoke step ran
  // (`smoke_json` set, regardless of whether it passed) — the deploy-look
  // step's own screenshot, shown inline through the deploy's own id.
  function renderDeployRow(d, fmtTime) {
    const verdicts = verdictBadge('check', d.check_ok) + verdictBadge('smoke', d.smoke_ok) + verdictBadge('look', d.look_ok);
    const rollback = d.rolled_back_to
      ? `<div class="mute">rolled back to ${esc(d.rolled_back_to.slice(0, 8))}</div>` : '';
    const reason = d.reason ? `<div class="mute">${esc(d.reason)}</div>` : '';
    const shot = d.smoke_json
      ? `<img class="deploy-shot" src="/api/deploys/shot/${d.id}" alt="the last look at ${esc((d.sha || '').slice(0, 8))}">`
      : '';
    return `<div class="deploy-row">
      <div><b>${esc((d.sha || '').slice(0, 8))}</b> <span class="mute">${esc(fmtTime(d.started_at))}</span>${verdicts}</div>
      ${rollback}${reason}${shot}
    </div>`;
  }

  function renderDeployLog(deploys, fmtTime) {
    if (!deploys || !deploys.length) return '<div class="mute">no deploys</div>';
    return deploys.map(d => renderDeployRow(d, fmtTime)).join('');
  }

  // One target: method, host, the last deploy's own verdicts as the
  // summary line, a "deploy now" control (a confirmation lives in the
  // caller's submit handler, `app.js`'s `deploysView`, since this module
  // never touches the DOM), and the target's full log underneath.
  function renderTarget(t, fmtTime) {
    const last = (t.deploys || [])[0];
    const when = last ? fmtTime(last.started_at) : 'never deployed';
    const verdicts = last
      ? verdictBadge('check', last.check_ok) + verdictBadge('smoke', last.smoke_ok) + verdictBadge('look', last.look_ok)
      : '';
    return `<details class="card">
      <summary><b>${esc(t.project)} / ${esc(t.name)}</b> <span class="mute">${esc(t.method)}${t.host ? ' · ' + esc(t.host) : ''} · ${esc(when)}</span>${verdicts}</summary>
      <form class="deploy-now" data-project="${esc(t.project)}" data-target="${esc(t.name)}">
        <button type="submit">deploy now</button>
      </form>
      <h4>Log</h4>
      ${renderDeployLog(t.deploys, fmtTime)}
    </details>`;
  }

  function renderDeploys(targets, fmtTime) {
    if (!targets || !targets.length) return '<div class="mute">no deploy targets</div>';
    return targets.map(t => renderTarget(t, fmtTime)).join('');
  }

  return { verdictBadge, renderDeployRow, renderDeployLog, renderTarget, renderDeploys };
});

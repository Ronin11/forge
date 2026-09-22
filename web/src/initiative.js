// Pure rendering for the full initiative page (`/initiatives/<id>`): no
// DOM, no fetch, so web/tests/initiative.test.js can exercise it under
// node exactly like task.js/requests.js — a fixture built from
// `tests/fixtures/initiative.json` (the shape `forge initiative report
// --json` emits, `src/view.rs`'s `InitiativeDoc`) renders the cost-vs-
// budget bar, the refused rules and, when the initiative is held, the
// reason. A timestamp is only ever formatted through the `fmtTime`/
// `fmtSpan` the caller passes in (the shell/task convention), never read
// from `time.js` directly, so this module stays free of the viewer's
// clock.
(function (root, factory) {
  const api = factory();
  if (typeof module === 'object' && module.exports) module.exports = api;
  else root.ForgeInitiative = api;
})(globalThis, () => {
  const esc = s => String(s ?? '').replace(/[&<>"]/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));
  const usd = n => '$' + (Number(n) || 0).toFixed(2);

  // Cost against budget, as a filled bar: `budget_usd == null` means no
  // cap (the project's own default is unbounded, or unset), so no bar —
  // just the spend. Over budget still draws a full bar, marked `over` so
  // the caller can colour it as a warning; the initiative being `held`
  // on `"budget"` is exactly this case.
  function renderCostBar(cost, budget) {
    if (budget == null) {
      return `<div class="cost-bar"><div class="mute">${usd(cost)} · no budget cap</div></div>`;
    }
    const pct = budget > 0 ? Math.min(100, (cost / budget) * 100) : 100;
    const over = cost > budget;
    return `<div class="cost-bar">
      <div class="bar-track"><div class="bar-fill${over ? ' over' : ''}" style="width:${pct.toFixed(1)}%"></div></div>
      <div class="mute">${usd(cost)} of ${usd(budget)}${over ? ' — over budget' : ''}</div>
    </div>`;
  }

  // The state line: state plus, while held, the reason (`held_rule` is
  // `"budget"` or the L0 rule name whose repeated failure tripped the
  // stop rule — docs/CLIENT.md's `InitiativeDoc.held_rule`).
  function renderStateLine(d) {
    const held = d.state === 'held' && d.held_rule
      ? ` <span class="warn">— held: ${esc(d.held_rule)}</span>` : '';
    return `<span class="state ${esc(d.state)}">${esc(d.state)}</span>${held}`;
  }

  // The budget/stop-after control (`POST /api/initiatives/<id>`, `forge
  // initiative set`): a form pre-filled with the current settings, so
  // raising a stuck initiative's cap or its stop-after streak is one
  // submit away.
  function renderControls(d) {
    return `<form class="ini-set" data-id="${d.id}">
      <label>budget <input type="number" step="0.01" min="0.01" class="ini-budget" placeholder="unset" value="${d.budget_usd != null ? d.budget_usd : ''}"></label>
      <label>stop after <input type="number" step="1" min="1" class="ini-stop-after" value="${d.stop_after_same_rule}"></label>
      <button type="submit">set</button>
    </form>`;
  }

  // Every task in the initiative: id, state, cost, retries, score, and a
  // withdraw control for each still `blocked` — the operator's way to
  // clear a blocked piece without waiting on its question.
  function renderTasksTable(tasks) {
    const rows = (tasks || []).map(t => `
      <tr>
        <td><a href="/tasks/${t.id}">${t.id}</a></td>
        <td class="state ${esc(t.state)}">${esc(t.state)}${t.retries ? ` <span class="mute">(${t.retries} ${t.retries === 1 ? 'retry' : 'retries'})</span>` : ''}</td>
        <td class="num">${usd(t.cost_usd)}</td>
        <td>${t.score != null ? `${t.score}/10` : ''}</td>
        <td>${esc(t.reason)}</td>
        <td>${t.state === 'blocked' ? `<form class="req-withdraw" data-id="${t.id}">
          <input type="text" class="req-withdraw-reason" placeholder="reason" required>
          <button type="submit">withdraw</button>
        </form>` : ''}</td>
      </tr>`).join('');
    return `<table><thead><tr><th>id</th><th>state</th><th class="num">cost</th><th>score</th><th>reason</th><th></th></tr></thead>
      <tbody>${rows || '<tr><td colspan="6" class="mute">no tasks</td></tr>'}</tbody></table>`;
  }

  function renderRefused(refused) {
    if (!refused || !refused.length) return '';
    return `<h2>Refused</h2><div class="card">${refused.map(r => `<div>${esc(r.rule)}: ${r.count}</div>`).join('')}</div>`;
  }

  function renderRulings(rulings) {
    if (!rulings || !rulings.length) return '';
    return `<h2>Rulings</h2>${rulings.map(r => `
      <div class="card"><b><a href="/tasks/${r.task_id}">task ${r.task_id}</a></b> ${esc(r.question)}<div class="mute">${esc(r.answer)}</div></div>`).join('')}`;
  }

  function renderQuestions(questions) {
    if (!questions || !questions.length) return '';
    return `<h2>Questions</h2>${questions.map(q => `
      <div class="card"><b><a href="/tasks/${q.task_id}">task ${q.task_id}</a></b> ${esc(q.question)}<div class="mute">${q.answer ? esc(q.answer) : 'unanswered'}</div></div>`).join('')}`;
  }

  // Deploys the initiative's tasks triggered on landing (`InitiativeDoc.
  // deployed`), same status classification as `task.js`'s `renderDeploys`.
  function renderDeploysSection(deployed) {
    if (!deployed || !deployed.length) return '';
    const rows = deployed.map(d => {
      const status = d.check_ok === true ? 'ok'
        : d.check_ok === false ? (d.rolled_back_to ? `rolled back to ${esc(d.rolled_back_to.slice(0, 8))}` : 'failed')
        : 'running';
      const cls = status === 'ok' ? 'succeeded' : status === 'running' ? 'running' : 'failed';
      return `<div><a href="/tasks/${d.task_id}">task ${d.task_id}</a> ${esc(d.target)} ${esc((d.sha || '').slice(0, 8))} <span class="state ${cls}">${esc(status)}</span></div>`;
    }).join('');
    return `<h2>Deploys</h2><div class="card">${rows}</div>`;
  }

  // The whole page: everything `forge initiative report --json` (`d`)
  // carries. `fmtSpan` is the caller's own elapsed-time formatter (the
  // shell/task convention), so this module never touches the viewer's
  // clock.
  function renderInitiativeDoc(d, fmtSpan) {
    return `
      <h2>Initiative ${d.id} <span class="mute">· <a href="/projects/${encodeURIComponent(d.project)}">${esc(d.project)}</a></span></h2>
      <div class="card">
        <div><b>${esc(d.outcome)}</b></div>
        <div>${renderStateLine(d)}${d.elapsed_secs != null ? ` <span class="mute">· ${fmtSpan(d.elapsed_secs)} elapsed</span>` : ''}</div>
        ${renderCostBar(d.cost_usd, d.budget_usd)}
      </div>
      <div class="card actions">${renderControls(d)}</div>
      <h2>Tasks</h2>
      ${renderTasksTable(d.tasks)}
      ${renderRefused(d.refused)}
      ${renderRulings(d.rulings)}
      ${renderQuestions(d.questions)}
      ${renderDeploysSection(d.deployed)}`;
  }

  return {
    renderCostBar, renderStateLine, renderControls, renderTasksTable,
    renderRefused, renderRulings, renderQuestions, renderDeploysSection,
    renderInitiativeDoc,
  };
});

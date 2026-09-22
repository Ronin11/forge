// Pure rendering for the /workflows pages: no DOM, no fetch, so
// web/tests/workflows.test.js can exercise it under node exactly like
// time.js — the list page's rows and the editor's lint-problem list.
(function (root, factory) {
  const api = factory();
  if (typeof module === 'object' && module.exports) module.exports = api;
  else root.ForgeWorkflows = api;
})(globalThis, () => {
  const esc = s => String(s ?? '').replace(/[&<>"]/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));
  const pct = v => v == null ? '-' : (v * 100).toFixed(0) + '%';
  const usd = n => '$' + (Number(n) || 0).toFixed(2);

  // The step chain as a row of small action cards: the resolved steps
  // when present (a catalog build workflow), else the raw declared refs
  // (a repo workflow, or a run workflow — neither resolved in the list).
  function stepChain(w) {
    const steps = (w.resolved && w.resolved.length) ? w.resolved : (w.steps || []);
    if (!steps.length) return '<span class="mute">no steps</span>';
    return steps.map(s => {
      const name = s.action || (s.workflow ? `(${s.workflow})` : '?');
      const title = [s.contract, s.description].filter(Boolean).join(' — ');
      return `<span class="step-card"${title ? ` title="${esc(title)}"` : ''}>${esc(name)}</span>`;
    }).join('<span class="step-arrow">→</span>');
  }

  // The measured profile line: verified rate and interval, cost per
  // verified success, run count, and a regression mark.
  function profileLine(measured) {
    if (!measured || !measured.current) return '<span class="mute">no runs</span>';
    const c = measured.current;
    if (!c.n) return '<span class="mute">no runs</span>';
    if (!c.known) return `<span class="mute">${c.n} run(s), not yet known</span>`;
    const cost = c.cost_per_success != null ? usd(c.cost_per_success) + '/success' : 'no successes';
    return `${pct(c.rate)} <span class="mute">(${pct(c.rate_lo)}–${pct(c.rate_hi)})</span> · ${cost} · ${c.n} run(s)`
      + (measured.regressed ? ' <span class="failed">REGRESSION</span>' : '');
  }

  // One row of the /workflows list.
  function renderWorkflowRow(w) {
    const href = w.project
      ? `/workflows/${encodeURIComponent(w.name)}?project=${encodeURIComponent(w.project)}`
      : `/workflows/${encodeURIComponent(w.name)}`;
    return `<tr class="task" data-name="${esc(w.name)}" data-project="${esc(w.project || '')}">
      <td><a href="${href}">${esc(w.name)}</a></td>
      <td>${esc(w.kind)}</td>
      <td class="mute">${w.project ? esc(w.project) : 'catalog'}</td>
      <td>${stepChain(w)}</td>
      <td>${profileLine(w.measured)}</td>
    </tr>`;
  }

  // Every workflow row, for the list page: a fixture of N workflows
  // renders N `<tr class="task">` rows.
  function renderWorkflowRows(rows) {
    return rows.map(renderWorkflowRow).join('') || '<tr><td colspan="5" class="mute">no workflows</td></tr>';
  }

  // The resolved steps as cards, for the editor page beside the text —
  // the action's description and contract on hover (a native tooltip).
  function renderSteps(steps) {
    if (!steps || !steps.length) return '<span class="mute">no steps</span>';
    return steps.map(s => {
      const title = [s.contract, s.description].filter(Boolean).join(' — ');
      return `<div class="step-card"${title ? ` title="${esc(title)}"` : ''}>
        <b>${esc(s.name)}</b> <span class="mute">${esc(s.kind)}</span>
      </div>`;
    }).join('');
  }

  // Lint problems for the editor: one per line, so a change that
  // introduces two problems renders two lines. A problem with no line
  // (a whole-flow error) still renders, without the "line N" prefix.
  function renderLintProblems(problems) {
    if (!problems || !problems.length) return '<div class="mute">no problems</div>';
    return problems.map(p => `<div class="lint-problem" data-line="${p.line ?? ''}">${p.line ? `<b>line ${p.line}</b> ` : ''}${esc(p.message)}</div>`).join('');
  }

  return { renderWorkflowRows, renderWorkflowRow, renderSteps, renderLintProblems, stepChain, profileLine };
});

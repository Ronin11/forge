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

  // The project and run workflow the prompter starts (docs/WORKFLOWS.md,
  // "Authoring"): this repository's own project, self-registered so its
  // own `.forge/workflows/author-workflow.toml` can run against it.
  const DRAFT_PROJECT = 'forge';
  const DRAFT_WORKFLOW = 'author-workflow';

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

  // The prompter's draft result (`/workflows/new`): the `draft-workflow`
  // step's structured output from a finished job's doc — `/api/job/<id>`,
  // augmented server-side with each step's `output_ref` file parsed onto
  // it as `output` — or `null` when the job has none (still running, or
  // ended without ever reaching that step).
  function draftOutput(job) {
    const step = (job && job.steps || []).find(s => s.action === 'draft-workflow');
    return (step && step.output) || null;
  }

  // The prompter's result panel, above the editor once a draft loads: the
  // rationale and every open question, one per line, so a draft with two
  // open questions renders both. A job that ended `needs_human` shows the
  // human rung's question text instead, and a link to the task it was
  // filed as when one is known; `question` is `{text, task_id}` or `null`
  // (still resolving, or none found).
  function renderDraftPanel(job, question) {
    if (!job) return '';
    if (job.state === 'needs_human') {
      return `<div class="card">
        <b>needs a human</b>
        <div>${esc((question && question.text) || 'no question recorded')}</div>
        ${question && question.task_id ? `<div><a href="/tasks/${question.task_id}">task ${question.task_id}</a></div>` : ''}
      </div>`;
    }
    const d = draftOutput(job);
    if (!d) {
      if (job.state === 'ok') return '<div class="failed">the job ended ok but left no draft</div>';
      if (job.state === 'failed' || job.state === 'dropped') return `<div class="failed">job ${esc(job.state)}</div>`;
      return '';
    }
    const qs = d.open_questions || [];
    return `<div class="card">
      <div><b>rationale</b><div>${esc(d.rationale)}</div></div>
      ${qs.length ? `<div><b>open questions</b><ul>${qs.map(q => `<li>${esc(q)}</li>`).join('')}</ul></div>` : ''}
    </div>`;
  }

  // Whether a live SSE event belongs to the prompter's own in-flight
  // request (`/workflows/new`, promptView in app.js): the events carry no
  // request token (src/report.rs's `JobStarted` has only
  // project/workflow/job_id/dry_run), so the stream alone cannot tell
  // whose job a `job_started` is. `state` is `{pending}` — true only
  // between the POST to `/api/workflows/draft` and that request's own
  // response landing. A matching `job_started` seen while pending gives a
  // provisional "job N running…" to show; everything else — not pending,
  // the wrong project/workflow, a dry run, or any `job_finished` at all —
  // claims nothing and returns `null`. The draft is never loaded from
  // here: only the id the POST response itself returns is authoritative,
  // so the caller loads the draft from that id, not from a stream event.
  function draftStatusFor(state, event) {
    if (!state || !state.pending) return null;
    if (!event || event.type !== 'job_started') return null;
    if (event.project !== DRAFT_PROJECT || event.workflow !== DRAFT_WORKFLOW || event.dry_run) return null;
    return `job ${event.job_id} running…`;
  }

  return {
    DRAFT_PROJECT, DRAFT_WORKFLOW,
    renderWorkflowRows, renderWorkflowRow, renderSteps, renderLintProblems, stepChain, profileLine,
    draftOutput, renderDraftPanel, draftStatusFor,
  };
});

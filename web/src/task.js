// Pure rendering for the full task page (`/tasks/<id>`): no DOM, no
// fetch, so web/tests/task.test.js can exercise it under node exactly
// like requests.js/workflows.js — a fixture with N attempts across M
// steps renders every one of them, a failed L1 check's tail sits behind
// a collapsed <details>, and the action controls (retry/answer/withdraw/
// land) match `src/view.rs`'s `request_kind` classification of a blocked
// task's reason. A timestamp is only ever formatted through the
// `fmtTime` the caller passes in (the shell/workflows convention), never
// read from `time.js` directly, so this module stays free of the
// viewer's clock.
(function (root, factory) {
  const api = factory();
  if (typeof module === 'object' && module.exports) module.exports = api;
  else root.ForgeTask = api;
})(globalThis, () => {
  const esc = s => String(s ?? '').replace(/[&<>"]/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));
  const usd = n => '$' + (Number(n) || 0).toFixed(2);
  const secs = ms => ((ms || 0) / 1000).toFixed(1) + 's';

  // Mirrors `src/view.rs`'s `request_kind`: the one place a blocked
  // task's `reason` is sorted into what it is waiting on. Kept in lock
  // step with that function so the inbox (`web/src/requests.js`, fed
  // from `forge requests --json`) and this page classify the same
  // reason the same way.
  function requestKind(reason) {
    reason = reason || '';
    if (reason.startsWith('waits on task')) return ['dependency', reason];
    const i = reason.indexOf(': ');
    if (i === -1) return ['other', reason];
    const label = reason.slice(0, i), rest = reason.slice(i + 2);
    switch (label) {
      case 'needs workflow': return ['workflow', rest];
      case 'needs suite': return ['suite', rest];
      case 'needs input': return ['question', rest];
      case 'review demoted': return ['review', rest];
      default: return ['other', reason];
    }
  }

  // One row of an attempt's verdict (`checks::CheckResult`): the level
  // and rule/check name always show — an L0 rule's own name is the
  // finding, nothing else to add — and a failed row's tail, when there
  // is one (an L1 check's output), sits behind a collapsed `<details>`
  // rather than always on screen.
  function renderVerdictRow(c) {
    const label = `${c.ok ? '✓' : '✗'} ${esc(c.level)} ${esc(c.name)}`;
    const failing = (c.failing_tests && c.failing_tests.length)
      ? `<div class="mute">failing: ${c.failing_tests.map(esc).join(', ')}</div>` : '';
    const tail = (!c.ok && c.tail)
      ? `<details><summary>tail</summary><pre>${esc(c.tail)}</pre></details>` : '';
    return `<div class="${c.ok ? 'succeeded' : 'failed'}">${label} <span class="mute">${c.ms ? secs(c.ms) : ''}</span>${tail}</div>${failing}`;
  }

  function renderVerdictRows(verdict) {
    return (verdict && verdict.length) ? verdict.map(renderVerdictRow).join('') : '<span class="mute">no verdict</span>';
  }

  // An attempt's tool statistics (`audit::Outputs.tools`, populated by
  // `tools::summarize` from its own log): by-tool call counts and time,
  // and shell command families the same way, behind their own
  // `<details>` since a long attempt's tool list is itself a long tail.
  function renderToolStats(tools) {
    if (!tools) return '';
    const byTool = Object.entries(tools.by_tool || {});
    const shell = Object.entries(tools.shell || {});
    if (!byTool.length && !shell.length) return '';
    const table = (title, rows) => rows.length ? `<table><thead><tr><th>${title}</th><th class="num">calls</th><th class="num">time</th></tr></thead><tbody>${
      rows.map(([name, u]) => `<tr><td>${esc(name)}</td><td class="num">${u.calls}</td><td class="num">${secs(u.ms)}</td></tr>`).join('')
    }</tbody></table>` : '';
    return `<details><summary>tool stats</summary>${table('tool', byTool)}${table('shell', shell)}</details>`;
  }

  // One attempt: its own stats line, the verdict rows (every L0 and L1
  // row the kernel recorded, not only the failed ones), and its tool
  // stats on demand.
  function renderAttempt(a) {
    const outputs = a.outputs || {};
    return `<div class="attempt" data-attempt="${a.attempt_no}">
      <h4>attempt ${a.attempt_no} <span class="state ${esc(a.state)}">${esc(a.state)}</span>
        <span class="mute">${a.num_turns} turns · ${a.tool_calls} tools · ${secs(a.agent_ms)} · ${usd(a.cost_usd)} · ${a.commits} commit(s)${a.dirty ? ' · DIRTY' : ''}${a.timed_out ? ' · TIMED OUT' : ''}</span></h4>
      ${a.reason ? `<div class="mute">${esc(a.reason)}</div>` : ''}
      ${outputs.summary ? `<div>${esc(outputs.summary)}</div>` : ''}
      <div class="rows">${renderVerdictRows(a.verdict)}</div>
      ${renderToolStats(outputs.tools)}
    </div>`;
  }

  // Every attempt, grouped by the step it ran under (`TraceAttempt.step`),
  // in the order each step first appears — a fixture with attempts across
  // N steps renders N `.step` groups, each with every one of its own
  // attempts.
  function renderStepGroups(attempts) {
    const order = [], groups = new Map();
    for (const a of attempts || []) {
      if (!groups.has(a.step)) { groups.set(a.step, []); order.push(a.step); }
      groups.get(a.step).push(a);
    }
    return order.map(step => `<div class="step">
      <h3>${esc(step)}</h3>
      ${groups.get(step).map(renderAttempt).join('')}
    </div>`).join('');
  }

  // The kernel's own ops for this task (clone, push, land, ...): a plain
  // list, each with its output behind a `<details>` when it kept one.
  function renderOps(ops) {
    if (!ops || !ops.length) return '';
    return `<div class="card ops">${ops.map(o => `<div class="${o.ok ? 'succeeded' : 'failed'}">${o.ok ? '✓' : '✗'} ${esc(o.name)}${o.kernel ? '' : ' [user]'} <span class="mute">${secs(o.ms)}${o.detail ? ' · ' + esc(o.detail).slice(0, 160) : ''}</span>${o.output ? `<details><summary>output</summary><pre>${esc(o.output)}</pre></details>` : ''}</div>`).join('')}</div>`;
  }

  // The task's lineage, its own row bold, every other one linked —
  // shared shape with `requests.js`'s `lineageLine`.
  function renderLineage(t) {
    if (!t.lineage || !t.lineage.length) return '';
    return t.lineage.map(l => l.id === t.id
      ? `<b>${l.id} ${esc(l.state)}</b>`
      : `<a href="/tasks/${l.id}">${l.id}</a> ${esc(l.state)}`).join(' → ');
  }

  // The deploys this task's landing triggered (`store::Deploy`, newest
  // first): target, sha, and its outcome — rolled back names what it was
  // rolled back to.
  function renderDeploys(deploys) {
    if (!deploys || !deploys.length) return '';
    return deploys.map(d => {
      const status = d.check_ok === true ? 'ok'
        : d.check_ok === false ? (d.rolled_back_to ? `rolled back to ${esc(d.rolled_back_to.slice(0, 8))}` : 'failed')
        : 'running';
      const cls = status === 'ok' ? 'succeeded' : status === 'running' ? 'running' : 'failed';
      return `<div>${esc(d.target)} ${esc((d.sha || '').slice(0, 8))} <span class="state ${cls}">${esc(status)}</span>${d.reason ? ` — ${esc(d.reason)}` : ''}</div>`;
    }).join('');
  }

  // The assess directive's most recent run against this task's own
  // landing (`TraceAssessment`); `null`/`undefined` when it never ran.
  function renderAssessment(assessment) {
    if (!assessment) return '';
    const findings = (assessment.findings || [])
      .map(f => `<div class="mute">${esc(f.severity)} ${esc(f.path)}: ${esc(f.finding)}</div>`).join('');
    return `<div><span class="k">assessment</span>${assessment.score}/10 (${(assessment.findings || []).length} finding(s))</div>${findings}`;
  }

  function renderDiagnosis(diagnosis) {
    if (!diagnosis || !diagnosis.length) return '';
    return `<h2>Diagnosis</h2>${diagnosis.map(d => `<div class="card"><span class="k">what</span>${esc(d.what)}<br><span class="k">action</span>${esc(d.action)}</div>`).join('')}`;
  }

  // The branch and, once it is known, the compare link. `compare` is
  // never part of `forge trace --json` (only the live `task_done` event
  // carries it — docs/CLIENT.md), so the caller passes it in from the
  // event stream when this task finished during the current session;
  // otherwise the branch alone still shows.
  function renderBranch(t, compare) {
    return `<div><span class="k">branch</span>${esc(t.branch)} <span class="mute">from ${esc(t.base_branch)} @ ${esc((t.base_sha || '').slice(0, 8))}</span>${compare ? ` · <a href="${esc(compare)}" target="_blank" rel="noopener">compare →</a>` : ''}</div>`;
  }

  // The economist's task-shape inputs (`TraceTaskShape`, docs/ECONOMIST.md
  // "Task shape"): what was known about the task before it ever ran.
  function renderInputsShape(inputs) {
    if (!inputs) return '';
    return `<div class="mute">text ${inputs.text_len} chars · path tokens ${inputs.path_tokens} · tdd ${inputs.tdd ? 'yes' : 'no'} · declared checks ${inputs.declared_checks}</div>`;
  }

  // The operator actions from the inbox (task 530), shown only where
  // they apply to this task's own state: retry for anything finished
  // (not queued or running — `forge retry` refuses those two), land for
  // an unverified task, and for a blocked one, withdraw always plus an
  // inline answer box when it is blocked on a question specifically.
  // Same `.req-answer`/`.req-withdraw`/`.req-land` classes as the inbox
  // so the page can wire the same handlers.
  function renderActions(t) {
    const retry = (t.state !== 'queued' && t.state !== 'running')
      ? `<button class="task-retry" data-id="${t.id}">retry</button>` : '';
    const land = t.state === 'unverified'
      ? `<button class="req-land" data-id="${t.id}">land</button>` : '';
    let answer = '', withdraw = '';
    if (t.state === 'blocked') {
      const [kind] = requestKind(t.reason);
      withdraw = `<form class="req-withdraw" data-id="${t.id}">
        <input type="text" class="req-withdraw-reason" placeholder="reason" required>
        <button type="submit">withdraw</button>
      </form>`;
      if (kind === 'question') {
        answer = `<form class="req-answer" data-id="${t.id}">
          <input type="text" class="req-answer-text" placeholder="Your answer" required>
          <button type="submit">answer</button>
        </form>`;
      }
    }
    return (retry || land || answer || withdraw) ? `<div class="card actions">${retry} ${land}${answer}${withdraw}</div>` : '';
  }

  // The whole page: everything `forge trace --json` (`doc`) carries.
  // `fmtTime` is the caller's own (the shell/workflows convention, so
  // this module never touches the viewer's clock); `extra.compare` is
  // the live compare link, when known.
  function renderTaskDetail(doc, fmtTime, extra) {
    extra = extra || {};
    const t = doc.task || {};
    const refs = (t.refs || []).map(r => `<a href="${esc(r.url)}" target="_blank" rel="noopener">${esc(r.kind)}${r.label ? ': ' + esc(r.label) : ''}</a>`).join(' · ');
    const lineage = renderLineage(t);
    return `
      <h2>Task ${t.id} <span class="state ${esc(t.state)}">${esc(t.state)}</span></h2>
      <div class="card">
        <div><span class="k">repo</span>${esc(t.repo)} <a href="/graph?repo=${encodeURIComponent(t.repo)}">graph</a></div>
        ${renderBranch(t, extra.compare)}
        <div><span class="k">workflow</span>${esc(t.workflow)} <span class="mute">${esc((t.workflow_hash || '').slice(0, 8))}</span> · ${esc(t.model)} · ${t.max_turns} turns · ${t.max_attempts} attempts · timeout ${t.timeout_secs}s</div>
        ${t.checks && t.checks.length ? `<div><span class="k">checks</span>${t.checks.map(esc).join(', ')}</div>` : ''}
        ${(t.project || t.initiative != null) ? `<div><span class="k">project</span>${t.project ? `<a href="/projects/${encodeURIComponent(t.project)}">${esc(t.project)}</a>` : '-'}${t.initiative != null ? ` · <a href="/initiatives/${t.initiative}">initiative ${t.initiative}</a>` : ''}</div>` : ''}
        <div><span class="k">created</span>${fmtTime(t.created_at)}${t.started_at ? ` · started ${fmtTime(t.started_at)}` : ''}${t.finished_at ? ` · finished ${fmtTime(t.finished_at)}` : ''}</div>
        ${lineage ? `<div><span class="k">lineage</span>${lineage}</div>` : ''}
        ${refs ? `<div><span class="k">refs</span>${refs}</div>` : ''}
        ${t.reason ? `<div><span class="k">reason</span>${esc(t.reason)}</div>` : ''}
        ${t.budget_usd != null ? `<div><span class="k">budget</span>${usd(t.budget_usd)}</div>` : ''}
        ${renderInputsShape(t.inputs)}
        ${renderAssessment(doc.assessment)}
        ${doc.deploys && doc.deploys.length ? `<div><span class="k">deploys</span>${renderDeploys(doc.deploys)}</div>` : ''}
      </div>
      ${renderActions(t)}
      <div class="card"><pre style="margin:0">${esc(t.text)}</pre></div>
      ${t.plan ? `<h2>Plan</h2><div class="card"><pre style="margin:0">${esc(t.plan)}</pre></div>` : ''}
      ${renderDiagnosis(doc.diagnosis)}
      <h2>Steps &amp; attempts</h2>
      <div class="pipeline">${renderStepGroups(doc.attempts)}</div>
      ${renderOps(doc.ops)}
      ${t.journal ? `<h2>Journal</h2><div class="card"><pre style="margin:0">${esc(t.journal)}</pre></div>` : ''}`;
  }

  return {
    requestKind, renderVerdictRows, renderVerdictRow, renderToolStats, renderAttempt,
    renderStepGroups, renderOps, renderLineage, renderDeploys, renderAssessment,
    renderDiagnosis, renderBranch, renderInputsShape, renderActions, renderTaskDetail,
  };
});

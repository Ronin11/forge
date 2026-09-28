// Pure model and rendering for the workflow draft editor (/workflows/draft):
// no DOM, no fetch, so web/tests/drafts.test.js can exercise it under node
// exactly like workflows.js. The draft is data — a step list with failure
// edges and placeholders for actions that do not exist yet — edited by the
// functions below and rendered as a list plus a simple SVG of the edges.
// Linting is never done here: every change goes to the server's `check`,
// which runs the catalog's own linter, and the answer is what renders.
(function (root, factory) {
  const api = factory();
  if (typeof module === 'object' && module.exports) module.exports = api;
  else root.ForgeDrafts = api;
})(globalThis, () => {
  const esc = s => String(s ?? '').replace(/[&<>"]/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));
  const clone = d => JSON.parse(JSON.stringify(d));

  function emptyDraft(name, kind) {
    return {
      name, kind: kind || 'build', description: '', project: null, steps: [],
      settings: kind === 'run' ? { trigger: { on: 'manual' } } : {}, status: 'draft', tasks: [],
    };
  }

  // The document the server sends back annotated carries more than a
  // draft; what goes back up is only the draft's own fields.
  function bare(a) {
    const { name, kind, description, project, steps, settings, status, tasks, proposal } = a;
    return { name, kind, description, project: project || null, steps, settings, status, tasks, proposal: proposal || [] };
  }

  const at = (d, i) => Math.max(0, Math.min(i == null ? d.steps.length : i, d.steps.length));

  function addStep(d, action, index) {
    const n = clone(d); n.steps.splice(at(n, index), 0, { action }); return n;
  }
  // An action that does not exist yet: the one-line contract the operator writes.
  function addPlaceholder(d, action, ph, index) {
    const n = clone(d);
    n.steps.splice(at(n, index), 0, {
      action, placeholder: { kind: ph.kind || 'operation', inputs: ph.inputs || '', outputs: ph.outputs || '' },
    });
    return n;
  }
  function removeStep(d, i) { const n = clone(d); n.steps.splice(i, 1); return n; }
  function moveStep(d, i, delta) {
    const j = i + delta;
    if (j < 0 || j >= d.steps.length) return d;
    const n = clone(d); const [s] = n.steps.splice(i, 1); n.steps.splice(j, 0, s); return n;
  }
  // role, judgment, effect, action; an empty value clears the field.
  function setField(d, i, key, value) {
    const n = clone(d);
    if (key === 'action') n.steps[i].action = value;
    else if (value === '' || value == null) delete n.steps[i][key];
    else n.steps[i][key] = value;
    return n;
  }
  function setPlaceholderField(d, i, key, value) {
    const n = clone(d);
    const s = n.steps[i];
    s.placeholder = s.placeholder || { kind: 'operation', inputs: '', outputs: '' };
    s.placeholder[key] = value;
    return n;
  }
  function setEdge(d, i, key, to) {
    const n = clone(d);
    n.steps[i].on = Object.assign({}, n.steps[i].on, { [key]: to });
    return n;
  }
  function removeEdge(d, i, key) {
    const n = clone(d);
    if (n.steps[i].on) { delete n.steps[i].on[key]; if (!Object.keys(n.steps[i].on).length) delete n.steps[i].on; }
    return n;
  }
  // Switching kind carries the sections that kind needs: a run workflow a
  // trigger, a build workflow none of the run-only sections.
  function setKind(d, kind) {
    const n = clone(d); n.kind = kind;
    if (kind === 'run') n.settings.trigger = n.settings.trigger || { on: 'manual' };
    else for (const k of ['trigger', 'limits', 'assert', 'skip_if', 'env']) delete n.settings[k];
    return n;
  }
  // The suggestion's steps the operator picked (all when `picks` is empty)
  // join the step list; the proposal is then spent.
  function acceptProposal(d, picks) {
    const n = clone(d);
    const p = n.proposal || [];
    const chosen = picks && picks.length ? picks.map(i => p[i]).filter(Boolean) : p;
    n.steps.push(...chosen); n.proposal = [];
    return n;
  }

  const contractLine = p => `inputs: ${p.inputs}; outputs: ${p.outputs}; kind: ${p.kind}`;

  // Every edge as {from, to, key}: `to` an index, or 'end'. A target names
  // a step, a node id (`<index>-<action>`), or `end`; one that resolves to
  // nothing (or to several steps by name) is left for the linter to report.
  function edges(steps) {
    const out = [];
    steps.forEach((s, from) => {
      for (const [key, target] of Object.entries(s.on || {})) {
        let to = null;
        if (target === 'end') to = 'end';
        else {
          const byNode = steps.findIndex((t, i) => `${i}-${t.action}` === target);
          const byName = steps.map((t, i) => t.action === target ? i : -1).filter(i => i >= 0);
          if (byNode >= 0) to = byNode; else if (byName.length === 1) to = byName[0];
        }
        if (to !== null) out.push({ from, to, key });
      }
    });
    return out;
  }

  // The graph as a simple SVG: the steps top to bottom joined by the
  // forward arrows, each failure or outcome edge a dashed loop on the
  // right to its target, a final `end` node when an edge names it.
  function renderGraph(steps) {
    const W = 200, H = 30, GAP = 22, X = 16;
    const es = edges(steps);
    const hasEnd = es.some(e => e.to === 'end');
    const nodes = steps.length + (hasEnd ? 1 : 0);
    const y = i => 10 + i * (H + GAP);
    const height = nodes ? y(nodes - 1) + H + 10 : 20;
    const width = X + W + 40 + es.length * 14;
    const parts = [`<defs><marker id="arr" markerWidth="8" markerHeight="8" refX="6" refY="3" orient="auto"><path d="M0,0 L6,3 L0,6 z"/></marker></defs>`];
    steps.forEach((s, i) => {
      const cls = s.placeholder ? 'graph-node placeholder' : 'graph-node';
      parts.push(`<g class="${cls}" data-step="${i}"><rect x="${X}" y="${y(i)}" width="${W}" height="${H}" rx="4"/><text x="${X + 8}" y="${y(i) + 19}">${i + 1}. ${esc(s.action)}</text></g>`);
      if (i > 0) parts.push(`<line class="graph-next" x1="${X + W / 2}" y1="${y(i - 1) + H}" x2="${X + W / 2}" y2="${y(i)}" marker-end="url(#arr)"/>`);
    });
    if (hasEnd) parts.push(`<g class="graph-node end"><rect x="${X}" y="${y(steps.length)}" width="${W}" height="${H}" rx="14"/><text x="${X + 8}" y="${y(steps.length) + 19}">end</text></g>`);
    es.forEach((e, n) => {
      const x = X + W + 10 + n * 14;
      const y1 = y(e.from) + H / 2, y2 = y(e.to === 'end' ? steps.length : e.to) + H / 2;
      parts.push(`<path class="graph-edge" data-from="${e.from}" data-to="${e.to}" d="M${X + W},${y1} H${x} V${y2} H${X + W}" marker-end="url(#arr)"><title>${esc(e.key)}</title></path>`);
    });
    return `<svg class="draft-graph" width="${width}" height="${height}" viewBox="0 0 ${width} ${height}" xmlns="http://www.w3.org/2000/svg">${parts.join('')}</svg>`;
  }

  function renderProblems(problems) {
    if (!problems || !problems.length) return '<div class="succeeded">lint: clean</div>';
    return `<div class="failed">lint: ${problems.length} problem(s)</div>` + problems.map(p =>
      `<div class="lint-problem" data-step="${p.step ?? ''}">${p.step != null ? `<b>step ${p.step + 1}</b> ` : (p.line ? `<b>line ${p.line}</b> ` : '')}${esc(p.message)}</div>`).join('');
  }

  const input = (i, f, v, ph) => `<input data-i="${i}" data-f="${f}" value="${esc(v || '')}" placeholder="${esc(ph)}">`;

  // One step of the list: the action and its contract, its role,
  // judgment and effect (a run workflow's), its failure edges, and, for a
  // placeholder, the one-line contract being written.
  function renderStep(a, s, i) {
    const info = (a.info || [])[i] || {};
    const run = a.kind === 'run';
    const probs = (a.problems || []).filter(p => p.step === i);
    const contract = s.placeholder
      ? (info.landed ? '<span class="succeeded">landed</span>' : `<span class="mute">placeholder — ${esc(contractLine(s.placeholder))}</span>`)
      : (info.kind ? `<span class="mute">${esc(info.kind)}${info.contract ? ' · ' + esc(info.contract) : ''} — ${esc(info.description)}</span>` : '<span class="failed">unknown action</span>');
    const ph = s.placeholder ? `<div class="ph">
        <select data-i="${i}" data-f="ph-kind">${['operation', 'directive'].map(k => `<option${s.placeholder.kind === k ? ' selected' : ''}>${k}</option>`).join('')}</select>
        ${input(i, 'ph-inputs', s.placeholder.inputs, 'inputs')} ${input(i, 'ph-outputs', s.placeholder.outputs, 'outputs')}</div>` : '';
    const fields = run ? `<div>${input(i, 'role', s.role, 'role')} ${input(i, 'judgment', s.judgment, 'judgment (a directive)')} ${input(i, 'effect', s.effect, 'effect (an operation)')}</div>` : '';
    const on = Object.entries(s.on || {}).map(([k, to]) =>
      `<span class="edge-chip">on ${esc(k)} → ${esc(to)} <button data-act="unedge" data-i="${i}" data-key="${esc(k)}">×</button></span>`).join(' ');
    const edgeForm = run ? `<div>${on} <input data-i="${i}" data-f="edge-key" placeholder="failure" size="10"> → <input data-i="${i}" data-f="edge-to" placeholder="step or end" size="12"> <button data-act="edge" data-i="${i}">add edge</button></div>` : '';
    return `<div class="step-card${probs.length ? ' has-problem' : ''}" data-step="${i}">
      <div><b>${i + 1}. ${esc(s.action)}</b> ${contract}
        <button data-act="up" data-i="${i}">↑</button><button data-act="down" data-i="${i}">↓</button><button data-act="rm" data-i="${i}">remove</button></div>
      ${ph}${fields}${edgeForm}
      ${probs.map(p => `<div class="lint-problem">${esc(p.message)}</div>`).join('')}
    </div>`;
  }

  function renderStepList(a) {
    if (!a.steps || !a.steps.length) return '<div class="mute">no steps yet — pick an action below</div>';
    return a.steps.map((s, i) => renderStep(a, s, i)).join('');
  }

  // The placeholders the draft still waits on, and the build task filed
  // for each; the draft's status says whether it will enable itself.
  function renderPending(a) {
    const tasks = Object.fromEntries((a.tasks || []).map(t => [t.action, t.task_id]));
    const rows = (a.pending || []).map(n =>
      `<li>${esc(n)} ${tasks[n] ? `<a href="/tasks/${tasks[n]}">task ${tasks[n]}</a>` : '<span class="mute">no task filed yet — put files it</span>'}</li>`);
    const status = `<div>status <b class="draft-status ${esc(a.status)}">${esc(a.status)}</b></div>`;
    return status + (rows.length ? `<div>waiting on <ul>${rows.join('')}</ul></div>` : '');
  }

  // The one-shot suggestion: proposed steps, for the operator to edit.
  function renderProposal(d) {
    const p = d.proposal || [];
    if (!p.length) return '';
    return `<div class="card"><b>suggested steps</b> <button data-act="accept-all">add all</button>` +
      p.map((s, i) => `<div data-proposal="${i}">${i + 1}. ${esc(s.action)} <button data-act="accept" data-i="${i}">add</button></div>`).join('') + '</div>';
  }

  // The catalog picker: each action's kind and contract, the description
  // on hover; `selected` shows the chosen one's contract in full.
  function renderActionOptions(actions, selected) {
    return (actions || []).map(a =>
      `<option value="${esc(a.name)}" title="${esc(a.description)}"${a.name === selected ? ' selected' : ''}>${esc(a.name)} (${esc(a.kind)}${a.contract ? ', ' + esc(a.contract) : ''})</option>`).join('');
  }
  function renderActionContract(actions, name) {
    const a = (actions || []).find(x => x.name === name);
    if (!a) return '<span class="mute">not in the catalog</span>';
    const io = `consumes ${(a.consumes || []).join(', ') || '-'} · produces ${(a.produces || []).join(', ') || '-'}` +
      ((a.outcomes || []).length ? ` · outcomes ${a.outcomes.join(', ')}` : '');
    return `<b>${esc(a.name)}</b> ${esc(a.kind)}${a.contract ? ' · ' + esc(a.contract) : ''} <span class="mute">${esc(io)}</span><div>${esc(a.description)}</div>`;
  }

  return {
    emptyDraft, bare, addStep, addPlaceholder, removeStep, moveStep, setField, setPlaceholderField,
    setEdge, removeEdge, setKind, acceptProposal, contractLine, edges,
    renderGraph, renderProblems, renderStepList, renderPending, renderProposal,
    renderActionOptions, renderActionContract,
  };
});

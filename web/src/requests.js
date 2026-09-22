// Pure rendering for the /requests inbox: no DOM, no fetch, so
// web/tests/requests.test.js can exercise it under node exactly like
// workflows.js — a fixture with two questions renders two inline answer
// boxes, one per question, while every row (any kind) gets the shared
// withdraw control.
(function (root, factory) {
  const api = factory();
  if (typeof module === 'object' && module.exports) module.exports = api;
  else root.ForgeRequests = api;
})(globalThis, () => {
  const esc = s => String(s ?? '').replace(/[&<>"]/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));
  const usd = n => '$' + (Number(n) || 0).toFixed(2);

  // The task's lineage, when the caller has fetched `forge trace ID
  // --json` and attached it as `lineage` (`TraceDoc.task.lineage`): every
  // task in the piece of work, the row's own task bold, the rest linked.
  function lineageLine(id, lineage) {
    if (!lineage || !lineage.length) return '';
    return lineage.map(l => l.id === id
      ? `<b>${l.id} ${esc(l.state)}</b>`
      : `<a href="/tasks/${l.id}">${l.id}</a> ${esc(l.state)}`).join(' → ');
  }

  // One row of the inbox: a blocked task and what it is waiting on
  // (`RequestRow`, `forge requests --json`). Every row gets the shared
  // withdraw control (a stale or superseded request is not only a
  // question); a `question` row also gets an inline answer box.
  // `lineage` and `last_summary`, when the caller has attached them from
  // `/api/task/<id>`, show under the question text.
  function renderRequestRow(r) {
    const lineage = lineageLine(r.id, r.lineage);
    const answerBox = r.kind === 'question' ? `
      <form class="req-answer" data-id="${r.id}">
        <input type="text" class="req-answer-text" placeholder="Your answer" required>
        <button type="submit">answer</button>
      </form>` : '';
    return `<div class="card req" data-id="${r.id}" data-kind="${esc(r.kind)}">
      <b><a href="/tasks/${r.id}">${r.id}</a></b> <span class="mute">${esc(r.kind)}${r.to ? ' · to ' + esc(r.to) : ''}${r.path ? ' · ' + esc(r.path) : ''}</span>
      <div>${esc(r.text)}</div>
      ${lineage ? `<div class="mute">lineage: ${lineage}</div>` : ''}
      ${r.last_summary ? `<div class="mute">last attempt: ${esc(r.last_summary)}</div>` : ''}
      ${r.tried ? `<details><summary>tried</summary><div class="mute">${esc(r.tried)}</div></details>` : ''}
      ${answerBox}
      <form class="req-withdraw" data-id="${r.id}">
        <input type="text" class="req-withdraw-reason" placeholder="reason" required>
        <button type="submit">withdraw</button>
      </form>
    </div>`;
  }

  // Every blocked task, for the inbox's main list: a fixture with N
  // question rows renders N `.req-answer` boxes, one per question, plus
  // one `.req-withdraw` control per row regardless of kind.
  function renderRequestRows(rows) {
    return rows.length ? rows.map(renderRequestRow).join('') : '<div class="card mute">No open questions.</div>';
  }

  // One unverified task: it passed but never landed (`TaskRow.state`,
  // docs/CLIENT.md), so the only thing left waiting on a person is the
  // land decision itself.
  function renderUnverifiedRow(t) {
    return `<div class="card req" data-id="${t.id}">
      <b><a href="/tasks/${t.id}">${t.id}</a></b> <span class="mute">unverified · ${esc(t.workflow)} · ${usd(t.cost_usd)}</span>
      <div>${esc(t.task)}</div>
      <button class="req-land" data-id="${t.id}">land</button>
    </div>`;
  }

  // Every unverified task, for the inbox's second section; empty (not a
  // placeholder card) when none are waiting, so the section can hide
  // itself entirely.
  function renderUnverifiedRows(rows) {
    return rows.length ? rows.map(renderUnverifiedRow).join('') : '';
  }

  return { renderRequestRows, renderRequestRow, renderUnverifiedRows, renderUnverifiedRow };
});

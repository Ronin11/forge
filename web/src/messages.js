// Pure rendering for the /messages page (web UI task 10, "messages"): per
// project the message record (`forge message list PROJECT --json`), the
// concierge's own decisions on inbound messages (`forge decisions --json
// --project PROJECT`, `answered_by === "concierge"`), the questions
// addressed to contacts and their state (an open blocked task still
// waiting, or an answered one's outcome), and the jobs a message
// triggered. No DOM, no fetch, so web/tests/messages.test.js can run it
// under node exactly like activity.js/deploys.js, against a fixture built
// from tests/fixtures/messages.json.
(function (root, factory) {
  const api = factory();
  if (typeof module === 'object' && module.exports) module.exports = api;
  else root.ForgeMessages = api;
})(globalThis, () => {
  const esc = s => String(s ?? '').replace(/[&<>"]/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));

  // Every distinct contact a project's messages name, in first-seen
  // order — the contact filter's own option list.
  function contactsOf(messages) {
    const seen = [];
    for (const m of messages || []) {
      if (m.contact && !seen.includes(m.contact)) seen.push(m.contact);
    }
    return seen;
  }

  // `filters.contact` narrows to one contact (exact match); `filters.q`
  // is a case-insensitive substring over the message text. Never
  // re-sorts: the caller's own order (`forge message list`'s newest
  // first) is preserved.
  function filterMessages(messages, filters) {
    const contact = filters && filters.contact;
    const q = filters && filters.q ? String(filters.q).toLowerCase() : '';
    return (messages || []).filter(m =>
      (!contact || m.contact === contact)
      && (!q || (m.text || '').toLowerCase().includes(q)));
  }

  function renderMessageRow(m, fmtTime) {
    const dir = m.direction === 'in' ? 'from' : 'to';
    const task = m.task_id != null ? ` · <a href="/tasks/${m.task_id}">task ${m.task_id}</a>` : '';
    return `<div class="card msg" data-id="${m.id}" data-direction="${esc(m.direction)}">
      <span class="mute">${esc(fmtTime(m.at))}</span>
      <b>${esc(dir)} ${esc(m.contact)}</b> <span class="mute">${esc(m.channel)}${task}</span>
      <div>${esc(m.text)}</div>
    </div>`;
  }

  // Every message, in the order given — a fixture of N messages renders
  // N rows in that same order, newest first, not resorted.
  function renderMessageRows(messages, fmtTime) {
    return messages.length
      ? messages.map(m => renderMessageRow(m, fmtTime)).join('')
      : '<div class="card mute">No messages.</div>';
  }

  // The concierge's own decisions on inbound messages: `DecisionRow`
  // where `answered_by === "concierge"` (`src/concierge.rs`'s "question"
  // branch) — it answered the contact itself, with no task ever
  // blocking.
  function conciergeDecisions(decisions) {
    return (decisions || []).filter(d => d.answered_by === 'concierge');
  }

  function renderDecisionRow(d, fmtTime) {
    return `<div class="card" data-id="${d.id}">
      <span class="mute">${esc(fmtTime(d.created_at))}</span>
      <div><b>Q:</b> ${esc(d.question)}</div>
      <div><b>A:</b> ${esc(d.answer)}</div>
    </div>`;
  }

  function renderDecisionRows(decisions, fmtTime) {
    return decisions.length
      ? decisions.map(d => renderDecisionRow(d, fmtTime)).join('')
      : '<div class="card mute">No concierge decisions.</div>';
  }

  // Every question addressed to a contact and where it stands: open ones
  // (`RequestRow`, `kind === "question"`, `to` set — a blocked task
  // still waiting, so `state` is `"blocked"`) plus answered ones
  // (`DecisionRow.answered_for` set — `state` is the outcome of the task
  // the answer re-queued, or `"answered"` while that outcome is not yet
  // known).
  function mergeQuestions(questions, decisions) {
    const open = (questions || []).map(r => ({
      id: r.id, to: r.to, text: r.text, state: 'blocked', answer: null,
    }));
    const answered = (decisions || [])
      .filter(d => d.answered_for)
      .map(d => ({
        id: d.task_id, to: d.answered_for, text: d.question,
        state: d.outcome || 'answered', answer: d.answer,
      }));
    return open.concat(answered);
  }

  function renderQuestionRow(q) {
    const task = q.id != null ? `<a href="/tasks/${q.id}">${q.id}</a>` : '—';
    const answer = q.answer ? `<div class="mute">A: ${esc(q.answer)}</div>` : '';
    return `<div class="card" data-id="${q.id ?? ''}">
      <b>${task}</b> <span class="mute">to ${esc(q.to)}</span> <span class="state ${esc(q.state)}">${esc(q.state)}</span>
      <div>${esc(q.text)}</div>
      ${answer}
    </div>`;
  }

  function renderQuestionRows(rows) {
    return rows.length
      ? rows.map(renderQuestionRow).join('')
      : '<div class="card mute">No questions to contacts.</div>';
  }

  // Jobs a message triggered (`JobRow.trigger_kind === "message"`,
  // `trigger_ref` the message's own id, `src/worker.rs`'s
  // `message_triggers`).
  function renderJobRow(j, fmtTime) {
    const when = j.started_at ? fmtTime(j.started_at) : (j.due_at ? `due ${fmtTime(j.due_at)}` : 'queued');
    return `<div class="card" data-id="${j.id}">
      <b><a href="/jobs/${j.id}">${j.id}</a></b> <span class="mute">${esc(j.workflow)} · message ${esc(j.trigger_ref)} · ${esc(when)}</span>
      <span class="state ${esc(j.state)}">${esc(j.state)}</span>
    </div>`;
  }

  function renderJobRows(jobs, fmtTime) {
    return jobs.length
      ? jobs.map(j => renderJobRow(j, fmtTime)).join('')
      : '<div class="card mute">No jobs triggered by messages.</div>';
  }

  // The whole page from one `/api/messages/<project>` doc
  // (`{messages, decisions, questions, jobs}`) and the filter state
  // (`{contact, q}`).
  function renderMessagesDoc(doc, filters, fmtTime) {
    const messages = filterMessages(doc.messages, filters);
    const questions = mergeQuestions(doc.questions, doc.decisions);
    return `
      <h3>Messages</h3>
      ${renderMessageRows(messages, fmtTime)}
      <h3>Concierge decisions</h3>
      ${renderDecisionRows(conciergeDecisions(doc.decisions), fmtTime)}
      <h3>Questions to contacts</h3>
      ${renderQuestionRows(questions)}
      <h3>Jobs triggered</h3>
      ${renderJobRows(doc.jobs, fmtTime)}`;
  }

  return {
    contactsOf, filterMessages, renderMessageRow, renderMessageRows,
    conciergeDecisions, renderDecisionRow, renderDecisionRows,
    mergeQuestions, renderQuestionRow, renderQuestionRows,
    renderJobRow, renderJobRows,
    renderMessagesDoc,
  };
});

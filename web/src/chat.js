// Pure model and rendering for the Chat page (/chat, /chat/:id): no DOM,
// no fetch, so web/tests/chat.test.js can exercise it under node exactly
// like drafts.js. A session is a list of turns; a reply may carry the
// tool calls the model made and, for the three write verbs, proposed
// actions the operator confirms or rejects (docs/CHAT.md). The reply of
// a turn in flight arrives as server-sent events from `forge chat
// --stream`: `parseFrames` cuts them out of the byte stream, `apply`
// folds each into the live state, `renderLive` draws that state.
// Everything the page shows is escaped here; nothing is trusted markup.
(function (root, factory) {
  const api = factory();
  if (typeof module === 'object' && module.exports) module.exports = api;
  else root.ForgeChat = api;
})(globalThis, () => {
  const esc = s => String(s ?? '').replace(/[&<>"]/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));
  const usd = n => '$' + (Number(n) || 0).toFixed(4);
  const short = (v, n) => { const s = typeof v === 'string' ? v : JSON.stringify(v); return s.length > n ? s.slice(0, n) + '…' : s; };

  // `data: {...}` frames out of a chunk of an event stream. `carry` is what
  // the last call left unfinished; returns the parsed events and the new
  // carry. A frame that is not JSON is skipped.
  function parseFrames(chunk, carry) {
    const text = (carry || '') + chunk;
    const parts = text.split('\n\n');
    const rest = parts.pop();
    const events = [];
    for (const frame of parts) {
      for (const line of frame.split('\n')) {
        if (!line.startsWith('data:')) continue;
        try { events.push(JSON.parse(line.slice(5).trim())); } catch { /* skip */ }
      }
    }
    return { events, carry: rest };
  }

  const emptyLive = message => ({ message, session: null, tools: [], reply: null, proposals: [], cost: null, error: null, done: false });

  // Fold one event of a turn in flight into the live state.
  function apply(live, ev) {
    const s = { ...live, tools: live.tools.slice() };
    switch (ev.type) {
      case 'session': s.session = ev.session; break;
      case 'tool': s.tools.push(ev); break;
      case 'reply':
        s.reply = ev.text; s.proposals = ev.proposals || []; s.cost = ev.cost_usd; s.done = true;
        if (ev.session != null) s.session = ev.session;
        break;
      case 'error': s.error = ev.message; s.done = true; break;
      default: break;
    }
    return s;
  }

  function renderCall(c) {
    const args = esc(short(c.arguments, 200));
    const head = `<code>${esc(c.tool)}</code> <span class="mute">${args}</span>`;
    if (c.error) return `<li>${head} <span class="failed">${esc(c.error)}</span></li>`;
    if (c.proposal) return `<li>${head} <span class="mute">proposed</span></li>`;
    const body = c.result === undefined || c.result === null ? '' : `<pre class="chat-result">${esc(short(c.result, 4000))}</pre>`;
    return `<li><details><summary>${head}</summary>${body}</details></li>`;
  }

  function renderCalls(calls) {
    const shown = (calls || []).filter(c => c.error || c.result != null || c.proposal);
    if (!shown.length) return '';
    return `<ul class="chat-calls">${shown.map(renderCall).join('')}</ul>`;
  }

  // A proposed action: its summary, and the two buttons while it waits.
  function renderProposal(c) {
    const p = c.proposal;
    const id = esc(c.action);
    if (p.status === 'proposed') {
      return `<div class="chat-proposal" data-action="${id}"><div><b>Proposed</b> ${esc(p.summary)}</div>` +
        `<button class="chat-confirm" data-action="${id}" data-verb="confirm">confirm</button> ` +
        `<button class="chat-reject" data-action="${id}" data-verb="reject">reject</button></div>`;
    }
    const cls = p.status === 'failed' ? 'failed' : p.status === 'confirmed' ? 'succeeded' : 'mute';
    return `<div class="chat-proposal decided" data-action="${id}"><div>${esc(p.summary)}</div>` +
      `<div class="${cls}">${esc(p.status)}${p.outcome ? ': ' + esc(p.outcome) : ''}</div></div>`;
  }

  function renderTurn(t, fmtTime) {
    const when = t.at ? `<span class="mute">${esc(fmtTime ? fmtTime(t.at) : t.at)}</span>` : '';
    if (t.role === 'user') {
      return `<div class="chat-turn chat-user" data-turn="${esc(t.id)}"><div class="chat-who">you ${when}</div><div class="chat-text">${esc(t.text)}</div></div>`;
    }
    if (t.role === 'action') {
      return `<div class="chat-turn chat-action" data-turn="${esc(t.id)}"><div class="chat-text mute">${esc(t.text)}</div></div>`;
    }
    const proposals = (t.tool_calls || []).filter(c => c.proposal && c.action).map(renderProposal).join('');
    const cost = t.cost_usd ? ` <span class="mute">${usd(t.cost_usd)}${t.model ? ' · ' + esc(t.provider) + '/' + esc(t.model) : ''}</span>` : '';
    return `<div class="chat-turn chat-forge" data-turn="${esc(t.id)}"><div class="chat-who">forge ${when}${cost}</div>` +
      renderCalls(t.tool_calls) + `<div class="chat-text">${esc(t.text)}</div>${proposals}</div>`;
  }

  function renderSession(doc, fmtTime) {
    const turns = (doc && doc.turns) || [];
    if (!turns.length) return '<p class="mute">Nothing said yet.</p>';
    return turns.map(t => renderTurn(t, fmtTime)).join('');
  }

  function renderSessions(doc, fmtTime, activeId) {
    const rows = (doc && doc.sessions) || [];
    const items = rows.map(s =>
      `<li ${s.id === activeId ? 'class="active"' : ''}><a href="/chat/${esc(s.id)}">${esc(s.title || 'session ' + s.id)}</a>` +
      `<div class="mute">${esc(fmtTime ? fmtTime(s.updated_at) : s.updated_at)} · ${esc(s.turns)} turn(s) · ${usd(s.cost_usd)}</div></li>`).join('');
    return `<a class="chat-new" href="/chat">+ new conversation</a><ul class="chat-sessions">${items}</ul>` +
      (rows.length ? '' : '<p class="mute">No conversations yet.</p>');
  }

  // The turn in flight: what the operator just said, each tool call as it
  // lands, and the reply (or the failure) when it does.
  function renderLive(live) {
    const calls = live.tools.map(t => ({ tool: t.tool, arguments: t.arguments, result: t.result, error: t.error, proposal: t.proposal }));
    let html = `<div class="chat-turn chat-user"><div class="chat-who">you</div><div class="chat-text">${esc(live.message)}</div></div>`;
    html += '<div class="chat-turn chat-forge chat-live"><div class="chat-who">forge</div>' + renderCalls(calls);
    if (live.error) html += `<div class="chat-text failed">${esc(live.error)}</div>`;
    else if (live.reply != null) html += `<div class="chat-text">${esc(live.reply)}</div>`;
    else html += '<div class="chat-text mute">working…</div>';
    return html + '</div>';
  }

  return { parseFrames, emptyLive, apply, renderCalls, renderProposal, renderTurn, renderSession, renderSessions, renderLive };
});

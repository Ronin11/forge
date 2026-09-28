// Run by web/tests/chat_render.rs under node: pure rendering, no DOM —
// the Chat page (docs/CHAT.md) against a stream of `forge chat --stream`
// events and a stored session.
const assert = require('node:assert/strict');
const C = require('../src/chat.js');
const fmtTime = secs => `T${secs}`;

// Frames are cut out of a stream that arrives in arbitrary pieces.
{
  const a = C.parseFrames('data: {"type":"session","session":3}\n\ndata: {"type":"to', '');
  assert.deepEqual(a.events, [{ type: 'session', session: 3 }]);
  const b = C.parseFrames('ol","tool":"task"}\n\n: heartbeat\n\n', a.carry);
  assert.deepEqual(b.events, [{ type: 'tool', tool: 'task' }]);
  assert.equal(b.carry, '');
  assert.deepEqual(C.parseFrames('data: not json\n\n', '').events, []);
}

// The events of one turn fold into the state the live view draws.
{
  let live = C.emptyLive('why did task 1 fail <b>');
  live = C.apply(live, { type: 'session', session: 3 });
  live = C.apply(live, { type: 'tool', tool: 'task', arguments: { id: 1 }, result: { task: { state: 'failed' } } });
  let html = C.renderLive(live);
  assert.match(html, /why did task 1 fail &lt;b&gt;/, 'the operator words are escaped');
  assert.match(html, /<code>task<\/code>/);
  assert.match(html, /working…/);
  live = C.apply(live, { type: 'reply', session: 3, text: 'It failed <script>', cost_usd: 0.0123, proposals: [] });
  html = C.renderLive(live);
  assert.match(html, /It failed &lt;script&gt;/);
  assert.doesNotMatch(html, /<script>/);
  assert.doesNotMatch(html, /working…/);
  assert.equal(live.session, 3);
  assert.equal(live.done, true);
  const failed = C.apply(C.emptyLive('x'), { type: 'error', message: 'daily budget reached' });
  assert.match(C.renderLive(failed), /class="chat-text failed">daily budget reached/);
}

const SESSION = {
  session: { id: 3, title: 'file it' },
  turns: [
    { id: 10, role: 'user', text: 'file a task about nucleosynthesis', tool_calls: [], at: 100 },
    {
      id: 11, role: 'assistant', text: 'I proposed it.', cost_usd: 0.02, provider: 'p', model: 'm', at: 101,
      tool_calls: [
        { tool: 'task', arguments: { id: 1 }, result: { task: { state: 'failed' } } },
        { tool: 'tasks', arguments: { state: 'x' }, error: 'state: unknown' },
        { tool: 'add_task', arguments: { task: 'note' }, action: '11.2', proposal: { summary: 'File a task in project demo: note', status: 'proposed' } },
      ],
    },
    { id: 12, role: 'action', text: 'Confirmed: filed task 2', tool_calls: [], at: 102 },
  ],
};

// A stored session: turns in order, calls collapsed, the proposal with buttons.
{
  const html = C.renderSession(SESSION, fmtTime);
  assert.ok(html.indexOf('nucleosynthesis') < html.indexOf('I proposed it.'));
  assert.ok(html.indexOf('I proposed it.') < html.indexOf('Confirmed: filed task 2'));
  assert.match(html, /<details><summary><code>task<\/code>/);
  assert.match(html, /class="failed">state: unknown/);
  assert.match(html, /<button class="chat-confirm" data-action="11\.2" data-verb="confirm">confirm<\/button>/);
  assert.match(html, /<button class="chat-reject" data-action="11\.2" data-verb="reject">reject<\/button>/);
  assert.match(html, /File a task in project demo: note/);
  assert.match(html, /T101/, 'times go through the caller\'s fmtTime');
  assert.match(html, /\$0\.0200/);
}

// A decided proposal keeps its outcome and loses its buttons.
{
  const decided = JSON.parse(JSON.stringify(SESSION));
  decided.turns[1].tool_calls[2].proposal = { summary: 'File a task', status: 'confirmed', outcome: 'filed task 2 (direct)' };
  const html = C.renderSession(decided, fmtTime);
  assert.doesNotMatch(html, /chat-confirm|chat-reject/);
  assert.match(html, /confirmed: filed task 2 \(direct\)/);
  decided.turns[1].tool_calls[2].proposal.status = 'rejected';
  assert.doesNotMatch(C.renderSession(decided, fmtTime), /<button/);
}

// The sessions list links each session and marks the active one.
{
  const html = C.renderSessions({ sessions: [
    { id: 3, title: 'file it', updated_at: 5, turns: 3, cost_usd: 0.03 },
    { id: 2, title: '', updated_at: 4, turns: 2, cost_usd: 0 },
  ] }, fmtTime, 3);
  assert.match(html, /<li class="active"><a href="\/chat\/3">file it<\/a>/);
  assert.match(html, /<a href="\/chat\/2">session 2<\/a>/);
  assert.match(html, /href="\/chat">\+ new conversation/);
  assert.match(C.renderSessions({ sessions: [] }, fmtTime, null), /No conversations yet/);
}

// A markup-bearing tool result never becomes markup.
{
  const html = C.renderCalls([{ tool: 'task', arguments: {}, result: { reason: '<img src=x onerror=alert(1)>' } }]);
  assert.doesNotMatch(html, /<img/);
}
console.log('ok');

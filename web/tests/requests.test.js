// Run by web/tests/requests_render.rs under node: pure rendering, no DOM
// — a fixture with two questions renders two inline answer boxes, one
// per question, while every row (any kind) gets the shared withdraw
// control, and an unverified task renders a land control.
const assert = require('node:assert/strict');
const { renderRequestRows, renderUnverifiedRows } = require('../src/requests.js');

const FIXTURE = [
  {
    id: 1, kind: 'question', to: null, text: 'which db?', question: 'which db?',
    tried: 'looked at config', path: '', workflow: 'direct', repo: '/r', task: 'add a db',
  },
  {
    id: 2, kind: 'question', to: 'alice', text: 'ship now?', question: 'ship now?',
    tried: '', path: '', workflow: 'direct', repo: '/r', task: 'release',
  },
  {
    id: 3, kind: 'dependency', to: null,
    text: 'waits on task 14 (failed: L1 failed: test)', question: 'waits on task 14 (failed: L1 failed: test)',
    tried: '', path: '', workflow: 'direct', repo: '/r', task: 'fix things',
  },
];

const html = renderRequestRows(FIXTURE);
// Two questions, two answer boxes — not one per row.
const answerBoxes = html.match(/class="req-answer"/g) || [];
assert.equal(answerBoxes.length, 2, html);
assert.match(html, /which db\?/);
assert.match(html, /waits on task 14 \(failed: L1 failed: test\)/);
assert.match(html, /to alice/);
// Every row — question or dependency — gets the shared withdraw control.
const withdrawControls = html.match(/class="req-withdraw"/g) || [];
assert.equal(withdrawControls.length, 3, html);

// An empty fixture renders no boxes and no controls, not a broken page.
assert.equal((renderRequestRows([]).match(/class="req-answer"/g) || []).length, 0);
assert.match(renderRequestRows([]), /No open questions/);

// Lineage and the last attempt's summary, when the caller has attached
// them (from `/api/task/<id>`), show under the question text.
const enriched = [{
  ...FIXTURE[0],
  lineage: [{ id: 1, state: 'blocked' }, { id: 5, state: 'queued' }],
  last_summary: 'looked at three drivers',
}];
const enrichedHtml = renderRequestRows(enriched);
assert.match(enrichedHtml, /looked at three drivers/);
assert.match(enrichedHtml, /href="\/tasks\/5"/);
assert.match(enrichedHtml, /<b>1 blocked<\/b>/);

// An unverified task renders a land control, not an answer or withdraw
// box — it passed, it just never landed.
const unverified = [{ id: 9, workflow: 'direct', task: 'polish the button', cost_usd: 1.2 }];
const uHtml = renderUnverifiedRows(unverified);
assert.match(uHtml, /class="req-land"/);
assert.match(uHtml, /polish the button/);
assert.doesNotMatch(uHtml, /req-answer/);
assert.doesNotMatch(uHtml, /req-withdraw/);

// No unverified tasks: nothing renders, so the page can hide the section.
assert.equal(renderUnverifiedRows([]), '');

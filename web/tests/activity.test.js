// Run by web/tests/activity_render.rs under node: pure rendering, no
// DOM — the /activity page (web UI task 8, "activity") against
// tests/fixtures/activity.json, a fixture event stream covering every
// feed kind the task names plus a handful of raw types the feed must
// never show (task_queued, tool_call, agent_done, note, op). Checks that
// the fixture renders the right kinds, newest first, and that each of
// project/kind/task narrows the feed; also exercises the
// running-attempts panel built by replaying attempt events.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const {
  KINDS, kindOf, renderFeed, reduceRunning, renderRunningAttempts,
} = require('../src/activity.js');

const fmtTime = ts => `t${ts}`;

const FIXTURE = JSON.parse(
  fs.readFileSync(path.join(__dirname, '..', '..', 'tests', 'fixtures', 'activity.json'), 'utf8'),
);

const TASK_PROJECTS = { 1: 'acme', 2: 'acme', 3: 'globex' };

// Every kind the task names, in its own order.
assert.deepEqual(KINDS, [
  'task started', 'attempt started', 'attempt done', 'check', 'pushed',
  'task done', 'question', 'deploy started', 'deploy finished',
  'job started', 'job finished', 'initiative settled',
]);

// The unfiltered feed shows exactly one row per feed-kind event in the
// fixture (13 of the 20 raw lines: task_queued, both task-1 tool_calls,
// task-2's tool_call, agent_done, note and op are never feed rows), and
// nothing else.
const all = renderFeed(FIXTURE, {}, fmtTime, TASK_PROJECTS);
assert.equal((all.match(/class="card activity-row"/g) || []).length, 13);
for (const kind of KINDS) {
  assert.match(all, new RegExp(`data-kind="${kind}"`), `missing a ${kind} row`);
}
for (const excluded of ['task_queued', 'tool_call', 'agent_done', 'note', 'op']) {
  assert.equal(kindOf(FIXTURE.find(e => e.type === excluded)), null, `${excluded} should not be a feed kind`);
}
// The blocked task_done (task 2) renders as a question, not a second
// "task done" row.
assert.equal((all.match(/data-kind="task done"/g) || []).length, 1);
assert.equal((all.match(/data-kind="question"/g) || []).length, 1);

// Newest first: initiative_settled (ts 117, the newest feed row) comes
// before task_started (ts 100, the oldest).
assert.ok(all.indexOf('initiative settled') < all.indexOf('task started'));

// Filtering by kind narrows to just that kind.
const checks = renderFeed(FIXTURE, { kind: 'check' }, fmtTime, TASK_PROJECTS);
assert.equal((checks.match(/class="card activity-row"/g) || []).length, 1);
assert.match(checks, /data-kind="check"/);

// Filtering by project narrows across event shapes: deploy_*/job_*
// events carry `project` directly, task_done/initiative_settled resolve
// it through the id → project map.
const shop = renderFeed(FIXTURE, { project: 'shop' }, fmtTime, TASK_PROJECTS);
assert.equal((shop.match(/class="card activity-row"/g) || []).length, 4);
const globex = renderFeed(FIXTURE, { project: 'globex' }, fmtTime, TASK_PROJECTS);
assert.equal((globex.match(/class="card activity-row"/g) || []).length, 1);
assert.match(globex, /data-kind="initiative settled"/);

// Filtering by task narrows to just that task's own rows.
const task1 = renderFeed(FIXTURE, { task: 1 }, fmtTime, TASK_PROJECTS);
assert.equal((task1.match(/class="card activity-row"/g) || []).length, 6);
assert.ok(!task1.includes('data-kind="question"'));

// No match at all still renders something, not an empty string.
const none = renderFeed(FIXTURE, { project: 'nobody' }, fmtTime, TASK_PROJECTS);
assert.match(none, /no matching activity/);

// ---- running-attempts panel: replaying attempt_started/tool_call/
// agent_done/attempt_done against a small scripted stream.
const stream = [
  { type: 'attempt_started', task: 5, ts: 1, n: 1, of: 2 },
  { type: 'tool_call', task: 5, ts: 2, name: 'Bash' },
  { type: 'tool_call', task: 5, ts: 3, name: 'Edit' },
  { type: 'agent_done', task: 5, ts: 4, turns: 8, cost_usd: 0.25 },
];
const running = reduceRunning(stream, []);
assert.equal(running.length, 1);
assert.deepEqual(running[0], { task: 5, n: 1, of: 2, toolCalls: 2, lastTool: 'Edit', turns: 8, cost: 0.25 });
const html = renderRunningAttempts(running);
assert.match(html, /attempt 1 of 2/);
assert.match(html, /<td class="num">8<\/td>/);
assert.match(html, /<td class="num">2<\/td>/);
assert.match(html, /\$0\.25/);
assert.match(html, /Edit/);

// attempt_done (or task_done) closes the attempt out of the panel.
const closed = reduceRunning([...stream, { type: 'attempt_done', task: 5, ts: 5, state: 'succeeded', reason: '' }], []);
assert.equal(closed.length, 0);

// A task the snapshot already showed as `running` shows up even before
// any of its own attempt events have arrived in this client's window.
const seeded = reduceRunning([], [7]);
assert.equal(seeded.length, 1);
assert.equal(seeded[0].task, 7);
assert.match(renderRunningAttempts(seeded), /<a href="\/tasks\/7">7<\/a>/);

// An empty running-attempts list renders a plain "nothing running" line.
assert.match(renderRunningAttempts([]), /no attempts running/);

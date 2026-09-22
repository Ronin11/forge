// Run by web/tests/task_render.rs under node: pure rendering, no DOM —
// the full task page (task 531) against tests/fixtures/trace.json, a
// three-attempt, two-step `forge trace --json` fixture with one failed
// L1 check and a follow-up attempt that fixes it. A fixture with N
// attempts across M steps must render every one of them: this is the
// snapshot the task asks for, not just a assert-something-shows smoke
// test.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const {
  requestKind, renderVerdictRows, renderToolStats, renderStepGroups,
  renderActions, renderTaskDetail,
} = require('../src/task.js');

const fmtTime = secs => (secs == null ? '' : `t${secs}`);

const FIXTURE = JSON.parse(
  fs.readFileSync(path.join(__dirname, '..', '..', 'tests', 'fixtures', 'trace.json'), 'utf8'),
);

// `requestKind` mirrors `src/view.rs`'s `request_kind` exactly.
assert.deepEqual(requestKind('waits on task 14 (failed: boom)'), ['dependency', 'waits on task 14 (failed: boom)']);
assert.deepEqual(requestKind('needs workflow: no run workflow'), ['workflow', 'no run workflow']);
assert.deepEqual(requestKind('needs suite: tests/hidden/x.rs'), ['suite', 'tests/hidden/x.rs']);
assert.deepEqual(requestKind('needs input: which db?'), ['question', 'which db?']);
assert.deepEqual(requestKind('review demoted: scope crept'), ['review', 'scope crept']);
assert.deepEqual(requestKind('something else entirely'), ['other', 'something else entirely']);

const html = renderTaskDetail(FIXTURE, fmtTime, {});

// Every attempt in the fixture renders, under its own step heading.
for (const n of [1, 2, 3]) assert.match(html, new RegExp(`attempt ${n}\\b`), html);
const stepHtml = renderStepGroups(FIXTURE.attempts);
const stepHeadings = stepHtml.match(/<h3>[^<]+<\/h3>/g) || [];
assert.deepEqual(stepHeadings, ['<h3>investigate</h3>', '<h3>code</h3>']);
// Attempts 2 and 3 both ran under "code", so its group holds both.
const codeGroup = stepHtml.split('<h3>code</h3>')[1];
assert.match(codeGroup, /attempt 2\b/);
assert.match(codeGroup, /attempt 3\b/);

// The failed L1 check's rule name always shows; its tail sits behind a
// collapsed <details>, not inline.
assert.match(html, /L1 cargo test/);
assert.match(html, /<details><summary>tail<\/summary><pre>/);
assert.match(html, /assertion failed: body\.contains/);
// A passing check still shows, just without a tail.
assert.match(html, /L1 cargo clippy/);
// The failing test name from `failing_tests` shows too.
assert.match(html, /failing: task_render::a_fixture_renders_every_step_and_attempt/);

// Tool statistics (`outputs.tools`) render per attempt, behind their own
// <details> — every attempt in the fixture reported at least one tool.
const toolDetails = html.match(/tool stats/g) || [];
assert.equal(toolDetails.length, 3, html);
assert.match(renderToolStats(FIXTURE.attempts[1].outputs.tools), /cargo test/);
assert.match(renderToolStats(FIXTURE.attempts[1].outputs.tools), /Bash/);

// Diagnosis, journal, lineage, refs, deploys and the assessment all show.
assert.match(html, /burned 18 turns/);
assert.match(html, /for the existing pattern/);
assert.match(html, /href="\/tasks\/43"/);
assert.match(html, /#9/);
assert.match(html, /prod/);
assert.match(html, /zebra striping/);

// The branch always shows; no compare link unless the caller supplies one.
assert.match(html, /forge\/42-retry-button/);
assert.doesNotMatch(html, /compare →/);
const withCompare = renderTaskDetail(FIXTURE, fmtTime, { compare: 'https://github.com/nate/equitizr/compare/main...forge/42-retry-button?expand=1' });
assert.match(withCompare, /compare →/);
assert.match(withCompare, /github\.com\/nate\/equitizr\/compare/);

// Actions match the task's own state: a succeeded (not queued/running)
// task can still be retried, but it is not blocked or unverified, so no
// answer, withdraw, or land control shows.
assert.match(html, /class="task-retry"/);
assert.doesNotMatch(html, /req-land/);
assert.doesNotMatch(html, /req-answer/);
assert.doesNotMatch(html, /req-withdraw/);

// A blocked task waiting on a question gets both an answer box and a
// withdraw control; a blocked task on any other kind of reason gets only
// withdraw.
const blockedOnQuestion = { ...FIXTURE.task, id: 99, state: 'blocked', reason: 'needs input: which db?' };
const questionActions = renderActions(blockedOnQuestion);
assert.match(questionActions, /req-answer/);
assert.match(questionActions, /req-withdraw/);
const blockedOnDependency = { ...FIXTURE.task, id: 98, state: 'blocked', reason: 'waits on task 14 (queued)' };
const dependencyActions = renderActions(blockedOnDependency);
assert.doesNotMatch(dependencyActions, /req-answer/);
assert.match(dependencyActions, /req-withdraw/);

// An unverified task gets a land control and, since it is not
// queued/running, still a retry.
const unverified = { ...FIXTURE.task, id: 97, state: 'unverified', reason: '' };
const unverifiedActions = renderActions(unverified);
assert.match(unverifiedActions, /req-land/);
assert.match(unverifiedActions, /task-retry/);

// Queued and running tasks get no actions at all: nothing to retry yet,
// nothing blocked, nothing to land.
for (const state of ['queued', 'running']) {
  const none = renderActions({ ...FIXTURE.task, state, reason: '' });
  assert.equal(none, '', `${state}: ${none}`);
}

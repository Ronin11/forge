// Run by web/tests/doctor_render.rs under node: pure rendering, no DOM —
// the /doctor page (web UI task 7, "doctor") against
// tests/fixtures/doctor.json, a doctor document with exactly one WARN
// check (`worktrees`, two retained tasks) among a page of OK checks: the
// task asks specifically for a fixture with one WARN that renders its
// fix line, so this checks that line shows up, not just that something
// renders.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const {
  renderCheckRow, renderChecks, renderHeldInitiatives, renderWorktrees,
  renderRateLimits, renderSpend, renderLearning, renderDoctorDoc,
} = require('../src/doctor.js');

const fmtTime = ts => `t${ts}`;

const FIXTURE = JSON.parse(
  fs.readFileSync(path.join(__dirname, '..', '..', 'tests', 'fixtures', 'doctor.json'), 'utf8'),
);

const html = renderDoctorDoc(FIXTURE, [], fmtTime);

// Every check shows up as a row, named and with its own detail.
for (const name of ['binary.codex', 'git', 'sandbox', 'home', 'cache', 'config', 'schema', 'purposes', 'workflows', 'plugins', 'learning', 'worker', 'queue', 'worktrees', 'logs', 'initiatives', 'spend', 'rate_limit']) {
  assert.match(html, new RegExp(`>${name.replace('.', '\\.')}<`), `missing row for ${name}`);
}

// The one WARN check (worktrees) renders its state and its fix line —
// the task's own requirement.
assert.match(html, /WARN/);
assert.match(html, /2 retained: \[3, 7\]/);
assert.match(html, /fix: forge gc removes the published ones and explains the rest/);

// Every other check is OK, and OK checks never show a "fix:" line of
// their own (none of the fixture's OK rows carry a non-empty hint).
assert.equal((html.match(/>OK<\/td>/g) || []).length, 17);
assert.equal((html.match(/>WARN<\/td>/g) || []).length, 1);

// The worktrees section draws a gc control since worktree_ids is
// non-empty, addressed at the write route the doctor page's gc control
// uses.
assert.match(html, /<form class="doctor-gc"><button type="submit">forge gc<\/button><\/form>/);

// The rate-limit section draws the provider's own reset times through
// the caller's fmtTime, not raw unix seconds.
assert.match(html, /5h 42%.*resets t2000003600/);
assert.match(html, /7d 18%.*resets t2000600000/);

// Spend draws the structured usd figures, not the raw prose.
assert.match(html, /\$3\.50 of \$50\.00/);

// Learning is OK here, so it shows the plain detail rather than a
// split-into-lines list.
assert.match(html, /3 of 3 workflow\(s\) measured; no regressions/);

// A refresh control is always present.
assert.match(html, /<form class="doctor-refresh"><button type="submit">refresh<\/button><\/form>/);

// No held initiatives in this fixture: the section renders nothing.
assert.doesNotMatch(html, /Held initiatives/);

// renderHeldInitiatives on its own: one held initiative draws the same
// budget/stop-after control the initiative page uses, addressed at
// task 4's write route by id.
const heldHtml = renderHeldInitiatives([
  { id: 9, project: 'demo', outcome: 'ship it', state: 'held', held_rule: 'budget', budget_usd: 25, stop_after_same_rule: 3 },
]);
assert.match(heldHtml, /initiative 9/);
assert.match(heldHtml, /held: budget/);
assert.match(heldHtml, /<form class="ini-set" data-id="9">/);
assert.match(heldHtml, /value="25"/);
assert.match(heldHtml, /class="ini-stop-after" value="3"/);
assert.equal(renderHeldInitiatives([]), '');
assert.equal(renderHeldInitiatives(null), '');

// renderWorktrees with nothing retained draws no gc control.
assert.doesNotMatch(
  renderWorktrees([{ name: 'worktrees', status: 'ok', detail: 'none retained', hint: '' }]),
  /doctor-gc/,
);

// renderLearning on a WARN check splits its semicolon-joined lines.
const learningHtml = renderLearning([
  { name: 'learning', status: 'warn', detail: 'direct regressed on anthropic (10% vs 80%); repo-flow verifies 0/10 on local (95% upper 20%)', hint: 'revert the workflow' },
]);
assert.match(learningHtml, /direct regressed on anthropic \(10% vs 80%\)/);
assert.match(learningHtml, /repo-flow verifies 0\/10 on local \(95% upper 20%\)/);

// renderCheckRow and renderChecks on their own, and the empty case.
assert.match(renderCheckRow({ name: 'x', status: 'fail', detail: 'broken', hint: 'fix it' }), /FAIL/);
assert.match(renderChecks([]), /no checks/);

// renderSpend and renderRateLimits fall back to the plain detail line
// when the structured numbers are absent.
assert.match(renderSpend([{ name: 'spend', status: 'ok', detail: 'no cap set', hint: '' }]), /no cap set/);
assert.equal(renderRateLimits([], fmtTime), '');

// Run by web/tests/initiative_render.rs under node: pure rendering, no
// DOM — the full initiative page (task 532, "the initiative page in
// full") against tests/fixtures/initiative.json, a held initiative over
// its own budget with two refused rules, a ruling, an open question and
// a deploy. The task asks specifically for the cost-vs-budget bar, the
// refused rules, and the held reason to render: this checks all three,
// not just that something shows.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const {
  renderCostBar, renderStateLine, renderTasksTable, renderRefused,
  renderInitiativeDoc,
} = require('../src/initiative.js');

const fmtSpan = secs => `${secs}s`;

const FIXTURE = JSON.parse(
  fs.readFileSync(path.join(__dirname, '..', '..', 'tests', 'fixtures', 'initiative.json'), 'utf8'),
);

const html = renderInitiativeDoc(FIXTURE, fmtSpan);

// The outcome, and every task's own state and cost.
assert.match(html, /the billing page shows usage-based line items/);
for (const id of [40, 41, 42, 43]) assert.match(html, new RegExp(`href="/tasks/${id}"`));
assert.match(html, /\$3\.20/); // task 40's own cost
assert.match(html, /\$4\.75/); // task 41's own cost

// The cost-vs-budget bar: over budget ($10.50 of $10.00), so it is full
// width and marked `over`.
assert.match(html, /class="bar-fill over" style="width:100\.0%"/);
assert.match(html, /\$10\.50 of \$10\.00/);
assert.match(html, /over budget/);
const underBudget = renderCostBar(2.5, 10);
assert.match(underBudget, /class="bar-fill" style="width:25\.0%"/);
assert.doesNotMatch(underBudget, /over/);
const noBudget = renderCostBar(2.5, null);
assert.match(noBudget, /no budget cap/);
assert.doesNotMatch(noBudget, /bar-fill/);

// The refused rules, each with its own count.
assert.match(html, /cargo clippy: 3/);
assert.match(html, /cargo test: 1/);
assert.match(renderRefused([]), /^$/);

// The held reason: state `held` on rule `budget` shows "held: budget".
assert.match(html, /held: budget/);
assert.match(renderStateLine(FIXTURE), /held: budget/);
const openLine = renderStateLine({ ...FIXTURE, state: 'open', held_rule: null });
assert.doesNotMatch(openLine, /held/);

// Rulings, questions and the deploy all show.
assert.match(html, /is the retry budget worth raising/);
assert.match(html, /which pricing tier\?/);
assert.match(html, /unanswered/);
assert.match(html, /prod cdcce8ad/);

// Elapsed time, through the caller's own formatter.
assert.match(html, /5400s elapsed/);

// The budget/stop-after control is present, pre-filled with the current
// settings.
assert.match(html, /class="ini-set" data-id="7"/);
assert.match(html, /class="ini-budget"[^>]*value="10"/);
assert.match(html, /class="ini-stop-after"[^>]*value="3"/);

// A blocked task gets a withdraw control; the others don't.
const tasksHtml = renderTasksTable(FIXTURE.tasks);
const blockedRow = tasksHtml.split('<tr>').find(r => r.includes('>42<'));
assert.match(blockedRow, /req-withdraw/);
const succeededRow = tasksHtml.split('<tr>').find(r => r.includes('>40<'));
assert.doesNotMatch(succeededRow, /req-withdraw/);

// Run by web/tests/deploys_render.rs under node: pure rendering, no DOM —
// the /deploys page (task 534, "deploys") against tests/fixtures/deploys.json,
// one target with two deploys: a passing one (check/smoke/look all ok, a
// screenshot) and a failing one that rolled back. The task asks
// specifically for a fixture with one target and two deploys to render
// both verdicts and the screenshot tag: this checks all three, not just
// that something shows.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { verdictBadge, renderDeployLog, renderTarget, renderDeploys } = require('../src/deploys.js');

const fmtTime = ts => `t${ts}`;

const FIXTURE = JSON.parse(
  fs.readFileSync(path.join(__dirname, '..', '..', 'tests', 'fixtures', 'deploys.json'), 'utf8'),
);

const html = renderDeploys(FIXTURE, fmtTime);

// The target itself: method, host, and the last (newest) deploy's time.
assert.match(html, /equitizr \/ prod/);
assert.match(html, /rsync/);
assert.match(html, /equitizr\.example\.com/);
assert.match(html, /t200/);

// Deploy 12 (the newest, passing) shows all three verdicts ok.
assert.match(html, /check ok/);
assert.match(html, /smoke ok/);
assert.match(html, /look ok/);

// Deploy 11 (the older, failing) shows its own failed check and rollback
// — and, since it never ran smoke, none of its own verdict badges.
assert.match(html, /check FAILED/);
assert.match(html, /rolled back to 99999999/);
assert.match(html, /connection refused|failed its check/);

// The screenshot tag: only deploy 12 declared a smoke_json, so only it
// gets an <img> pointed at its own id.
assert.match(html, /<img class="deploy-shot" src="\/api\/deploys\/shot\/12"/);
assert.doesNotMatch(html, /\/api\/deploys\/shot\/11/);

// A "deploy now" control, addressed to this exact target.
assert.match(html, /class="deploy-now" data-project="equitizr" data-target="prod"/);

// verdictBadge itself: null/undefined draw nothing (never ran, or the
// target declares no smoke url).
assert.equal(verdictBadge('check', null), '');
assert.equal(verdictBadge('check', undefined), '');
assert.match(verdictBadge('check', true), /check ok/);
assert.match(verdictBadge('check', false), /check FAILED/);

// renderDeployLog and renderTarget on their own, and the empty cases.
assert.match(renderDeployLog(FIXTURE[0].deploys, fmtTime), /cdcce8ad/);
assert.match(renderDeployLog([], fmtTime), /no deploys/);
assert.match(renderTarget(FIXTURE[0], fmtTime), /equitizr \/ prod/);
assert.match(renderDeploys([], fmtTime), /no deploy targets/);

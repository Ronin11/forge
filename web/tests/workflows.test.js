// Run by web/tests/workflows_render.rs under node: pure rendering, no DOM
// — a fixture of two workflows renders two rows, and a lint problem
// renders at its line.
const assert = require('node:assert/strict');
const { renderWorkflowRows, renderLintProblems, profileLine } = require('../src/workflows.js');

const FIXTURE = [
  {
    name: 'direct', kind: 'build', source: 'catalog', project: null,
    resolved: [{ action: 'setup', kind: 'operation', contract: 'setup' }, { action: 'code', kind: 'directive', contract: 'code' }],
    steps: [],
    measured: { current: { n: 12, known: true, rate: 0.8333333333333334, rate_lo: 0.55, rate_hi: 0.95, cost_per_success: 1.44 }, regressed: false },
  },
  {
    name: 'repo-flow', kind: 'build', source: 'repo', project: 'demo',
    resolved: null,
    steps: [{ action: 'setup' }],
    measured: { current: { n: 3, known: false }, regressed: false },
  },
];

const html = renderWorkflowRows(FIXTURE);
const rows = html.match(/<tr class="task"/g) || [];
assert.equal(rows.length, 2, html);
assert.match(html, /direct/);
assert.match(html, /repo-flow/);
assert.match(html, /href="\/workflows\/direct"/);
assert.match(html, /href="\/workflows\/repo-flow\?project=demo"/);
assert.match(html, /83%/); // rate for the known profile
assert.match(html, /not yet known/); // the unknown profile

// An empty fixture renders no rows, not a broken table.
assert.equal((renderWorkflowRows([]).match(/<tr class="task"/g) || []).length, 0);

// A lint problem with a line renders under that line.
const problems = [{ line: 3, message: 'unknown action "nope"' }];
const lintHtml = renderLintProblems(problems);
assert.match(lintHtml, /data-line="3"/);
assert.match(lintHtml, /line 3/);
assert.match(lintHtml, /unknown action/);

// A whole-flow problem with no line still renders, without a line prefix.
const noLine = renderLintProblems([{ line: null, message: 'a data-flow violation' }]);
assert.match(noLine, /data-line=""/);
assert.doesNotMatch(noLine, /line null/);
assert.match(noLine, /data-flow violation/);

// No problems at all renders a clean message, not an empty string.
assert.match(renderLintProblems([]), /no problems/);

assert.equal(profileLine(null), '<span class="mute">no runs</span>');

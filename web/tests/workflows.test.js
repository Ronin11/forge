// Run by web/tests/workflows_render.rs under node: pure rendering, no DOM
// — a fixture of two workflows renders two rows, and a lint problem
// renders at its line.
const assert = require('node:assert/strict');
const { renderWorkflowRows, renderLintProblems, profileLine, draftOutput, renderDraftPanel } = require('../src/workflows.js');

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

// The prompter (`/workflows/new`): a fixture job document with a finished
// `draft-workflow` step renders the draft's rationale and both of its
// open questions.
const OK_JOB = {
  id: 77, project: 'forge', workflow: 'author-workflow', state: 'ok',
  steps: [
    { id: 1, job_id: 77, seq: 0, action: 'dump-workflow-catalog', kind: 'operation', output_ref: '' },
    {
      id: 2, job_id: 77, seq: 1, action: 'draft-workflow', kind: 'directive',
      output_ref: '/tmp/output-draft-workflow.json',
      output: {
        name: 'save-note', kind: 'run', description: 'writes a note to disk',
        toml: 'name = "save-note"\nkind = "run"\n',
        rationale: 'the write-file operation already does exactly this',
        open_questions: [
          'the webhook\'s own name is guessed from the description',
          'the per_day rate and on_failure are guessed defaults',
        ],
      },
    },
    { id: 3, job_id: 77, seq: 2, action: 'lint-workflow-draft', kind: 'operation', output_ref: '' },
  ],
  effects: [],
};

assert.deepEqual(draftOutput(OK_JOB), OK_JOB.steps[1].output);
assert.equal(draftOutput({ state: 'running', steps: [] }), null);
assert.equal(draftOutput(null), null);

const panel = renderDraftPanel(OK_JOB, null);
assert.match(panel, /the write-file operation already does exactly this/);
const questions = panel.match(/<li>/g) || [];
assert.equal(questions.length, 2, panel);
assert.match(panel, /webhook.s own name is guessed/);
assert.match(panel, /per_day rate and on_failure/);

// A job that ended `needs_human` shows the human rung's question text and
// a link to the task it was filed as, once one is found.
const NEEDS_HUMAN_JOB = { id: 78, project: 'forge', workflow: 'author-workflow', state: 'needs_human', steps: [], effects: [] };
const needsHuman = renderDraftPanel(NEEDS_HUMAN_JOB, { text: 'job 78 (author-workflow) failed: budget: $0.60 of $0.50', task_id: 9 });
assert.match(needsHuman, /needs a human/);
assert.match(needsHuman, /budget: \$0\.60 of \$0\.50/);
assert.match(needsHuman, /href="\/tasks\/9"/);

// Still resolving (or no filed task found): the panel renders without a
// link rather than crashing on a missing one.
const needsHumanNoLink = renderDraftPanel(NEEDS_HUMAN_JOB, null);
assert.match(needsHumanNoLink, /no question recorded/);
assert.doesNotMatch(needsHumanNoLink, /href="\/tasks\//);

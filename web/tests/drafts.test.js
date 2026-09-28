// Run by web/tests/drafts_render.rs under node: the draft editor's model
// and rendering are pure — a step list with edges and a placeholder edits
// as data, renders as a list plus an SVG of its edges, and shows the
// linter's answer (never its own).
const assert = require('node:assert/strict');
const D = require('../src/drafts.js');

let d = D.emptyDraft('docs-lint', 'run');
d = D.addStep(d, 'code');
d = D.addPlaceholder(d, 'lint-docs', { kind: 'operation', inputs: 'the tree', outputs: 'a verdict' });
d = D.setField(d, 0, 'role', 'author');
d = D.setEdge(d, 1, 'failure', 'code');
d = D.setEdge(d, 0, 'failure', 'end');
assert.equal(d.steps.length, 2);
assert.equal(d.steps[1].placeholder.inputs, 'the tree');
assert.deepEqual(d.steps[0].on, { failure: 'end' });
assert.equal(d.settings.trigger.on, 'manual', 'a run draft starts with a trigger');

// edges resolve a step name, a node id and `end`
assert.deepEqual(D.edges(d.steps), [{ from: 0, to: 'end', key: 'failure' }, { from: 1, to: 0, key: 'failure' }]);
assert.deepEqual(D.edges(D.setEdge(d, 1, 'failure', '0-code').steps), [{ from: 0, to: 'end', key: 'failure' }, { from: 1, to: 0, key: 'failure' }]);

// the graph: a node per step (plus end), a forward arrow, a dashed loop per edge
const svg = D.renderGraph(d.steps);
assert.equal((svg.match(/class="graph-node/g) || []).length, 3, svg);
assert.equal((svg.match(/class="graph-edge"/g) || []).length, 2, svg);
assert.equal((svg.match(/class="graph-next"/g) || []).length, 1, svg);
assert.match(svg, /graph-node placeholder/);

// moving, removing, clearing a field
assert.equal(D.moveStep(d, 1, -1).steps[0].action, 'lint-docs');
assert.equal(D.moveStep(d, 0, -1), d, 'moving past the top is a no-op');
assert.equal(D.removeStep(d, 0).steps.length, 1);
assert.equal(D.setField(d, 0, 'role', '').steps[0].role, undefined);
assert.equal(D.removeEdge(d, 0, 'failure').steps[0].on, undefined);
assert.equal(d.steps[0].role, 'author', 'edits never change the draft they started from');

// the list shows the placeholder's contract line and the linter's problems on the step
const annotated = Object.assign({}, d, {
  status: 'incomplete', pending: ['lint-docs'], tasks: [{ action: 'lint-docs', task_id: 7 }],
  info: [{ kind: 'directive', contract: 'code', description: 'writes the change' }, { placeholder: true, landed: false }],
  problems: [{ step: 1, line: 9, message: 'step 2 has no judgment' }],
});
const list = D.renderStepList(annotated);
assert.equal((list.match(/class="step-card/g) || []).length, 2, list);
assert.match(list, /inputs: the tree; outputs: a verdict; kind: operation/);
assert.match(list, /step 2 has no judgment/);
assert.match(list, /has-problem/);
assert.match(D.renderPending(annotated), /task 7/);
assert.match(D.renderPending(annotated), /incomplete/);
assert.match(D.renderProblems([]), /clean/);
assert.match(D.renderProblems(annotated.problems), /step 2/);

// a build draft shows no run-only controls, and switching kind carries the right sections
const build = D.setKind(D.addStep(D.emptyDraft('b', 'build'), 'code'), 'build');
assert.doesNotMatch(D.renderStepList(build), /data-f="judgment"/);
assert.equal(D.setKind(build, 'run').settings.trigger.on, 'manual');
assert.deepEqual(D.setKind(D.setKind(build, 'run'), 'build').settings, {});

// the one-shot suggestion is proposed, edited by accepting steps
const proposed = Object.assign({}, build, { proposal: [{ action: 'a' }, { action: 'b' }] });
assert.match(D.renderProposal(proposed), /suggested steps/);
assert.deepEqual(D.acceptProposal(proposed, [1]).steps.map(s => s.action), ['code', 'b']);
assert.deepEqual(D.acceptProposal(proposed, []).proposal, []);
assert.equal(D.renderProposal(build), '');

// the picker shows each action's contract
const actions = [{ name: 'code', kind: 'directive', contract: 'code', description: 'writes', consumes: ['branch'], produces: ['branch'], outcomes: [] }];
assert.match(D.renderActionOptions(actions, 'code'), /code \(directive, code\)/);
assert.match(D.renderActionContract(actions, 'code'), /consumes branch/);
assert.match(D.renderActionContract(actions, 'nope'), /not in the catalog/);
assert.equal(D.bare(annotated).pending, undefined, 'only the draft goes back up');

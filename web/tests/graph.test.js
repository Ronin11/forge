// Run by web/tests/graph_render.rs under node: pure rendering, no DOM —
// a three-node, two-edge fixture renders three nodes and two edges, the
// overlay shows up as a cost colour and a demotion badge, and hovering
// (the SVG <title>) lists a node's tasks.
const assert = require('node:assert/strict');
const { moduleGraph, layoutModules, nodeCost, nodeDemotions, renderModuleGraph } = require('../src/graph.js');

const THREE_MODULES = {
  nodes: [
    { path: 'src', kind: 'module', symbols: 5, lines: 120, overlay: { tasks: [], demotions: [], repair_cost_usd: 0 } },
    { path: 'lib', kind: 'module', symbols: 2, lines: 40, overlay: { tasks: [], demotions: [], repair_cost_usd: 0 } },
    { path: 'web', kind: 'module', symbols: 1, lines: 10, overlay: { tasks: [], demotions: [], repair_cost_usd: 0 } },
  ],
  edges: [
    { from: 'src', to: 'lib' },
    { from: 'lib', to: 'web' },
  ],
};

const { svgHtml, width, height } = renderModuleGraph(THREE_MODULES);
assert.equal((svgHtml.match(/class="mnode"/g) || []).length, 3, svgHtml);
assert.equal((svgHtml.match(/class="medge"/g) || []).length, 2, svgHtml);
assert.match(svgHtml, /data-path="src"/);
assert.match(svgHtml, /data-path="lib"/);
assert.match(svgHtml, /data-path="web"/);
assert.ok(width > 0 && height > 0, `${width}x${height}`);

// A layered layout puts a node strictly to the right of what points into
// it: src -> lib -> web, so src.x < lib.x < web.x.
{
  const g = moduleGraph(THREE_MODULES);
  const { pos } = layoutModules(g.nodes, g.edges);
  assert.ok(pos.get('src').x < pos.get('lib').x, [...pos.entries()]);
  assert.ok(pos.get('lib').x < pos.get('web').x, [...pos.entries()]);
}

// A cycle (mutual imports) never loses a node: every node still gets a
// layer, so it still renders.
{
  const cyc = {
    nodes: [
      { path: 'a', kind: 'module', symbols: 1, lines: 5, overlay: {} },
      { path: 'b', kind: 'module', symbols: 1, lines: 5, overlay: {} },
    ],
    edges: [{ from: 'a', to: 'b' }, { from: 'b', to: 'a' }],
  };
  const g = moduleGraph(cyc);
  const { pos } = layoutModules(g.nodes, g.edges);
  assert.equal(pos.size, 2);
}

// The overlay, from `forge graph --json`: a module's cost and demotion
// count sum only the files grouped directly under it (the same rule
// `src/graph.rs`'s `group` uses for symbols/lines), a file elsewhere in
// the tree left out.
const OVERLAID = {
  nodes: [
    { path: 'src', kind: 'module', symbols: 2, lines: 8 },
    { path: 'src/a.rs', kind: 'file', symbols: 1, lines: 5, overlay: { tasks: [{ id: 9, at: 1, cost_usd: 1.5 }], demotions: [{ id: 3, at: 2, reason: 'off by one' }], repair_cost_usd: 0.25 } },
    { path: 'src/b.rs', kind: 'file', symbols: 1, lines: 3, overlay: { tasks: [], demotions: [], repair_cost_usd: 0 } },
    { path: 'other.rs', kind: 'file', symbols: 1, lines: 1, overlay: { tasks: [{ id: 4, at: 1, cost_usd: 100 }], demotions: [], repair_cost_usd: 0 } },
  ],
  edges: [],
};
const srcModule = OVERLAID.nodes[0];
assert.equal(nodeCost(srcModule, OVERLAID.nodes), 1.75, "1.5 + 0.25 from a.rs, b.rs contributes 0, other.rs is outside src");
assert.equal(nodeDemotions(srcModule, OVERLAID.nodes), 1);

const rendered = renderModuleGraph(OVERLAID);
// src (the module) and other.rs (a root-level file) display; a.rs/b.rs
// are collapsed into src, not shown as their own nodes.
assert.equal((rendered.svgHtml.match(/class="mnode"/g) || []).length, 2, rendered.svgHtml);
assert.match(rendered.svgHtml, /data-path="src"/);
assert.match(rendered.svgHtml, /data-path="other\.rs"/);
assert.match(rendered.svgHtml, />1<\/text>/, "the one-demotion badge on src");
assert.match(rendered.svgHtml, /tasks: 9/, "hovering src lists the task that touched a\\.rs");

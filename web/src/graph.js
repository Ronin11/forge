// Pure rendering for the /graph/modules page (docs/LATER.md, "The code
// visualiser"): no DOM, no fetch, so web/tests/graph.test.js can exercise
// it under node exactly like time.js and workflows.js — collapsing a
// `forge graph --json` document to its module nodes, laying them out in
// layers with no library, and drawing the SVG.
(function (root, factory) {
  const api = factory();
  if (typeof module === 'object' && module.exports) module.exports = api;
  else root.ForgeGraph = api;
})(globalThis, () => {
  const esc = s => String(s ?? '').replace(/[&<>"]/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));
  const usd = n => '$' + (Number(n) || 0).toFixed(2);

  function parentOf(path) {
    const i = path.lastIndexOf('/');
    return i === -1 ? null : path.slice(0, i);
  }

  // Collapses a `forge graph --json` document (file nodes grouped under
  // module nodes, `src/graph.rs`'s `group`) to the module level: every
  // module node, plus any file that joined no module (a root-level file);
  // edges re-pointed from a file to its display node, self-edges dropped,
  // duplicates deduped.
  function moduleGraph(graph) {
    const nodes = (graph && graph.nodes) || [];
    const modules = nodes.filter(n => n.kind === 'module');
    const moduleSet = new Set(modules.map(m => m.path));
    const displayOf = new Map();
    for (const n of nodes) {
      if (n.kind !== 'file') continue;
      const parent = parentOf(n.path);
      displayOf.set(n.path, parent && moduleSet.has(parent) ? parent : n.path);
    }
    const rootFiles = nodes.filter(n => n.kind === 'file' && displayOf.get(n.path) === n.path);
    const seen = new Set();
    const edges = [];
    for (const e of (graph && graph.edges) || []) {
      const from = displayOf.get(e.from) ?? e.from;
      const to = displayOf.get(e.to) ?? e.to;
      if (from === to || !from || !to) continue;
      const key = `${from}\u0000${to}`;
      if (seen.has(key)) continue;
      seen.add(key);
      edges.push({ from, to });
    }
    return { nodes: [...modules, ...rootFiles], edges };
  }

  // One node's own overlay cost (repair cost plus every touching task's
  // own cost), or, for a module, the sum over the files grouped directly
  // under it — the same "direct children only" rule `group` in
  // `src/graph.rs` uses for `symbols`/`lines`.
  function fileCost(n) {
    const o = n.overlay || {};
    return (o.tasks || []).reduce((s, t) => s + (t.cost_usd || 0), 0) + (o.repair_cost_usd || 0);
  }
  function nodeCost(node, allNodes) {
    if (node.kind === 'file') return fileCost(node);
    return allNodes.filter(n => n.kind === 'file' && parentOf(n.path) === node.path)
      .reduce((s, n) => s + fileCost(n), 0);
  }
  function nodeDemotions(node, allNodes) {
    if (node.kind === 'file') return ((node.overlay || {}).demotions || []).length;
    return allNodes.filter(n => n.kind === 'file' && parentOf(n.path) === node.path)
      .reduce((s, n) => s + ((n.overlay || {}).demotions || []).length, 0);
  }
  function nodeTasks(node, allNodes) {
    if (node.kind === 'file') return (node.overlay || {}).tasks || [];
    return allNodes.filter(n => n.kind === 'file' && parentOf(n.path) === node.path)
      .flatMap(n => (n.overlay || {}).tasks || []);
  }

  // A layered layout, no library: a node's layer is the longest path from
  // a root (a node no edge points into) — Kahn's algorithm run forward,
  // keeping the longest distance seen rather than stopping at the first.
  // A node only reachable through a cycle (no root reaches it) falls back
  // to layer 0. Deterministic: same graph, same picture, ties within a
  // layer broken on path.
  function layoutModules(nodes, edges) {
    const paths = new Set(nodes.map(n => n.path));
    const adj = new Map(nodes.map(n => [n.path, []]));
    const indeg = new Map(nodes.map(n => [n.path, 0]));
    for (const e of edges) {
      if (!paths.has(e.from) || !paths.has(e.to) || e.from === e.to) continue;
      adj.get(e.from).push(e.to);
      indeg.set(e.to, indeg.get(e.to) + 1);
    }
    const layer = new Map();
    const queue = nodes.filter(n => indeg.get(n.path) === 0).map(n => n.path);
    for (const p of queue) layer.set(p, 0);
    const remaining = new Map(indeg);
    for (let i = 0; i < queue.length; i++) {
      const p = queue[i];
      for (const to of adj.get(p)) {
        const next = layer.get(p) + 1;
        if (!layer.has(to) || layer.get(to) < next) layer.set(to, next);
        remaining.set(to, remaining.get(to) - 1);
        if (remaining.get(to) === 0) queue.push(to);
      }
    }
    for (const n of nodes) if (!layer.has(n.path)) layer.set(n.path, 0);

    const byLayer = new Map();
    for (const n of nodes) {
      const l = layer.get(n.path);
      if (!byLayer.has(l)) byLayer.set(l, []);
      byLayer.get(l).push(n);
    }
    const colGap = 220, rowGap = 64, pad = 50;
    const pos = new Map();
    let maxRows = 1;
    [...byLayer.keys()].sort((a, b) => a - b).forEach(l => {
      const row = byLayer.get(l).sort((a, b) => a.path.localeCompare(b.path));
      maxRows = Math.max(maxRows, row.length);
      row.forEach((n, i) => pos.set(n.path, { x: pad + l * colGap, y: pad + i * rowGap }));
    });
    const width = pad * 2 + Math.max(1, byLayer.size) * colGap;
    const height = pad * 2 + maxRows * rowGap;
    return { pos, width, height };
  }

  const radiusFor = lines => Math.max(7, Math.min(34, 7 + Math.sqrt(Math.max(0, Number(lines) || 0))));

  function costColor(cost, maxCost) {
    if (!cost || !maxCost) return 'var(--panel)';
    const t = Math.min(1, cost / maxCost);
    return `color-mix(in srgb, var(--bad) ${Math.round(t * 70)}%, var(--panel))`;
  }

  // A cubic bezier curving between two node centres, its control points
  // pulled to the horizontal midpoint — the standard "sankey" curve
  // shape, cheap to compute and reads well at any layer gap.
  function curve(a, b) {
    const midX = (a.x + b.x) / 2;
    return `M ${a.x} ${a.y} C ${midX} ${a.y} ${midX} ${b.y} ${b.x} ${b.y}`;
  }

  // A `forge graph --json` document (file nodes grouped under module
  // nodes) or an already module-shaped `{nodes, edges}`, to an SVG's
  // inner markup: nodes sized by lines, coloured by cost sunk
  // (`nodeCost`), badged with a demotion count, edges as curves.
  // `selected` highlights one node by path; `filterQuery` blanks anything
  // whose path doesn't match.
  function renderModuleGraph(graph, opts) {
    opts = opts || {};
    const allNodes = (graph && graph.nodes) || [];
    const hasFileOrModuleKinds = allNodes.some(n => n.kind === 'file' || n.kind === 'module');
    const g = hasFileOrModuleKinds ? moduleGraph(graph) : { nodes: allNodes, edges: (graph && graph.edges) || [] };
    const { pos, width, height } = layoutModules(g.nodes, g.edges);
    const maxCost = Math.max(0, ...g.nodes.map(n => nodeCost(n, allNodes)));
    const q = (opts.filterQuery || '').trim().toLowerCase();
    const shown = new Set(g.nodes.filter(n => !q || n.path.toLowerCase().includes(q)).map(n => n.path));

    const edgesSvg = g.edges
      .filter(e => pos.has(e.from) && pos.has(e.to) && shown.has(e.from) && shown.has(e.to))
      .map(e => `<path class="medge" d="${curve(pos.get(e.from), pos.get(e.to))}" fill="none" stroke="var(--run)" stroke-width="1.5" opacity="0.4" />`)
      .join('');

    const nodesSvg = g.nodes.filter(n => shown.has(n.path)).map(n => {
      const p = pos.get(n.path);
      const r = radiusFor(n.lines);
      const cost = nodeCost(n, allNodes);
      const demotions = nodeDemotions(n, allNodes);
      const tasks = nodeTasks(n, allNodes);
      const label = n.path.split('/').pop();
      const title = `${n.path} — ${n.lines || 0} line(s), ${n.symbols || 0} symbol(s)`
        + (cost ? `, ${usd(cost)} sunk` : '')
        + (demotions ? `, ${demotions} demotion(s)` : '')
        + (tasks.length ? ` — tasks: ${tasks.map(t => t.id).join(', ')}` : '');
      const badge = demotions
        ? `<circle cx="${r - 3}" cy="${-r + 3}" r="7" fill="var(--bad)" />
           <text x="${r - 3}" y="${-r + 6}" font-size="9" text-anchor="middle" fill="#fff">${demotions}</text>`
        : '';
      return `<g class="mnode" data-path="${esc(n.path)}" transform="translate(${p.x},${p.y})">
        <circle r="${r}" fill="${n.path === opts.selected ? 'var(--sel)' : costColor(cost, maxCost)}" stroke="var(--line)"></circle>
        <text y="${r + 12}" font-size="10" text-anchor="middle" fill="var(--fg)">${esc(label)}</text>
        ${badge}
        <title>${esc(title)}</title>
      </g>`;
    }).join('');

    return { svgHtml: edgesSvg + nodesSvg, width, height };
  }

  return {
    moduleGraph, layoutModules, radiusFor, nodeCost, nodeDemotions, nodeTasks,
    renderModuleGraph,
  };
});

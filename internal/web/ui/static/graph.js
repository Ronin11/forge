// The workflow graph editor and run view (workflow-edit.html,
// workflow-run.html). Hand-rolled SVG, same rules as app.js: no framework, no
// build step, behaviour attaches by data- attributes, colors come from the
// theme tokens via CSS classes. The wire shape is the API's WorkflowGraph —
// nodes [{id, type, config, position:{x,y}}], edges [{from, to, when, case,
// default, stack_on, loop, max_iterations}] — and serialization preserves
// every field the editor does not render, so CLI/TOML round-trips survive.
(function () {
  'use strict';

  var SVG = 'http://www.w3.org/2000/svg';
  var NODE_W = 180, NODE_H = 54;
  var TYPES = {
    routine: { label: 'Routine', icon: '#i-routines' },
    script: { label: 'JavaScript', icon: '#i-gv-js' },
    switch: { label: 'Switch', icon: '#i-gv-switch' },
    join: { label: 'Join', icon: '#i-gv-join' },
  };

  function fetchJSON(url, opts) {
    return fetch(url, opts).then(function (resp) {
      if (!resp.ok) return resp.json().then(function (e) { throw new Error(e.error || resp.status); });
      if (resp.status === 204) return null;
      return resp.json();
    });
  }

  // capturePointer keeps a drag alive outside the svg. Synthetic pointers
  // (tests, automation) have no capturable id — without capture the moves
  // still bubble to the svg listeners, so the drag works either way.
  function capturePointer(e) {
    try { document.querySelector('[data-gv-stage]').setPointerCapture(e.pointerId); } catch (err) { /* synthetic pointer */ }
  }

  function el(name, attrs) {
    var node = document.createElementNS(SVG, name);
    for (var k in attrs || {}) node.setAttribute(k, attrs[k]);
    return node;
  }

  // ---- graph model helpers -------------------------------------------------

  // edgeKey mirrors the engine's flow.EdgeKey: how a decision is recorded on
  // the target instance, and how the run view tells taken edges apart.
  function edgeKey(e) {
    var k = e.from + '|' + (e.when || '');
    if (e.default) return k + '|default';
    if (e.case) return k + '|' + e.case;
    return k;
  }

  function nodeById(graph, id) {
    for (var i = 0; i < graph.nodes.length; i++) if (graph.nodes[i].id === id) return graph.nodes[i];
    return null;
  }

  function edgesOf(graph) { return graph.edges || []; }

  // autoLayout is a small Sugiyama: layer by longest path over non-loop
  // edges, order rows by two barycenter sweeps, place on a fixed grid.
  function autoLayout(graph) {
    var nodes = graph.nodes, edges = edgesOf(graph).filter(function (e) { return !e.loop; });
    var depth = {}, order = {};
    nodes.forEach(function (n) { depth[n.id] = 0; });
    for (var pass = 0; pass < nodes.length; pass++) {
      var changed = false;
      edges.forEach(function (e) {
        if (depth[e.from] + 1 > (depth[e.to] || 0)) { depth[e.to] = depth[e.from] + 1; changed = true; }
      });
      if (!changed) break;
    }
    var cols = {};
    nodes.forEach(function (n) { (cols[depth[n.id]] = cols[depth[n.id]] || []).push(n); });
    nodes.forEach(function (n, i) { order[n.id] = i; });
    var incoming = {}, outgoing = {};
    edges.forEach(function (e) {
      (incoming[e.to] = incoming[e.to] || []).push(e.from);
      (outgoing[e.from] = outgoing[e.from] || []).push(e.to);
    });
    function sweep(byNeighbors) {
      Object.keys(cols).forEach(function (d) {
        cols[d].sort(function (a, b) {
          function bary(n) {
            var ns = byNeighbors[n.id] || [];
            if (!ns.length) return order[n.id];
            var sum = 0;
            ns.forEach(function (m) { sum += order[m]; });
            return sum / ns.length;
          }
          return bary(a) - bary(b);
        });
        cols[d].forEach(function (n, i) { order[n.id] = i; });
      });
    }
    sweep(incoming); sweep(outgoing); sweep(incoming);
    var maxRows = 0;
    Object.keys(cols).forEach(function (d) { maxRows = Math.max(maxRows, cols[d].length); });
    Object.keys(cols).forEach(function (d) {
      var offset = (maxRows - cols[d].length) / 2;
      cols[d].forEach(function (n, i) {
        n.position = { x: 40 + d * 250, y: 40 + (i + offset) * 100 };
      });
    });
  }

  function needsLayout(graph) {
    return graph.nodes.length > 0 && graph.nodes.every(function (n) {
      return !n.position || (!n.position.x && !n.position.y);
    });
  }

  // validateGraph mirrors the server's rules the editor can check early.
  // Errors block save; warnings do not.
  function validateGraph(graph, routineNames) {
    var out = [];
    function err(msg, ref) { out.push({ level: 'err', msg: msg, ref: ref }); }
    function warn(msg, ref) { out.push({ level: 'warn', msg: msg, ref: ref }); }
    var ids = {};
    if (!graph.nodes.length) err('at least one node is required');
    graph.nodes.forEach(function (n) {
      if (!/^[a-z0-9][a-z0-9-]{0,39}$/.test(n.id)) err('node "' + n.id + '": name must be a lower-case slug', { node: n.id });
      if (ids[n.id]) err('node "' + n.id + '" listed twice', { node: n.id });
      ids[n.id] = n.type;
      var cfg = n.config || {};
      if (n.type === 'routine') {
        if (!cfg.routine) err('node "' + n.id + '": pick a routine', { node: n.id });
        else if (routineNames && routineNames.length && routineNames.indexOf(cfg.routine) < 0) warn('node "' + n.id + '": routine "' + cfg.routine + '" is not in the list', { node: n.id });
      }
      if (n.type === 'script' && !(cfg.source || '').trim()) err('node "' + n.id + '": script body is empty', { node: n.id });
      if (n.type === 'switch' && !(cfg.expression || '').trim()) err('node "' + n.id + '": switch expression is empty', { node: n.id });
    });
    var seen = {};
    var casesBySwitch = {}, defaultsBySwitch = {};
    edgesOf(graph).forEach(function (e, i) {
      var ref = { edge: i };
      if (!ids[e.from] || !ids[e.to]) { err('edge ' + e.from + '→' + e.to + ' points at a missing node', ref); return; }
      var key = edgeKey(e) + '>' + e.to;
      if (seen[key]) err('duplicate edge ' + e.from + '→' + e.to, ref);
      seen[key] = true;
      var isCase = e.when === 'case' || e.default;
      if (ids[e.from] === 'switch') {
        if (!isCase && e.when !== 'failure') err('edge ' + e.from + '→' + e.to + ': a switch edge is a case, default, or failure', ref);
        if (e.when === 'case' && !e.default && !e.case) err('edge ' + e.from + '→' + e.to + ': set a case value (or make it the default)', ref);
        if (e.default) {
          defaultsBySwitch[e.from] = (defaultsBySwitch[e.from] || 0) + 1;
          if (defaultsBySwitch[e.from] > 1) err('switch "' + e.from + '" has two default edges', ref);
        } else if (e.when === 'case') {
          var seenCase = (casesBySwitch[e.from] = casesBySwitch[e.from] || {});
          if (e.case && seenCase[e.case]) err('switch "' + e.from + '": case "' + e.case + '" listed twice', ref);
          seenCase[e.case] = true;
        }
      } else if (isCase) {
        err('edge ' + e.from + '→' + e.to + ': case edges only leave a switch', ref);
      }
      if (ids[e.from] === 'join' && e.when === 'failure') err('edge ' + e.from + '→' + e.to + ': a join cannot fail', ref);
      if (e.stack_on && e.when && e.when !== 'success') err('edge ' + e.from + '→' + e.to + ': stack_on needs a success edge', ref);
      if (e.loop && !(e.max_iterations >= 1 && e.max_iterations <= 20)) err('loop edge ' + e.from + '→' + e.to + ': max_iterations 1..20 is required', ref);
      if (!e.loop && e.max_iterations) err('edge ' + e.from + '→' + e.to + ': max_iterations only applies to loop edges', ref);
    });
    graph.nodes.forEach(function (n) {
      if (n.type !== 'switch') return;
      if (!casesBySwitch[n.id] && !defaultsBySwitch[n.id]) err('switch "' + n.id + '" has no case edges', { node: n.id });
    });
    graph.nodes.forEach(function (n) {
      if (n.type !== 'join') return;
      var count = edgesOf(graph).filter(function (e) { return e.to === n.id && !e.loop; }).length;
      if (count < 2) err('join "' + n.id + '" fans in ' + count + ' edge(s); it needs at least 2', { node: n.id });
    });
    // Cycles: the graph minus loop edges must be a DAG.
    var indeg = {}, next = {};
    graph.nodes.forEach(function (n) { indeg[n.id] = 0; });
    edgesOf(graph).forEach(function (e) {
      if (e.loop || !ids[e.from] || !ids[e.to]) return;
      indeg[e.to]++;
      (next[e.from] = next[e.from] || []).push(e.to);
    });
    var queue = Object.keys(indeg).filter(function (id) { return !indeg[id]; });
    var visited = 0;
    while (queue.length) {
      var id = queue.shift();
      visited++;
      (next[id] || []).forEach(function (to) { if (!--indeg[to]) queue.push(to); });
    }
    if (graph.nodes.length && visited < graph.nodes.length) {
      Object.keys(indeg).forEach(function (id) {
        if (indeg[id] > 0) err('cycle through "' + id + '" — mark the intentional back-edge as a loop with max iterations', { node: id });
      });
    }
    if (graph.nodes.length && !graph.nodes.some(function (n) {
      return !edgesOf(graph).some(function (e) { return e.to === n.id && !e.loop; });
    })) err('no root node: something must start the run');
    // Reachability warning.
    var reach = {};
    var roots = graph.nodes.filter(function (n) {
      return !edgesOf(graph).some(function (e) { return e.to === n.id && !e.loop; });
    }).map(function (n) { return n.id; });
    var stack = roots.slice();
    while (stack.length) {
      var cur = stack.pop();
      if (reach[cur]) continue;
      reach[cur] = true;
      edgesOf(graph).forEach(function (e) { if (e.from === cur) stack.push(e.to); });
    }
    graph.nodes.forEach(function (n) { if (!reach[n.id]) warn('node "' + n.id + '" is unreachable', { node: n.id }); });
    return out;
  }

  // ---- stage: SVG construction, viewport, rendering ------------------------

  function subtitleOf(n) {
    var cfg = n.config || {};
    if (n.type === 'routine') return cfg.routine || '(no routine)';
    if (n.type === 'script') return (cfg.source || '').split('\n')[0].slice(0, 26) || '(empty)';
    if (n.type === 'switch') return (cfg.expression || '').slice(0, 26) || '(no expression)';
    if (n.type === 'join') return (cfg.mode === 'any' ? 'any' : 'all');
    return '';
  }

  // ports returns [{kind, x, y}] in node-local coordinates. `kind` names the
  // edge the port starts: success | failure | case | out (join) | in.
  function portsOf(n) {
    var out = [{ kind: 'in', x: 0, y: NODE_H / 2 }];
    if (n.type === 'routine' || n.type === 'script') {
      out.push({ kind: 'success', x: NODE_W, y: NODE_H / 2 - 12 });
      out.push({ kind: 'failure', x: NODE_W, y: NODE_H / 2 + 12 });
    } else if (n.type === 'switch') {
      out.push({ kind: 'case', x: NODE_W, y: NODE_H / 2 - 12 });
      out.push({ kind: 'failure', x: NODE_W, y: NODE_H / 2 + 12 });
    } else {
      out.push({ kind: 'success', x: NODE_W, y: NODE_H / 2 });
    }
    return out;
  }

  function portFor(n, e) {
    var kind = 'success';
    if (e.when === 'failure') kind = 'failure';
    else if (e.when === 'case' || e.default) kind = 'case';
    var ps = portsOf(n);
    for (var i = 0; i < ps.length; i++) if (ps[i].kind === kind) return ps[i];
    return ps[ps.length - 1];
  }

  function edgeClass(e) {
    var cls = 'gv-edge';
    if (e.when === 'failure') cls += ' gv-e-failure';
    else if (e.when === 'case' || e.default) cls += ' gv-e-case';
    else if (e.when === 'always') cls += ' gv-e-always';
    else cls += ' gv-e-success';
    if (e.loop) cls += ' gv-e-loop';
    return cls;
  }

  function edgePath(a, b, loop) {
    if (loop) {
      // A back-edge dips below both nodes so it reads as a loop.
      var dipY = Math.max(a.y, b.y) + 90;
      return 'M' + a.x + ',' + a.y + ' C' + (a.x + 60) + ',' + dipY + ' ' + (b.x - 60) + ',' + dipY + ' ' + b.x + ',' + b.y;
    }
    var dx = Math.max(40, Math.abs(b.x - a.x) / 2);
    return 'M' + a.x + ',' + a.y + ' C' + (a.x + dx) + ',' + a.y + ' ' + (b.x - dx) + ',' + b.y + ' ' + b.x + ',' + b.y;
  }

  function createStage(svg, opts) {
    var view = { x: 40, y: 20, k: 1 };
    var defs = el('defs');
    ['gv-arrow'].forEach(function (id) {
      var m = el('marker', { id: id, viewBox: '0 0 10 10', refX: 9, refY: 5, markerWidth: 7, markerHeight: 7, orient: 'auto-start-reverse' });
      var p = el('path', { d: 'M0,0 L10,5 L0,10 z' });
      p.setAttribute('fill', 'context-stroke');
      m.appendChild(p);
      defs.appendChild(m);
    });
    svg.appendChild(defs);
    var pattern = el('pattern', { id: 'gv-dots', width: 24, height: 24, patternUnits: 'userSpaceOnUse' });
    pattern.appendChild(el('circle', { cx: 1, cy: 1, r: 1, class: 'gv-grid' }));
    defs.appendChild(pattern);
    var bg = el('rect', { x: 0, y: 0, width: '100%', height: '100%', fill: 'url(#gv-dots)', 'data-gv-bg': '' });
    svg.appendChild(bg);
    var viewport = el('g');
    var edgeLayer = el('g'), nodeLayer = el('g'), overlay = el('g');
    viewport.appendChild(edgeLayer);
    viewport.appendChild(nodeLayer);
    viewport.appendChild(overlay);
    svg.appendChild(viewport);

    function apply() {
      viewport.setAttribute('transform', 'translate(' + view.x + ' ' + view.y + ') scale(' + view.k + ')');
    }
    apply();

    function toWorld(clientX, clientY) {
      var r = svg.getBoundingClientRect();
      return { x: (clientX - r.left - view.x) / view.k, y: (clientY - r.top - view.y) / view.k };
    }

    svg.addEventListener('wheel', function (e) {
      e.preventDefault();
      var delta = e.deltaY * (e.deltaMode === 1 ? 16 : 1); // Firefox sends lines
      var factor = Math.exp(-delta * 0.0012);
      var k = Math.min(2.5, Math.max(0.25, view.k * factor));
      var r = svg.getBoundingClientRect();
      var cx = e.clientX - r.left, cy = e.clientY - r.top;
      view.x = cx - (cx - view.x) * (k / view.k);
      view.y = cy - (cy - view.y) * (k / view.k);
      view.k = k;
      apply();
    }, { passive: false });

    // Pan: drag the background.
    bg.addEventListener('pointerdown', function (e) {
      if (opts.onBackgroundClick && !e.shiftKey) opts.onBackgroundClick(e);
      var sx = e.clientX, sy = e.clientY, ox = view.x, oy = view.y;
      svg.classList.add('gv-panning');
      bg.setPointerCapture(e.pointerId);
      function move(ev) { view.x = ox + ev.clientX - sx; view.y = oy + ev.clientY - sy; apply(); }
      function up(ev) {
        bg.removeEventListener('pointermove', move);
        bg.removeEventListener('pointerup', up);
        svg.classList.remove('gv-panning');
      }
      bg.addEventListener('pointermove', move);
      bg.addEventListener('pointerup', up);
    });

    function zoomFit(graph) {
      if (!graph.nodes.length) { view.x = 40; view.y = 20; view.k = 1; apply(); return; }
      var minX = Infinity, minY = Infinity, maxX = -Infinity, maxY = -Infinity;
      graph.nodes.forEach(function (n) {
        var p = n.position || { x: 0, y: 0 };
        minX = Math.min(minX, p.x); minY = Math.min(minY, p.y);
        maxX = Math.max(maxX, p.x + NODE_W); maxY = Math.max(maxY, p.y + NODE_H + 40);
      });
      var r = svg.getBoundingClientRect();
      var k = Math.min(2.5, Math.max(0.25, Math.min(r.width / (maxX - minX + 120), r.height / (maxY - minY + 120))));
      view.k = k;
      view.x = (r.width - (maxX - minX) * k) / 2 - minX * k;
      view.y = (r.height - (maxY - minY) * k) / 2 - minY * k;
      apply();
    }

    return {
      svg: svg, edgeLayer: edgeLayer, nodeLayer: nodeLayer, overlay: overlay,
      toWorld: toWorld, zoomFit: zoomFit, view: view, apply: apply,
    };
  }

  // renderGraph redraws every node and edge. Full redraws are fine at the
  // graph sizes the server allows (≤ 100 nodes).
  function renderGraph(stage, graph, o) {
    o = o || {};
    stage.edgeLayer.textContent = '';
    stage.nodeLayer.textContent = '';
    edgesOf(graph).forEach(function (e, i) {
      var from = nodeById(graph, e.from), to = nodeById(graph, e.to);
      if (!from || !to) return;
      var pf = portFor(from, e), pi = { x: 0, y: NODE_H / 2 };
      var a = { x: (from.position || {}).x + pf.x, y: (from.position || {}).y + pf.y };
      var b = { x: (to.position || {}).x + pi.x, y: (to.position || {}).y + pi.y };
      var d = edgePath(a, b, e.loop);
      var g = el('g');
      var cls = edgeClass(e) + (o.selectedEdge === i ? ' gv-selected' : '') + (o.takenEdges && o.takenEdges[i] ? ' gv-taken' : '');
      var path = el('path', { d: d, class: cls, 'marker-end': 'url(#gv-arrow)' });
      g.appendChild(path);
      if (!o.readOnly) {
        var hit = el('path', { d: d, class: 'gv-edge-hit' });
        hit.addEventListener('pointerdown', function (ev) { ev.stopPropagation(); if (o.onSelectEdge) o.onSelectEdge(i); });
        g.appendChild(hit);
      }
      var label = '';
      if (e.default) label = 'default';
      else if (e.when === 'case') label = e.case || 'case?';
      if (e.loop) label = (label ? label + ' ' : '') + '↺ ' + (e.max_iterations || '?');
      if (e.stack_on) label = (label ? label + ' ' : '') + 'stacked';
      if (label) {
        var mx = (a.x + b.x) / 2, my = e.loop ? Math.max(a.y, b.y) + 78 : (a.y + b.y) / 2 - 6;
        var t = el('text', { x: mx, y: my, 'text-anchor': 'middle', class: 'gv-edge-label' });
        t.textContent = label;
        g.appendChild(t);
      }
      stage.edgeLayer.appendChild(g);
    });
    graph.nodes.forEach(function (n) {
      var p = n.position || { x: 0, y: 0 };
      var cls = 'gv-node gv-t-' + n.type;
      if (o.selectedNode === n.id) cls += ' gv-selected';
      if (o.nodeState) cls += ' gv-st-' + (o.nodeState(n) || 'none');
      var g = el('g', { class: cls, transform: 'translate(' + p.x + ' ' + p.y + ')', tabindex: 0 });
      g.setAttribute('data-node', n.id);
      g.appendChild(el('rect', { class: 'gv-body', width: NODE_W, height: NODE_H, rx: 8 }));
      var icon = el('use', { href: (TYPES[n.type] || {}).icon || '#i-routines', x: 10, y: 10, width: 16, height: 16 });
      icon.setAttribute('class', 'gv-icon');
      g.appendChild(icon);
      var title = el('text', { x: 34, y: 22 });
      title.textContent = n.id;
      g.appendChild(title);
      var sub = el('text', { x: 34, y: 40, class: 'gv-sub' });
      sub.textContent = subtitleOf(n);
      g.appendChild(sub);
      if (o.decorate) o.decorate(g, n);
      portsOf(n).forEach(function (port) {
        if (o.readOnly) return;
        var hit = el('circle', { cx: port.x, cy: port.y, r: 13, class: 'gv-port-hit' });
        hit.setAttribute('data-port', port.kind);
        g.appendChild(hit);
        var dot = el('circle', { cx: port.x, cy: port.y, r: 5, class: 'gv-port gv-p-' + port.kind });
        dot.setAttribute('data-port', port.kind);
        g.appendChild(dot);
      });
      if (o.onNode) o.onNode(g, n);
      stage.nodeLayer.appendChild(g);
    });
  }

  // ---- the editor ----------------------------------------------------------

  var editorRoot = document.querySelector('[data-graph-editor]');
  if (editorRoot) initEditor(editorRoot);

  function initEditor(root) {
    var name = root.dataset.workflowName || '';
    var svg = root.querySelector('[data-gv-stage]');
    var panel = root.querySelector('[data-gv-panel]');
    var errorBox = document.querySelector('[data-gv-error]');
    var lintBox = document.querySelector('[data-gv-lint]');
    var rawBox = document.querySelector('[data-gv-raw]');
    var stage = createStage(svg, { onBackgroundClick: function () { select(null); } });

    var current = null; // the fetched workflow object; null for a new one
    var graph = { nodes: [], edges: [] };
    var routineNames = [];
    var selection = null; // {node: id} | {edge: index}
    var history = [], future = [];
    var armedType = null; // palette tile clicked, waiting for a canvas click

    function pageError(err) {
      errorBox.hidden = false;
      errorBox.textContent = 'Refused: ' + String(err.message || err).replace(/: (conflict|not found|draining)$/, '');
    }
    function clearError() { errorBox.hidden = true; }

    function snapshot() { return JSON.stringify(graph); }
    function pushHistory() {
      history.push(snapshot());
      if (history.length > 100) history.shift();
      future = [];
    }
    function mutate(fn) {
      pushHistory();
      fn();
      afterChange();
    }
    function afterChange() {
      render();
      lint();
      syncRaw();
    }

    function render() {
      renderGraph(stage, graph, {
        selectedNode: selection && selection.node,
        selectedEdge: selection && selection.edge,
        onSelectEdge: function (i) { select({ edge: i }); },
        onNode: bindNode,
      });
    }

    function lint() {
      var findings = validateGraph(graph, routineNames);
      lintBox.textContent = '';
      lintBox.hidden = findings.length === 0;
      findings.forEach(function (f) {
        var li = document.createElement('li');
        li.className = f.level === 'err' ? 'gv-err' : 'gv-warn';
        li.textContent = f.msg;
        if (f.ref) li.addEventListener('click', function () {
          if (f.ref.node) select({ node: f.ref.node }); else if (f.ref.edge !== undefined) select({ edge: f.ref.edge });
        });
        lintBox.appendChild(li);
      });
      return findings;
    }

    function syncRaw() {
      if (rawBox && document.activeElement !== rawBox) rawBox.value = JSON.stringify(graph, null, 2);
    }

    function select(sel) {
      selection = sel;
      render();
      renderPanel();
    }

    function freshNodeID(type) {
      var base = type, i = 1, id = base;
      while (nodeById(graph, id)) id = base + '-' + (++i);
      return id;
    }

    function addNode(type, pos) {
      var id = freshNodeID(type);
      var config = {};
      if (type === 'script') config.source = 'function main(input) {\n  return {};\n}';
      if (type === 'switch') config.expression = '';
      if (type === 'join') config.mode = 'all';
      mutate(function () {
        graph.nodes.push({ id: id, type: type, config: config, position: { x: Math.round(pos.x / 8) * 8, y: Math.round(pos.y / 8) * 8 } });
      });
      select({ node: id });
    }

    // ---- node interactions: drag to move, ports to connect ----

    function bindNode(g, n) {
      g.addEventListener('pointerdown', function (e) {
        var portKind = e.target.getAttribute && e.target.getAttribute('data-port');
        if (portKind && portKind !== 'in') { startConnect(e, n, portKind); return; }
        startDrag(e, g, n);
      });
      g.addEventListener('keydown', function (e) {
        var d = { ArrowLeft: [-8, 0], ArrowRight: [8, 0], ArrowUp: [0, -8], ArrowDown: [0, 8] }[e.key];
        if (!d) return;
        e.preventDefault();
        mutate(function () { n.position.x += d[0]; n.position.y += d[1]; });
        select({ node: n.id });
      });
    }

    // Listeners live on the svg, not the node group: selecting re-renders and
    // replaces every node element mid-drag, but the svg survives.
    function startDrag(e, g, n) {
      e.stopPropagation();
      select({ node: n.id });
      var start = stage.toWorld(e.clientX, e.clientY);
      var ox = n.position.x, oy = n.position.y;
      var moved = false;
      capturePointer(e);
      function move(ev) {
        var w = stage.toWorld(ev.clientX, ev.clientY);
        if (!moved && (Math.abs(w.x - start.x) > 3 || Math.abs(w.y - start.y) > 3)) {
          moved = true;
          pushHistory();
        }
        if (!moved) return;
        n.position.x = Math.round((ox + w.x - start.x) / 8) * 8;
        n.position.y = Math.round((oy + w.y - start.y) / 8) * 8;
        render();
      }
      function up() {
        svg.removeEventListener('pointermove', move);
        svg.removeEventListener('pointerup', up);
        if (moved) {
          afterChange();
          savePositions();
        }
      }
      svg.addEventListener('pointermove', move);
      svg.addEventListener('pointerup', up);
    }

    // Drag-only changes persist through the layout PATCH: no generation bump,
    // no conflict against a real edit. New workflows just keep positions in
    // the model until Save.
    var layoutTimer = null;
    function savePositions() {
      if (!current) return;
      window.clearTimeout(layoutTimer);
      layoutTimer = window.setTimeout(function () {
        var positions = {};
        graph.nodes.forEach(function (n) { positions[n.id] = { x: n.position.x, y: n.position.y }; });
        fetch('/api/v1/workflows/' + encodeURIComponent(name) + '/layout', {
          method: 'PATCH', headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ positions: positions }),
        }).catch(function () { /* the Save button still carries positions */ });
      }, 600);
    }

    function startConnect(e, from, portKind) {
      e.stopPropagation();
      var fromPos = portFor(from, portKind === 'failure' ? { when: 'failure' } : portKind === 'case' ? { when: 'case' } : {});
      var a = { x: from.position.x + fromPos.x, y: from.position.y + fromPos.y };
      var temp = el('path', { class: 'gv-temp-edge' });
      stage.overlay.appendChild(temp);
      capturePointer(e);
      svg.querySelectorAll('.gv-port.gv-p-in').forEach(function (p) { p.classList.add('gv-p-open'); });
      function move(ev) {
        var w = stage.toWorld(ev.clientX, ev.clientY);
        temp.setAttribute('d', edgePath(a, w, false));
      }
      function up(ev) {
        svg.removeEventListener('pointermove', move);
        svg.removeEventListener('pointerup', up);
        temp.remove();
        svg.querySelectorAll('.gv-p-open').forEach(function (p) { p.classList.remove('gv-p-open'); });
        // Pointer capture retargets the event to the svg; hit-test the drop
        // point instead of trusting ev.target.
        var under = document.elementFromPoint(ev.clientX, ev.clientY);
        var targetG = under && under.closest && under.closest('.gv-node');
        if (!targetG) return;
        var to = targetG.getAttribute('data-node');
        if (!to || to === from.id) return;
        connect(from, to, portKind);
      }
      svg.addEventListener('pointermove', move);
      svg.addEventListener('pointerup', up);
    }

    function connect(from, to, portKind) {
      var e = { from: from.id, to: to };
      if (portKind === 'failure') e.when = 'failure';
      else if (portKind === 'case') { e.when = 'case'; e.case = ''; }
      else e.when = 'success';
      var dup = edgesOf(graph).some(function (x) { return edgeKey(x) === edgeKey(e) && x.to === e.to; });
      if (dup) { pageError(new Error('that edge already exists')); return; }
      // A connection that closes a cycle is meant as a loop: declare it, with
      // a small default cap the panel can adjust.
      if (createsCycle(e)) { e.loop = true; e.max_iterations = 3; }
      mutate(function () {
        graph.edges = edgesOf(graph).concat([e]);
      });
      select({ edge: graph.edges.length - 1 });
      clearError();
    }

    function createsCycle(candidate) {
      // Is `from` reachable from `to` over non-loop edges?
      var stack = [candidate.to], seen = {};
      while (stack.length) {
        var cur = stack.pop();
        if (cur === candidate.from) return true;
        if (seen[cur]) continue;
        seen[cur] = true;
        edgesOf(graph).forEach(function (e) { if (!e.loop && e.from === cur) stack.push(e.to); });
      }
      return false;
    }

    function deleteSelection() {
      if (!selection) return;
      if (selection.node) {
        var id = selection.node;
        mutate(function () {
          graph.nodes = graph.nodes.filter(function (n) { return n.id !== id; });
          graph.edges = edgesOf(graph).filter(function (e) { return e.from !== id && e.to !== id; });
        });
      } else if (selection.edge !== undefined) {
        var i = selection.edge;
        mutate(function () { graph.edges.splice(i, 1); });
      }
      select(null);
    }

    // ---- the config panel ----

    function field(labelText, input) {
      var label = document.createElement('label');
      label.textContent = labelText;
      panel.appendChild(label);
      panel.appendChild(input);
      return input;
    }
    function textInput(value, oninput) {
      var input = document.createElement('input');
      input.value = value || '';
      input.addEventListener('input', function () { oninput(input.value); });
      return input;
    }

    // Panel edits mutate config live but push one history entry per focus.
    var panelDirty = false;
    function panelMutate(fn) {
      if (!panelDirty) { pushHistory(); panelDirty = true; }
      fn();
      render();
      lint();
      syncRaw();
    }
    panel.addEventListener('focusout', function () { panelDirty = false; });

    function renderPanel() {
      panel.textContent = '';
      panelDirty = false;
      if (selection && selection.node) return renderNodePanel(nodeById(graph, selection.node));
      if (selection && selection.edge !== undefined) return renderEdgePanel(selection.edge);
      renderWorkflowPanel();
    }

    function renderWorkflowPanel() {
      var h = document.createElement('h3');
      h.textContent = current ? 'Workflow ' + name : 'New workflow';
      panel.appendChild(h);
      if (!current) {
        var nameInput = field('Name (lower-case slug)', textInput(pendingName, function (v) { pendingName = v.trim(); }));
        nameInput.pattern = '[a-z0-9][a-z0-9-]{0,39}';
        nameInput.placeholder = 'my-workflow';
      }
      var sched = field('Schedule (cron, blank = none)', textInput(pendingSchedule, function (v) { pendingSchedule = v.trim(); }));
      sched.placeholder = '0 3 * * *';
      var check = document.createElement('label');
      check.className = 'check';
      var box = document.createElement('input');
      box.type = 'checkbox';
      box.checked = pendingScheduleEnabled;
      box.addEventListener('change', function () { pendingScheduleEnabled = box.checked; });
      check.appendChild(box);
      check.appendChild(document.createTextNode(' Schedule enabled'));
      panel.appendChild(check);
      var hint = document.createElement('p');
      hint.className = 'hint';
      hint.textContent = 'Click a node or edge to configure it. Drag from a colored port to connect: green = on success, red = on failure, purple = a switch case.';
      panel.appendChild(hint);
    }

    function renderNodePanel(n) {
      if (!n) return renderWorkflowPanel();
      var h = document.createElement('h3');
      h.textContent = (TYPES[n.type] || {}).label + ' node';
      panel.appendChild(h);
      var idInput = field('Name', textInput(n.id, function (v) {
        panelMutate(function () { renameNode(n, v.trim()); });
      }));
      idInput.pattern = '[a-z0-9][a-z0-9-]{0,39}';
      n.config = n.config || {};
      if (n.type === 'routine') {
        var r = field('Routine', textInput(n.config.routine, function (v) { panelMutate(function () { n.config.routine = v.trim(); }); }));
        r.setAttribute('list', 'gv-routine-names');
        var objective = document.createElement('textarea');
        objective.rows = 3;
        objective.value = n.config.objective || '';
        objective.placeholder = 'Optional objective. {{steps.<node>.output.<path>}} and {{run.objective}} expand at run time.';
        objective.addEventListener('input', function () { panelMutate(function () { n.config.objective = objective.value; }); });
        field('Objective (optional)', objective);
        var personaInput = field('Persona (blank = the routine’s own)', textInput(n.config.persona, function (v) {
          panelMutate(function () {
            if (v.trim()) n.config.persona = v.trim(); else delete n.config.persona;
          });
        }));
        personaInput.placeholder = 'from ~/.forge/prompts';
        var repos = field('Repositories (comma-separated, blank = run default)', textInput((n.config.repositories || []).join(', '), function (v) {
          panelMutate(function () {
            var list = v.split(',').map(function (s) { return s.trim(); }).filter(Boolean);
            if (list.length) n.config.repositories = list; else delete n.config.repositories;
          });
        }));
        repos.placeholder = 'repo-a, repo-b';
      } else if (n.type === 'script') {
        var src = document.createElement('textarea');
        src.rows = 14;
        src.value = n.config.source || '';
        src.addEventListener('input', function () { panelMutate(function () { n.config.source = src.value; }); });
        src.addEventListener('keydown', function (e) {
          if (e.key !== 'Tab') return;
          e.preventDefault();
          var s = src.selectionStart;
          src.value = src.value.slice(0, s) + '  ' + src.value.slice(src.selectionEnd);
          src.selectionStart = src.selectionEnd = s + 2;
          src.dispatchEvent(new Event('input'));
        });
        field('function main(input) — the return value becomes the node output', src);
        var timeout = field('Timeout ms (default 5000, max 30000)', textInput(n.config.timeout_ms || '', function (v) {
          panelMutate(function () {
            var ms = parseInt(v, 10);
            if (ms > 0) n.config.timeout_ms = ms; else delete n.config.timeout_ms;
          });
        }));
        timeout.type = 'number';
        var hint = document.createElement('p');
        hint.className = 'hint';
        hint.textContent = 'input.run = {id, workflow, objective, repositories}; input.steps.<node> = {status, state, summary, output}. Sandboxed: no filesystem, no network.';
        panel.appendChild(hint);
      } else if (n.type === 'switch') {
        var expr = field('Expression (its String() value picks the case edge)', textInput(n.config.expression, function (v) { panelMutate(function () { n.config.expression = v; }); }));
        expr.placeholder = 'input.steps.triage.output.kind';
        var hint2 = document.createElement('p');
        hint2.className = 'hint';
        hint2.textContent = 'Draw one purple edge per case and set each value on the edge; mark one edge default for everything else.';
        panel.appendChild(hint2);
      } else if (n.type === 'join') {
        var sel = document.createElement('select');
        ['all', 'any'].forEach(function (m) {
          var o = document.createElement('option');
          o.value = m;
          o.textContent = m === 'all' ? 'all — wait for every branch' : 'any — first branch wins';
          sel.appendChild(o);
        });
        sel.value = n.config.mode === 'any' ? 'any' : 'all';
        sel.addEventListener('change', function () { panelMutate(function () { n.config.mode = sel.value; }); });
        field('Mode', sel);
      }
      var del = document.createElement('button');
      del.className = 'btn gv-danger';
      del.textContent = 'Delete node';
      del.addEventListener('click', deleteSelection);
      panel.appendChild(del);
    }

    function renameNode(n, newID) {
      if (!newID || newID === n.id) return;
      var old = n.id;
      n.id = newID;
      edgesOf(graph).forEach(function (e) {
        if (e.from === old) e.from = newID;
        if (e.to === old) e.to = newID;
      });
      selection = { node: newID };
    }

    function renderEdgePanel(i) {
      var e = edgesOf(graph)[i];
      if (!e) return renderWorkflowPanel();
      var from = nodeById(graph, e.from);
      var h = document.createElement('h3');
      h.textContent = 'Edge ' + e.from + ' → ' + e.to;
      panel.appendChild(h);
      var isSwitch = from && from.type === 'switch';
      var kinds = isSwitch ? ['case', 'default', 'failure'] : (from && from.type === 'join' ? ['success', 'always'] : ['success', 'failure', 'always']);
      var sel = document.createElement('select');
      kinds.forEach(function (k) {
        var o = document.createElement('option');
        o.value = k;
        o.textContent = { success: 'on success', failure: 'on failure', always: 'always (either way)', case: 'case (value below)', default: 'default (no case matched)' }[k];
        sel.appendChild(o);
      });
      sel.value = e.default ? 'default' : (e.when || 'success');
      sel.addEventListener('change', function () {
        panelMutate(function () {
          delete e.default;
          if (sel.value === 'default') { e.default = true; e.when = 'case'; delete e.case; }
          else { e.when = sel.value; if (sel.value !== 'case') delete e.case; }
          if (e.when !== 'success') delete e.stack_on;
        });
        renderPanel();
      });
      field('Taken when', sel);
      if (isSwitch && e.when === 'case' && !e.default) {
        field('Case value', textInput(e.case, function (v) { panelMutate(function () { e.case = v.trim(); }); }));
      }
      if (!isSwitch && (!e.when || e.when === 'success') && from && from.type === 'routine' && (nodeById(graph, e.to) || {}).type === 'routine') {
        var stack = document.createElement('label');
        stack.className = 'check';
        var sbox = document.createElement('input');
        sbox.type = 'checkbox';
        sbox.checked = !!e.stack_on;
        sbox.addEventListener('change', function () { panelMutate(function () { if (sbox.checked) e.stack_on = true; else delete e.stack_on; }); });
        stack.appendChild(sbox);
        stack.appendChild(document.createTextNode(' Stack: start on the upstream branch before it merges'));
        panel.appendChild(stack);
      }
      var loop = document.createElement('label');
      loop.className = 'check';
      var lbox = document.createElement('input');
      lbox.type = 'checkbox';
      lbox.checked = !!e.loop;
      lbox.addEventListener('change', function () {
        panelMutate(function () {
          if (lbox.checked) { e.loop = true; e.max_iterations = e.max_iterations || 3; }
          else { delete e.loop; delete e.max_iterations; }
        });
        renderPanel();
      });
      loop.appendChild(lbox);
      loop.appendChild(document.createTextNode(' Loop (back-edge: re-enters the target)'));
      panel.appendChild(loop);
      if (e.loop) {
        var cap = field('Max iterations (1..20)', textInput(e.max_iterations, function (v) {
          panelMutate(function () { e.max_iterations = parseInt(v, 10) || 0; });
        }));
        cap.type = 'number';
        cap.min = 1;
        cap.max = 20;
      }
      var del = document.createElement('button');
      del.className = 'btn gv-danger';
      del.textContent = 'Delete edge';
      del.addEventListener('click', deleteSelection);
      panel.appendChild(del);
    }

    // ---- palette ----

    root.querySelectorAll('[data-gv-add]').forEach(function (tile) {
      tile.addEventListener('dragstart', function (e) {
        e.dataTransfer.setData('text/gv-node-type', tile.dataset.gvAdd);
        e.dataTransfer.effectAllowed = 'copy';
      });
      tile.addEventListener('click', function () {
        var arm = armedType === tile.dataset.gvAdd ? null : tile.dataset.gvAdd;
        armedType = arm;
        root.querySelectorAll('.gv-tile').forEach(function (t) { t.classList.toggle('gv-armed', t === tile && !!arm); });
      });
    });
    svg.addEventListener('dragover', function (e) { e.preventDefault(); e.dataTransfer.dropEffect = 'copy'; });
    svg.addEventListener('drop', function (e) {
      e.preventDefault();
      var type = e.dataTransfer.getData('text/gv-node-type');
      if (TYPES[type]) addNode(type, stage.toWorld(e.clientX, e.clientY));
    });
    svg.addEventListener('click', function (e) {
      if (!armedType) return;
      if (e.target.closest('.gv-node')) return;
      addNode(armedType, stage.toWorld(e.clientX, e.clientY));
      armedType = null;
      root.querySelectorAll('.gv-tile').forEach(function (t) { t.classList.remove('gv-armed'); });
    });

    // ---- keyboard, undo, toolbar ----

    document.addEventListener('keydown', function (e) {
      var inField = /^(INPUT|TEXTAREA|SELECT)$/.test(document.activeElement.tagName);
      if ((e.key === 'Delete' || e.key === 'Backspace') && !inField && selection) {
        e.preventDefault();
        deleteSelection();
      }
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'z' && !inField) {
        e.preventDefault();
        if (e.shiftKey) redo(); else undo();
      }
      if (e.key === 'Escape') select(null);
    });

    function undo() {
      if (!history.length) return;
      future.push(snapshot());
      graph = JSON.parse(history.pop());
      selection = null;
      afterChange();
      renderPanel();
    }
    function redo() {
      if (!future.length) return;
      history.push(snapshot());
      graph = JSON.parse(future.pop());
      selection = null;
      afterChange();
      renderPanel();
    }
    document.querySelector('[data-gv-undo]').addEventListener('click', undo);
    document.querySelector('[data-gv-redo]').addEventListener('click', redo);
    document.querySelector('[data-gv-layout]').addEventListener('click', function () {
      mutate(function () { autoLayout(graph); });
      stage.zoomFit(graph);
      savePositions();
    });
    document.querySelector('[data-gv-fit]').addEventListener('click', function () { stage.zoomFit(graph); });

    // Draft with AI: describe the workflow, get a graph back to refine. The
    // current graph rides along so a description over an existing graph is an
    // edit; the result replaces the model (undo restores the previous state)
    // and nothing is saved until the human hits Save.
    var draftBtn = document.querySelector('[data-gv-draft]');
    if (draftBtn) draftBtn.addEventListener('click', function () {
      var text = document.querySelector('[data-gv-draft-text]');
      var notes = document.querySelector('[data-gv-draft-notes]');
      var description = (text.value || '').trim();
      if (!description) { pageError(new Error('describe the workflow first')); return; }
      clearError();
      draftBtn.disabled = true;
      draftBtn.textContent = 'Drafting…';
      fetchJSON('/api/v1/workflows/draft', {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ description: description, graph: graph.nodes.length ? graph : undefined }),
      }).then(function (out) {
        mutate(function () {
          graph = out.graph;
          graph.edges = graph.edges || [];
          if (needsLayout(graph)) autoLayout(graph);
          graph.nodes.forEach(function (n) { n.position = n.position || { x: 0, y: 0 }; });
        });
        select(null);
        stage.zoomFit(graph);
        notes.hidden = !out.notes;
        notes.textContent = out.notes || '';
      }).catch(pageError).then(function () {
        draftBtn.disabled = false;
        draftBtn.textContent = 'Draft';
      });
    });

    var rawApply = document.querySelector('[data-gv-raw-apply]');
    if (rawApply) rawApply.addEventListener('click', function () {
      try {
        var parsed = JSON.parse(rawBox.value);
        if (!parsed || !Array.isArray(parsed.nodes)) throw new Error('want {"nodes": [...], "edges": [...]}');
        mutate(function () { graph = parsed; graph.edges = graph.edges || []; });
        select(null);
        clearError();
      } catch (err) { pageError(err); }
    });

    // ---- save ----

    var pendingName = '', pendingSchedule = '', pendingScheduleEnabled = false;
    var savedSnapshot = '';
    function dirty() {
      return snapshot() !== savedSnapshot ||
        (current ? (pendingSchedule !== (current.schedule || '') || pendingScheduleEnabled !== !!current.schedule_enabled) : graph.nodes.length > 0);
    }
    window.addEventListener('beforeunload', function (e) {
      if (dirty()) { e.preventDefault(); e.returnValue = ''; }
    });

    document.querySelector('[data-gv-save]').addEventListener('click', function () {
      clearError();
      var findings = lint().filter(function (f) { return f.level === 'err'; });
      if (findings.length) { pageError(new Error(findings[0].msg)); return; }
      var body = Object.assign({}, current || {});
      body.name = current ? name : pendingName;
      if (!body.name) { pageError(new Error('the workflow needs a name')); return; }
      body.schedule = pendingSchedule;
      body.schedule_enabled = pendingScheduleEnabled;
      body.graph = graph;
      delete body.steps;
      var url = '/api/v1/workflows', method = 'POST';
      if (current) {
        url += '/' + encodeURIComponent(name) + '?generation=' + current.generation;
        method = 'PUT';
      }
      fetchJSON(url, { method: method, headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body) })
        .then(function () {
          savedSnapshot = snapshot();
          window.location.href = '/workflows';
        })
        .catch(function (err) {
          if (/generation/.test(err.message)) pageError(new Error('this workflow changed under you — reload the page and redo the edit'));
          else pageError(err);
        });
    });

    // ---- load ----

    fetchJSON('/api/v1/routines').then(function (list) {
      routineNames = (list || []).map(function (r) { return r.name; });
      var dl = document.createElement('datalist');
      dl.id = 'gv-routine-names';
      routineNames.forEach(function (r) {
        var o = document.createElement('option');
        o.value = r;
        dl.appendChild(o);
      });
      document.body.appendChild(dl);
      lint();
    }).catch(function () {});

    function loaded() {
      if (needsLayout(graph)) autoLayout(graph);
      graph.nodes.forEach(function (n) { n.position = n.position || { x: 0, y: 0 }; });
      savedSnapshot = snapshot();
      afterChange();
      renderPanel();
      stage.zoomFit(graph);
    }

    if (name) {
      fetchJSON('/api/v1/workflows/' + encodeURIComponent(name)).then(function (wf) {
        current = wf;
        graph = wf.graph || { nodes: [], edges: [] };
        graph.edges = graph.edges || [];
        pendingSchedule = wf.schedule || '';
        pendingScheduleEnabled = !!wf.schedule_enabled;
        loaded();
      }).catch(pageError);
    } else {
      loaded();
    }

    // Test and debugging hooks (the Playwright lever, like window.ForgeSearch).
    window.ForgeGraph = {
      getModel: function () { return JSON.parse(snapshot()); },
      setModel: function (g) { mutate(function () { graph = g; graph.edges = graph.edges || []; }); select(null); },
      layout: function () { autoLayout(graph); afterChange(); },
      validate: function () { return validateGraph(graph, routineNames); },
      zoomFit: function () { stage.zoomFit(graph); },
      selectNode: function (id) { select({ node: id }); },
    };
  }

  // ---- the run view --------------------------------------------------------

  var runRoot = document.querySelector('[data-graph-run]');
  if (runRoot) initRunView(runRoot);

  function initRunView(root) {
    var runID = root.dataset.runId;
    var svg = root.querySelector('[data-gv-stage]');
    var panel = root.querySelector('[data-gv-panel]');
    var errorBox = document.querySelector('[data-gv-error]');
    var stateChip = document.querySelector('[data-gv-run-state]');
    var shortEl = document.querySelector('[data-gv-run-short]');
    var metaEl = document.querySelector('[data-gv-run-meta]');
    var cancelBtn = document.querySelector('[data-gv-run-cancel]');
    var stage = createStage(svg, {});
    var detail = null;
    var selectedNode = null;
    var fitted = false;

    if (shortEl) shortEl.textContent = runID.slice(0, 8);

    // latest instance per graph node id.
    function latestByNode() {
      var out = {};
      (detail.nodes || []).forEach(function (inst) {
        if (!out[inst.node_id] || inst.iteration > out[inst.node_id].iteration) out[inst.node_id] = inst;
      });
      return out;
    }

    function takenEdgeMap() {
      var taken = {};
      var byNode = {};
      (detail.nodes || []).forEach(function (inst) {
        (byNode[inst.node_id] = byNode[inst.node_id] || []).push(inst);
      });
      edgesOf(detail.graph).forEach(function (e, i) {
        var key = edgeKey(e);
        (byNode[e.to] || []).forEach(function (inst) {
          if (inst.edges && inst.edges[key] === 'taken') taken[i] = true;
        });
      });
      return taken;
    }

    function render() {
      var latest = latestByNode();
      renderGraph(stage, detail.graph, {
        readOnly: true,
        selectedNode: selectedNode,
        takenEdges: takenEdgeMap(),
        nodeState: function (n) {
          var inst = latest[n.id];
          return inst ? inst.status : 'none';
        },
        decorate: function (g, n) {
          var inst = latest[n.id];
          var t = el('text', { x: NODE_W - 8, y: 22, 'text-anchor': 'end', class: 'gv-state' });
          t.textContent = inst ? (inst.iteration > 1 ? '#' + inst.iteration + ' ' : '') + inst.status : '';
          g.appendChild(t);
        },
        onNode: function (g, n) {
          g.addEventListener('pointerdown', function () {
            selectedNode = n.id;
            render();
            renderNodeAside(n.id);
          });
        },
      });
      if (!fitted) { stage.zoomFit(detail.graph); fitted = true; }
      stateChip.textContent = detail.status;
      stateChip.className = 'state state-' + detail.status;
      if (metaEl) {
        var bits = ['trigger ' + detail.trigger];
        if (detail.script_runs) bits.push(detail.script_runs + ' script run(s)');
        if (detail.context && detail.context.objective) bits.push('objective: ' + detail.context.objective);
        metaEl.textContent = bits.join(' · ');
      }
      if (cancelBtn) cancelBtn.hidden = detail.status !== 'running';
    }

    function renderNodeAside(nodeID) {
      panel.textContent = '';
      var insts = (detail.nodes || []).filter(function (i) { return i.node_id === nodeID; });
      var h = document.createElement('h3');
      h.textContent = nodeID;
      panel.appendChild(h);
      if (!insts.length) {
        var p = document.createElement('p');
        p.className = 'hint';
        p.textContent = 'Not reached (yet).';
        panel.appendChild(p);
        return;
      }
      insts.sort(function (a, b) { return b.iteration - a.iteration; });
      insts.forEach(function (inst) {
        var head = document.createElement('p');
        var chip = document.createElement('span');
        chip.className = 'state state-' + inst.status;
        chip.textContent = inst.status;
        head.appendChild(chip);
        head.appendChild(document.createTextNode(' iteration ' + inst.iteration));
        panel.appendChild(head);
        if (inst.error) {
          var errP = document.createElement('p');
          errP.className = 'error';
          errP.textContent = inst.error;
          panel.appendChild(errP);
        }
        if (inst.work_id) {
          var link = document.createElement('a');
          link.className = 'btn';
          link.href = '/tasks/' + inst.work_id;
          link.textContent = 'Task ' + inst.work_id.slice(0, 8) + ' →';
          panel.appendChild(link);
        }
        if (inst.output) {
          var pre = document.createElement('pre');
          pre.className = 'result';
          try { pre.textContent = JSON.stringify(inst.output, null, 2); } catch (e) { pre.textContent = String(inst.output); }
          panel.appendChild(pre);
        }
      });
    }

    function refresh() {
      return fetchJSON('/api/v1/workflow-runs/' + encodeURIComponent(runID)).then(function (d) {
        detail = d;
        render();
        if (selectedNode) renderNodeAside(selectedNode);
      }).catch(function (err) {
        errorBox.hidden = false;
        errorBox.textContent = String(err.message || err);
      });
    }

    // Poll while the run is live; pause when the tab is hidden.
    var timer = null;
    function schedule() {
      window.clearTimeout(timer);
      if (!detail || detail.status !== 'running' || document.hidden) return;
      timer = window.setTimeout(function () { refresh().then(schedule); }, 2500);
    }
    document.addEventListener('visibilitychange', function () {
      if (!document.hidden) refresh().then(schedule); else window.clearTimeout(timer);
    });

    if (cancelBtn) cancelBtn.addEventListener('click', function () {
      cancelBtn.disabled = true;
      fetchJSON('/api/v1/workflow-runs/' + encodeURIComponent(runID) + '/cancel', { method: 'POST' })
        .then(refresh)
        .catch(function (err) { errorBox.hidden = false; errorBox.textContent = String(err.message || err); })
        .then(function () { cancelBtn.disabled = false; });
    });
    document.querySelector('[data-gv-fit]').addEventListener('click', function () { if (detail) stage.zoomFit(detail.graph); });

    refresh().then(schedule);

    window.ForgeGraph = window.ForgeGraph || {};
    window.ForgeGraph.runRefresh = refresh;
    window.ForgeGraph.runDetail = function () { return detail; };
  }
})();

// Row links and the phase timeline. No framework; behaviour attaches by data- attributes.
document.querySelectorAll('tr[data-href]').forEach(function (row) {
  row.addEventListener('click', function (e) {
    if (e.target.closest('a')) return;
    window.location = row.dataset.href;
  });
});
document.querySelectorAll('[data-timeline]').forEach(function (tl) {
  var spans = Array.prototype.slice.call(tl.querySelectorAll('.span'));
  var end = 0;
  spans.forEach(function (s) { end = Math.max(end, Number(s.dataset.elapsed)); });
  if (!end) return;
  spans.forEach(function (s) {
    var dur = Number(s.dataset.dur), start = Number(s.dataset.elapsed) - dur;
    var bar = s.querySelector('.bar');
    bar.style.marginLeft = (100 * Math.max(0, start) / end) + '%';
    bar.style.width = Math.max(0.5, 100 * dur / end) + '%';
    if (s.dataset.parent) s.style.paddingLeft = '1rem';
  });
});

// Queue drag-and-drop: PATCH {move_before}; the daemon refuses an order that
// puts a task above one it is blocked by, and the row snaps back.
(function () {
  var table = document.querySelector('[data-queue] tbody');
  if (!table) return;
  var dragging = null;
  var errBox = document.getElementById('queue-error');
  table.addEventListener('dragstart', function (e) {
    var row = e.target.closest('tr[data-id]');
    if (!row) return;
    dragging = row;
    e.dataTransfer.effectAllowed = 'move';
  });
  table.addEventListener('dragover', function (e) {
    e.preventDefault();
    var over = e.target.closest('tr[data-id]');
    if (!over || over === dragging || !dragging) return;
    var rect = over.getBoundingClientRect();
    var after = e.clientY > rect.top + rect.height / 2;
    over.parentNode.insertBefore(dragging, after ? over.nextSibling : over);
  });
  table.addEventListener('drop', function (e) { e.preventDefault(); commit(); });
  table.addEventListener('dragend', function () { commit(); });
  function commit() {
    if (!dragging) return;
    var moved = dragging; dragging = null;
    var next = moved.nextElementSibling;
    var body = next ? { move_before: next.dataset.id } : { move_before: '' };
    fetch('/api/v1/work/' + moved.dataset.id, {
      method: 'PATCH',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body),
    }).then(function (resp) {
      if (!resp.ok) return resp.json().then(function (e) { throw new Error(e.error || resp.status); });
      window.location.reload();
    }).catch(function (err) {
      errBox.hidden = false;
      errBox.textContent = 'Refused: ' + err.message;
      window.setTimeout(function () { window.location.reload(); }, 1200);
    });
  }
})();

// Human queue: answer a question in place.
document.querySelectorAll('[data-answer-form]').forEach(function (form) {
  form.addEventListener('submit', function (e) {
    e.preventDefault();
    var id = form.dataset.answerForm;
    var answer = form.querySelector('[name=answer]').value;
    fetch('/api/v1/questions/' + id + '/answer', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ answer: answer, by: 'human' }),
    }).then(function (resp) {
      if (!resp.ok) return resp.json().then(function (e) { throw new Error(e.error || resp.status); });
      window.location.reload();
    }).catch(function (err) { window.alert('Answer failed: ' + err.message); });
  });
});

// Proposals: approve/reject post to the API, which owns the rule — approve
// applies inside the decision's transaction, and a refused apply answers 409
// with the proposal left proposed.
document.querySelectorAll('[data-proposal-approve], [data-proposal-reject]').forEach(function (btn) {
  btn.addEventListener('click', function () {
    var id = btn.dataset.proposalApprove || btn.dataset.proposalReject;
    var action = btn.dataset.proposalApprove ? 'approve' : 'reject';
    fetch('/api/v1/proposals/' + id + '/' + action, { method: 'POST' }).then(function (resp) {
      if (!resp.ok) return resp.json().then(function (e) { throw new Error(e.error || resp.status); });
      window.location.reload();
    }).catch(function (err) {
      var box = document.getElementById('proposal-error');
      if (box) {
        box.hidden = false;
        box.textContent = 'Refused: ' + err.message;
      } else {
        window.alert('Refused: ' + err.message);
      }
    });
  });
});

// Dashboard usage gauges: one per window, target line, filled from the API so
// the page and `forge usage` can never disagree.
(function () {
  var box = document.querySelector('[data-usage]');
  if (!box) return;
  fetch('/api/v1/usage').then(function (resp) {
    if (!resp.ok) return; // budget policy inactive: leave the section hidden
    return resp.json();
  }).then(function (data) {
    if (!data) return;
    box.hidden = false;
    ['five_hour', 'seven_day'].forEach(function (w) {
      var g = box.querySelector('[data-window=' + w + ']');
      var u = data.usage[w], cfg = data.config;
      var target = cfg[w + '_target'], hard = cfg[w + '_hard_stop'];
      if (!u || u.utilization < 0) { g.querySelector('.meta').textContent = 'no samples yet'; return; }
      var fill = g.querySelector('.fill');
      fill.style.width = Math.min(100, 100 * u.utilization) + '%';
      fill.className = 'fill ' + (u.utilization >= hard ? 'bad' : u.utilization >= target ? 'warn' : 'ok');
      g.querySelector('.target').style.left = 100 * target + '%';
      var resets = Math.max(0, Math.round((new Date(u.resets_at) - Date.now()) / 60000));
      g.querySelector('.meta').textContent = (100 * u.utilization).toFixed(1) + '% · target ' + (100 * target).toFixed(0) +
        '% · resets in ' + (resets >= 60 ? Math.floor(resets / 60) + 'h' + (resets % 60) + 'm' : resets + 'm') +
        ' · 1h rate ' + (100 * u.rate_1h).toFixed(1) + '%/h · Δ ' + (u.delta >= 0 ? '+' : '') + (100 * u.delta).toFixed(1) + '%/h';
    });
  }).catch(function () {});
})();

// Click-to-copy: any inline `forge …` command shown anywhere in the UI becomes
// a one-click copy. A single pass upgrades every page, so a command in the
// human queue, an empty state, a hint, or a kb note is copied with one click
// (loopback is a secure context, so the clipboard API is available).
(function () {
  document.querySelectorAll('code').forEach(function (el) {
    var text = el.textContent.trim();
    if (text.indexOf('forge ') !== 0) return;   // only forge commands
    if (el.closest('pre')) return;               // leave fenced blocks alone
    el.classList.add('cmd-copy');
    el.setAttribute('role', 'button');
    el.setAttribute('tabindex', '0');
    el.setAttribute('aria-label', 'Copy command: ' + text);
    el.setAttribute('title', 'Click to copy');
    function copy(e) {
      if (e) { e.stopPropagation(); e.preventDefault(); }
      var done = function () {
        el.classList.add('copied');
        window.setTimeout(function () { el.classList.remove('copied'); }, 1200);
      };
      if (navigator.clipboard && navigator.clipboard.writeText) {
        navigator.clipboard.writeText(text).then(done).catch(select);
      } else {
        select();
      }
    }
    function select() {
      var r = document.createRange(); r.selectNodeContents(el);
      var s = window.getSelection(); s.removeAllRanges(); s.addRange(r);
      el.classList.add('copied');
      window.setTimeout(function () { el.classList.remove('copied'); }, 1200);
    }
    el.addEventListener('click', copy);
    el.addEventListener('keydown', function (e) {
      if (e.key === 'Enter' || e.key === ' ') copy(e);
    });
  });
})();

// Dashboard timeline: a live wall-clock view of everything active in a trailing
// window. Two controls (window + view) persist in the URL hash (#tl=view,window)
// so a reload keeps them. It re-fetches on a window change and every 10s so
// running bars grow and `now` advances — but never while the pointer is over the
// section (that would yank the tooltip). Colours use theme tokens only.
(function () {
  var root = document.querySelector('[data-timeline-live]');
  if (!root) return;
  var axisEl = root.querySelector('.tl-axis');
  var lanesEl = root.querySelector('.tl-lanes');
  var emptyEl = root.querySelector('.tl-empty');
  var windows = ['15m', '1h', '6h'];
  var views = ['attempt', 'task', 'repo'];
  var state = { window: '1h', view: 'task' };
  var data = null;      // last fetch: {now, since, window_seconds, items}
  var hovering = false; // pointer over the section: skip auto-refresh

  // Phase → colour: agent is the dominant work (--run), verify is --wait, the
  // git/setup phases are muted greys, cleanup the faintest. rgba() greys read
  // the same in light and dark.
  var phaseColor = {
    queue_wait: 'rgba(128,128,128,.22)',
    fetch: 'rgba(128,128,128,.3)',
    resolve_base: 'rgba(128,128,128,.4)',
    worktree_add: 'rgba(128,128,128,.35)',
    manifest: 'rgba(128,128,128,.48)',
    agent: 'var(--run)',
    git_inspect: 'rgba(128,128,128,.55)',
    verify: 'var(--wait)',
    cleanup: 'rgba(128,128,128,.16)'
  };

  // State → colour where phases are absent (running or a finished attempt with
  // no facts). The running family collapses to --run.
  function stateColor(s) {
    switch (s) {
      case 'succeeded': case 'merged': case 'applied': return 'var(--ok)';
      case 'failed': case 'unverified': case 'conflict': case 'cancelled': case 'partial': return 'var(--bad)';
      case 'running': case 'merging': case 'claimed': case 'preparing': case 'verifying': return 'var(--run)';
      case 'waiting_human': return 'var(--wait)';
      default: return 'var(--muted)';
    }
  }

  var tip = document.createElement('div');
  tip.className = 'tl-tip';
  tip.hidden = true;
  document.body.appendChild(tip);

  function esc(s) {
    return String(s == null ? '' : s).replace(/[&<>"]/g, function (c) {
      return { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c];
    });
  }
  function pad(n) { return (n < 10 ? '0' : '') + n; }
  function clock(ms) { var d = new Date(ms); return pad(d.getHours()) + ':' + pad(d.getMinutes()); }
  function fmtDur(us) {
    var s = us / 1e6;
    if (s < 1) return Math.round(us / 1000) + 'ms';
    if (s < 60) return s.toFixed(1) + 's';
    var m = Math.floor(s / 60);
    if (m < 60) return m + 'm' + pad(Math.round(s) % 60) + 's';
    return Math.floor(m / 60) + 'h' + pad(m % 60) + 'm';
  }
  function isRunning(it) { return !it.finished_at || it.finished_at.indexOf('0001-01-01') === 0; }
  function startMs(it) { return Date.parse(it.started_at); }
  function endMs(it, nowMs) { return isRunning(it) ? nowMs : Date.parse(it.finished_at); }
  function durLabel(it, nowMs) {
    if (isRunning(it)) return 'running ' + Math.max(1, Math.round((nowMs - startMs(it)) / 60000)) + 'm';
    return fmtDur(endMs(it, nowMs) - startMs(it));
  }

  function readHash() {
    var m = /(?:^|[#&])tl=([a-z]+),([0-9a-z]+)/i.exec(location.hash);
    if (!m) return;
    if (views.indexOf(m[1]) >= 0) state.view = m[1];
    if (windows.indexOf(m[2]) >= 0) state.window = m[2];
  }
  function writeHash() { location.hash = 'tl=' + state.view + ',' + state.window; }
  function syncButtons() {
    root.querySelectorAll('[data-tl-window]').forEach(function (b) { b.classList.toggle('on', b.dataset.tlWindow === state.window); });
    root.querySelectorAll('[data-tl-view]').forEach(function (b) { b.classList.toggle('on', b.dataset.tlView === state.view); });
  }

  function renderAxis(sinceMs, nowMs) {
    axisEl.textContent = '';
    var n = 5;
    for (var i = 0; i < n; i++) {
      var t = sinceMs + (nowMs - sinceMs) * i / (n - 1);
      var tick = document.createElement('span');
      tick.className = 'tl-tick';
      tick.style.left = (100 * i / (n - 1)) + '%';
      tick.textContent = clock(t);
      axisEl.appendChild(tick);
    }
  }

  function showTip(it, nowMs, seg) {
    var lines = [
      '<b>' + esc(it.title || it.work_id) + '</b>',
      '<span class="m">' + esc(it.routine || '?') + ' · ' + esc(it.repository || '?') + '</span>',
      '<span class="m">' + esc(it.state) + '</span>',
      '<span class="m">start ' + clock(startMs(it)) + ' · ' + esc(durLabel(it, nowMs)) + '</span>'
    ];
    if (seg && seg.dataset.phase) {
      lines.push('<span class="m">' + esc(seg.dataset.phase) + ' ' + fmtDur(Number(seg.dataset.dur)) + '</span>');
    }
    tip.innerHTML = lines.join('<br>');
    tip.hidden = false;
  }
  function moveTip(x, y) {
    var w = tip.offsetWidth, h = tip.offsetHeight;
    var left = Math.min(x + 14, window.innerWidth - w - 8);
    var top = Math.min(y + 14, window.innerHeight - h - 8);
    tip.style.left = Math.max(4, left) + 'px';
    tip.style.top = Math.max(4, top) + 'px';
  }
  function hideTip() { tip.hidden = true; }

  function bindBar(bar, it, nowMs) {
    function go() { window.location = '/tasks/' + it.work_id; }
    bar.addEventListener('click', go);
    bar.addEventListener('keydown', function (e) {
      if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); go(); }
    });
    bar.addEventListener('mouseenter', function (e) { showTip(it, nowMs, null); moveTip(e.clientX, e.clientY); });
    bar.addEventListener('mousemove', function (e) {
      var seg = e.target.classList && e.target.classList.contains('tl-seg') ? e.target : null;
      showTip(it, nowMs, seg);
      moveTip(e.clientX, e.clientY);
    });
    bar.addEventListener('focus', function () {
      var r = bar.getBoundingClientRect();
      showTip(it, nowMs, null);
      moveTip(r.left, r.bottom);
    });
    bar.addEventListener('mouseleave', hideTip);
    bar.addEventListener('blur', hideTip);
  }

  function renderBar(it, subRow, sinceMs, nowMs, span) {
    var s = Math.max(startMs(it), sinceMs), e = Math.min(endMs(it, nowMs), nowMs);
    var bar = document.createElement('div');
    bar.className = 'tl-bar';
    bar.style.left = ((s - sinceMs) / span * 100) + '%';
    bar.style.width = Math.max(0.6, (e - s) / span * 100) + '%';
    bar.style.top = (subRow * 1.15 + 0.1) + 'rem';
    bar.setAttribute('tabindex', '0');
    bar.setAttribute('role', 'listitem');
    bar.setAttribute('aria-label', (it.title || it.work_id) + ', ' + it.state + ', start ' + clock(startMs(it)) + ', ' + durLabel(it, nowMs));
    if (isRunning(it)) {
      bar.classList.add('tl-running');
      bar.style.background = stateColor(it.state);
    } else if (it.phases && it.phases.length) {
      var sum = 0;
      it.phases.forEach(function (p) { sum += p.duration_us; });
      it.phases.forEach(function (p) {
        var seg = document.createElement('span');
        seg.className = 'tl-seg';
        seg.style.flexGrow = String(p.duration_us);
        seg.style.background = phaseColor[p.name] || 'var(--muted)';
        seg.dataset.phase = p.name;
        seg.dataset.dur = p.duration_us;
        bar.appendChild(seg);
      });
      bar.style.boxShadow = 'inset 3px 0 ' + stateColor(it.state) + ', inset -3px 0 ' + stateColor(it.state);
    } else {
      bar.style.background = stateColor(it.state);
    }
    bindBar(bar, it, nowMs);
    return bar;
  }

  function renderLane(lane, sinceMs, nowMs, span) {
    var row = document.createElement('div');
    row.className = 'tl-lane';
    var label = document.createElement('div');
    label.className = 'tl-label';
    label.textContent = lane.label;
    label.title = lane.label;
    var track = document.createElement('div');
    track.className = 'tl-track';
    // Greedy packing: place each bar in the first sub-row whose last bar ends
    // before this one starts, so overlapping bars never visually collide.
    var bars = lane.items.slice().sort(function (a, b) { return startMs(a) - startMs(b); });
    var rowEnd = [];
    bars.forEach(function (it) {
      var s = startMs(it), e = endMs(it, nowMs), r = 0;
      for (; r < rowEnd.length; r++) { if (rowEnd[r] <= s) break; }
      rowEnd[r] = e;
      track.appendChild(renderBar(it, r, sinceMs, nowMs, span));
    });
    track.style.height = (Math.max(1, rowEnd.length) * 1.15 + 0.2) + 'rem';
    row.appendChild(label);
    row.appendChild(track);
    return row;
  }

  function render() {
    if (!data) return;
    var sinceMs = Date.parse(data.since), nowMs = Date.parse(data.now);
    var span = (nowMs - sinceMs) || 1;
    renderAxis(sinceMs, nowMs);
    emptyEl.hidden = data.items.length > 0;
    var groups = {}, order = [];
    data.items.forEach(function (it) {
      var key, label;
      if (state.view === 'attempt') { key = it.attempt_id; label = it.attempt_id.slice(0, 8); }
      else if (state.view === 'repo') { key = it.repository || '(none)'; label = key; }
      else { key = it.work_id; label = it.title || it.work_id.slice(0, 8); }
      var g = groups[key];
      if (!g) { g = groups[key] = { label: label, items: [], latest: 0 }; order.push(key); }
      g.items.push(it);
      var e = endMs(it, nowMs);
      if (e > g.latest) g.latest = e;
    });
    var lanes = order.map(function (k) { return groups[k]; });
    lanes.sort(function (a, b) { return b.latest - a.latest; }); // most-recent activity first
    var extra = 0;
    if (lanes.length > 40) { extra = lanes.length - 40; lanes = lanes.slice(0, 40); }
    lanesEl.textContent = '';
    lanes.forEach(function (lane) { lanesEl.appendChild(renderLane(lane, sinceMs, nowMs, span)); });
    if (extra > 0) {
      var more = document.createElement('p');
      more.className = 'tl-more empty';
      more.textContent = '+' + extra + ' more';
      lanesEl.appendChild(more);
    }
  }

  function load() {
    return fetch('/api/v1/timeline?window=' + encodeURIComponent(state.window))
      .then(function (r) { return r.ok ? r.json() : null; })
      .then(function (d) { if (d) { data = d; render(); } })
      .catch(function () {});
  }

  root.querySelectorAll('[data-tl-window]').forEach(function (b) {
    b.addEventListener('click', function () { state.window = b.dataset.tlWindow; syncButtons(); writeHash(); load(); });
  });
  root.querySelectorAll('[data-tl-view]').forEach(function (b) {
    b.addEventListener('click', function () { state.view = b.dataset.tlView; syncButtons(); writeHash(); render(); });
  });
  root.addEventListener('pointerenter', function () { hovering = true; });
  root.addEventListener('pointerleave', function () { hovering = false; });

  readHash();
  syncButtons();
  load();
  window.setInterval(function () { if (!hovering) load(); }, 10000);
})();

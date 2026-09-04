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
    var approve = !!btn.dataset.proposalApprove;
    var action = approve ? 'approve' : 'reject';

    function refuse(msg) {
      var clean = String(msg || '').replace(/: (conflict|not found|draining)$/, '');
      var box = document.getElementById('proposal-error');
      if (box) {
        box.hidden = false;
        box.textContent = 'Refused: ' + clean;
      } else {
        window.alert('Refused: ' + clean);
      }
    }

    // post resolves to null on success (after reloading) or {status, error}.
    function post(url) {
      return fetch(url, { method: 'POST' }).then(function (resp) {
        if (resp.ok) { window.location.reload(); return null; }
        return resp.json().then(function (e) { return { status: resp.status, error: e.error || String(resp.status) }; });
      });
    }

    post('/api/v1/proposals/' + id + '/' + action).then(function (fail) {
      if (!fail) return;
      // The eval gate refuses a routine/mode_prompt approval with no score
      // (409). Offer the human the force override; the A/B auto-revert still
      // guards the change.
      if (approve && fail.status === 409 && fail.error.indexOf('eval score') !== -1) {
        if (window.confirm('No eval score yet — approve anyway? The A/B auto-revert still guards a routine change.')) {
          post('/api/v1/proposals/' + id + '/approve?force=true').then(function (f2) {
            if (f2) refuse(f2.error);
          }).catch(function (err) { refuse(err.message); });
        }
        return;
      }
      refuse(fail.error);
    }).catch(function (err) { refuse(err.message); });
  });
});

// A fire-and-confirm button: POST somewhere, tick the button on success, alert
// on failure, and revert after a moment. The page never reloads — the click
// changes nothing on it. Backs two families of button below.
function bindFireButton(btn, url, opts) {
  var label = btn.textContent;
  btn.addEventListener('click', function () {
    btn.disabled = true;
    fetch(url, opts()).then(function (resp) {
      if (!resp.ok) return resp.json().then(function (e) { throw new Error(e.error || resp.status); });
      btn.textContent = label + ' ✓';
    }).catch(function (err) {
      window.alert('Action failed: ' + err.message);
    }).then(function () {
      btn.disabled = false;
      window.setTimeout(function () { btn.textContent = label; }, 2000);
    });
  });
}

// REST resource actions: a [data-action-post] button POSTs to a full resource
// URL (run a routine, run a workflow). The URL names a real, audited endpoint.
document.querySelectorAll('[data-action-post]').forEach(function (btn) {
  bindFireButton(btn, btn.dataset.actionPost, function () { return { method: 'POST' }; });
});

// RPC links: a [data-rpc] button fires a registered daemon action by NAME —
// POST /api/v1/rpc/<method> with the optional [data-rpc-args] JSON body. This
// is for small, side-effect-safe triggers (the test toast, queue-card actions
// an agent asked for) that would otherwise each need a bespoke endpoint; the
// method name is the whole contract, validated against the server registry.
document.querySelectorAll('[data-rpc]').forEach(function (btn) {
  bindFireButton(btn, '/api/v1/rpc/' + encodeURIComponent(btn.dataset.rpc), function () {
    var opts = { method: 'POST' };
    if (btn.dataset.rpcArgs) {
      opts.headers = { 'Content-Type': 'application/json' };
      opts.body = btn.dataset.rpcArgs;
    }
    return opts;
  });
});

// Repository controls: pause/resume, cancel-running, and the app-url form POST
// to the API and reload — the same pattern as the proposal decision buttons.
(function () {
  function fail(err) {
    var box = document.getElementById('repo-error');
    if (box) {
      box.hidden = false;
      box.textContent = 'Refused: ' + err.message;
    } else {
      window.alert('Refused: ' + err.message);
    }
  }
  function post(path, body) {
    return fetch(path, {
      method: 'POST',
      headers: body ? { 'Content-Type': 'application/json' } : undefined,
      body: body ? JSON.stringify(body) : undefined,
    }).then(function (resp) {
      if (!resp.ok) return resp.json().then(function (e) { throw new Error(e.error || resp.status); });
      window.location.reload();
    }).catch(fail);
  }
  function bind(attr, action) {
    document.querySelectorAll('[' + attr + ']').forEach(function (btn) {
      var name = btn.getAttribute(attr);
      btn.addEventListener('click', function () {
        post('/api/v1/repositories/' + encodeURIComponent(name) + '/' + action);
      });
    });
  }
  bind('data-repo-pause', 'pause');
  bind('data-repo-resume', 'resume');
  bind('data-repo-cancel', 'cancel-running');
  bind('data-repo-restore', 'restore');
  document.querySelectorAll('[data-repo-app-url]').forEach(function (form) {
    form.addEventListener('submit', function (e) {
      e.preventDefault();
      var url = form.querySelector('[name=url]').value;
      post('/api/v1/repositories/' + encodeURIComponent(form.dataset.repoAppUrl) + '/app-url', { url: url });
    });
  });
  // Archive deletes the checkout — confirm first.
  document.querySelectorAll('[data-repo-archive]').forEach(function (btn) {
    var name = btn.getAttribute('data-repo-archive');
    btn.addEventListener('click', function () {
      if (!window.confirm('Archive "' + name + '"? Its checkout is deleted from disk to free space; the metadata (facts, attempts, notes) is kept and it can be restored from its origin.')) return;
      post('/api/v1/repositories/' + encodeURIComponent(name) + '/archive');
    });
  });
  // Add-repo dialog: clone a URL or link a local path.
  var dialog = document.querySelector('[data-repo-dialog]');
  if (dialog) {
    var form = dialog.querySelector('form');
    var errBox = form.querySelector('.dialog-error');
    var addBtn = document.querySelector('[data-repo-add]');
    if (addBtn) addBtn.addEventListener('click', function () { form.reset(); errBox.hidden = true; dialog.showModal(); });
    form.querySelector('[data-repo-cancel]').onclick = function () { dialog.close(); };
    form.onsubmit = function (e) {
      e.preventDefault();
      var body = {
        url: form.querySelector('[name=url]').value.trim(),
        path: form.querySelector('[name=path]').value.trim(),
        name: form.querySelector('[name=name]').value.trim(),
      };
      var saveBtn = form.querySelector('[data-repo-save]');
      saveBtn.disabled = true;
      fetch('/api/v1/repositories', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body) })
        .then(function (resp) {
          if (!resp.ok) return resp.json().then(function (er) { throw new Error(er.error || resp.status); });
          window.location.reload();
        })
        .catch(function (err) { errBox.hidden = false; errBox.textContent = 'Refused: ' + String(err.message || err).replace(/: (conflict|not found|draining)$/, ''); })
        .then(function () { saveBtn.disabled = false; });
    };
  }
})();

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

// Search bar: GitHub-flavoured filtering as deletable chips. Committed filters
// render as chips (each with a delete ×); the input types the next one. Free
// text ANDs, key:value qualifiers (same key ORs, different keys AND), "-"
// negates, values may be quoted. In client mode it live-filters every
// [data-sf-item] on the page and mirrors the whole query into ?q= (shareable)
// AND into localStorage, so the active filters persist as you move between
// pages. A ?q= in the URL wins over the saved set. Server mode (Knowledge
// full-text) just submits. "/" focuses the bar.
(function () {
  var form = document.querySelector('[data-searchbar]');
  if (!form) return;
  var input = form.querySelector('input[name=q]');
  var suggestBox = form.querySelector('[data-sb-suggest]');
  var countEl = form.querySelector('[data-sb-count]');
  var chipsBox = form.querySelector('[data-sb-chips]');
  var serverMode = form.dataset.searchbar === 'server';
  var keys = (input.dataset.keys || '').split(',').filter(Boolean);
  var items = Array.prototype.slice.call(document.querySelectorAll('[data-sf-item]'));
  var STORE = 'forge.filter'; // shared across client-mode pages

  // committed filter tokens (the chips); the input holds the one being typed.
  var tokens = [];

  // --- tokenising: split a query into its raw [-]key:"value" pieces ---
  var TOKEN = /(-)?(?:([a-zA-Z][a-zA-Z0-9_-]*):)?("([^"]*)"?|[^\s"]+)/g;
  function tokenize(q) {
    var out = [], m; TOKEN.lastIndex = 0;
    while ((m = TOKEN.exec(q))) if (m[0].trim()) out.push(m[0].trim());
    return out;
  }
  function parse(q) {
    var terms = [], quals = {}, m; TOKEN.lastIndex = 0;
    while ((m = TOKEN.exec(q))) {
      var neg = !!m[1];
      var key = m[2] ? m[2].toLowerCase() : '';
      var val = (m[4] !== undefined ? m[4] : m[3]).toLowerCase();
      if (key && keys.indexOf(key) >= 0) (quals[key] = quals[key] || []).push({ v: val, neg: neg });
      else { var text = key ? key + ':' + val : val; if (text) terms.push({ v: text, neg: neg }); }
    }
    return { terms: terms, quals: quals };
  }

  // --- matching ---
  function textOf(el) {
    if (el._sfText === undefined) el._sfText = el.textContent.replace(/\s+/g, ' ').toLowerCase();
    return el._sfText;
  }
  function declared(el, key) {
    var raw = el.getAttribute('data-f-' + key);
    return raw == null ? null : raw.toLowerCase().split(/\s+/).filter(Boolean);
  }
  function valMatch(el, key, v) {
    var vals = declared(el, key);
    if (vals === null) return textOf(el).indexOf(v) >= 0;
    for (var i = 0; i < vals.length; i++) if (vals[i].indexOf(v) === 0) return true;
    return false;
  }
  function matches(el, q) {
    for (var i = 0; i < q.terms.length; i++) {
      var has = textOf(el).indexOf(q.terms[i].v) >= 0;
      if (q.terms[i].neg ? has : !has) return false;
    }
    for (var key in q.quals) {
      var pos = [], negs = [];
      q.quals[key].forEach(function (x) { (x.neg ? negs : pos).push(x.v); });
      if (pos.length && !pos.some(function (v) { return valMatch(el, key, v); })) return false;
      if (negs.some(function (v) { return valMatch(el, key, v); })) return false;
    }
    return true;
  }

  // effective query = committed chips + the token being typed.
  function effective() {
    var live = input.value.trim();
    return tokens.concat(live ? [live] : []).join(' ');
  }

  function renderChips() {
    if (!chipsBox) return;
    chipsBox.textContent = '';
    tokens.forEach(function (tok, i) {
      var chip = document.createElement('span');
      chip.className = 'sb-chip' + (tok.charAt(0) === '-' ? ' neg' : '');
      var label = document.createElement('span');
      label.className = 'sb-chip-label';
      label.textContent = tok;
      var x = document.createElement('button');
      x.type = 'button'; x.className = 'sb-chip-x'; x.setAttribute('aria-label', 'Remove filter ' + tok);
      x.textContent = '×';
      x.addEventListener('click', function (e) { e.preventDefault(); tokens.splice(i, 1); renderChips(); apply(); input.focus(); });
      chip.appendChild(label); chip.appendChild(x);
      chipsBox.appendChild(chip);
    });
  }

  function apply() {
    var q = effective();
    var active = q !== '';
    var pq = parse(q);
    items.forEach(function (el) { el._sfMatch = !active || matches(el, pq); });
    items.forEach(function (el) {
      var show = el._sfMatch;
      if (!show) { var kids = el.querySelectorAll('[data-sf-item]'); for (var i = 0; i < kids.length; i++) if (kids[i]._sfMatch) { show = true; break; } }
      el.hidden = !show;
    });
    if (countEl) {
      countEl.hidden = !active;
      if (active) {
        var total = 0, shown = 0;
        items.forEach(function (el) { if (el.parentElement && el.parentElement.closest('[data-sf-item]')) return; total++; if (!el.hidden) shown++; });
        countEl.textContent = shown + '/' + total;
      }
    }
    var params = new URLSearchParams(location.search);
    if (active) params.set('q', q); else params.delete('q');
    var qs = params.toString();
    history.replaceState(null, '', location.pathname + (qs ? '?' + qs : '') + location.hash);
    try { if (active) localStorage.setItem(STORE, q); else localStorage.removeItem(STORE); } catch (e) { /* private mode */ }
  }
  var applyTimer = null;
  function scheduleApply() { if (serverMode) return; window.clearTimeout(applyTimer); applyTimer = window.setTimeout(apply, 120); }

  // commit the typed input into chips (called on Enter / blur / navigation).
  function commit() {
    var live = input.value.trim();
    if (!live) return;
    tokenize(live).forEach(function (t) { tokens.push(t); });
    input.value = '';
    renderChips();
  }

  // --- suggestions (operate on the token being typed in the input) ---
  var current = [], sel = -1;
  function valuesFor(key) {
    var provided = input.getAttribute('data-values-' + key);
    if (provided) return provided.split(/\s+/).filter(Boolean);
    var seen = {}, out = [];
    items.forEach(function (el) { (declared(el, key) || []).forEach(function (v) { if (!seen[v]) { seen[v] = true; out.push(v); } }); });
    return out.sort();
  }
  function tokenAt() {
    var pos = input.selectionStart == null ? input.value.length : input.selectionStart;
    var before = input.value.slice(0, pos);
    var start = before.lastIndexOf(' ') + 1;
    return { start: start, end: pos, text: before.slice(start) };
  }
  function buildSuggestions() {
    var t = tokenAt().text.replace(/^-/, '');
    var list = [], colon = t.indexOf(':');
    if (colon >= 0) {
      var key = t.slice(0, colon).toLowerCase(), part = t.slice(colon + 1).toLowerCase().replace(/^"/, '');
      if (keys.indexOf(key) >= 0) valuesFor(key).forEach(function (v) { if (v.indexOf(part) === 0) list.push({ label: key + ':' + v, insert: key + ':' + v }); });
    } else {
      keys.forEach(function (k) { if (!t || k.indexOf(t.toLowerCase()) === 0) list.push({ label: k + ':', insert: k + ':' }); });
    }
    current = list.slice(0, 12); sel = -1; renderSuggest();
  }
  function renderSuggest() {
    suggestBox.textContent = '';
    if (!current.length) { suggestBox.hidden = true; return; }
    current.forEach(function (sug, i) {
      var el = document.createElement('div');
      el.className = 'sb-opt' + (i === sel ? ' on' : '');
      el.setAttribute('role', 'option'); el.setAttribute('aria-selected', i === sel ? 'true' : 'false');
      el.textContent = sug.label;
      el.addEventListener('mousedown', function (e) { e.preventDefault(); accept(i); });
      suggestBox.appendChild(el);
    });
    suggestBox.hidden = false;
  }
  function accept(i) {
    var sug = current[i]; if (!sug) return;
    var tok = tokenAt(), neg = tok.text.charAt(0) === '-' ? '-' : '';
    input.value = input.value.slice(0, tok.start) + neg + sug.insert + input.value.slice(tok.end);
    // a complete value (key:value) becomes a chip; a bare key: stays to type the value.
    if (sug.insert.charAt(sug.insert.length - 1) !== ':') { commit(); apply(); buildSuggestions(); return; }
    var caret = tok.start + neg.length + sug.insert.length;
    input.setSelectionRange(caret, caret); input.focus();
    buildSuggestions(); scheduleApply();
  }

  form.addEventListener('submit', function (e) { if (!serverMode) { e.preventDefault(); commit(); apply(); } });
  input.addEventListener('input', function () { buildSuggestions(); scheduleApply(); });
  input.addEventListener('focus', buildSuggestions);
  input.addEventListener('click', buildSuggestions);
  input.addEventListener('blur', function () { window.setTimeout(function () { suggestBox.hidden = true; if (!serverMode && input.value.trim()) { commit(); apply(); } }, 150); });
  input.addEventListener('keydown', function (e) {
    if (e.key === 'Backspace' && input.value === '' && tokens.length) { e.preventDefault(); input.value = tokens.pop(); renderChips(); apply(); buildSuggestions(); return; }
    if (!suggestBox.hidden && current.length) {
      if (e.key === 'ArrowDown') { e.preventDefault(); sel = (sel + 1) % current.length; renderSuggest(); return; }
      if (e.key === 'ArrowUp') { e.preventDefault(); sel = (sel - 1 + current.length) % current.length; renderSuggest(); return; }
      if (e.key === 'Enter' && sel >= 0) { e.preventDefault(); accept(sel); return; }
      if (e.key === 'Tab') { e.preventDefault(); accept(sel >= 0 ? sel : 0); return; }
      if (e.key === 'Escape') { suggestBox.hidden = true; return; }
    }
    if (e.key === 'Enter' && !serverMode) { e.preventDefault(); commit(); apply(); buildSuggestions(); }
  });
  document.addEventListener('keydown', function (e) {
    if (e.key !== '/' || e.ctrlKey || e.metaKey || e.altKey) return;
    var t = e.target;
    if (t && (t.tagName === 'INPUT' || t.tagName === 'TEXTAREA' || t.isContentEditable)) return;
    e.preventDefault(); input.focus(); input.select();
  });

  if (!serverMode) {
    var initial = new URLSearchParams(location.search).get('q');
    if (initial == null) { try { initial = localStorage.getItem(STORE); } catch (e) { initial = null; } }
    if (initial) { tokens = tokenize(initial); renderChips(); apply(); }
  }

  // Re-collect [data-sf-item]s and re-run the filter — for lists that grow after
  // load (the Tasks infinite-scroll loader appends rows the initial scan missed).
  window.ForgeSearch = {
    reapply: function () {
      items = Array.prototype.slice.call(document.querySelectorAll('[data-sf-item]'));
      apply();
    },
  };
})();

// Routine and workflow editors: one <dialog> per page, filled from the API on
// edit so the full object round-trips — the form only overwrites the fields it
// shows, and PUT replaces every editable field, so advanced fields (paths,
// models, allowed tools, …) survive an edit made here. POST creates, PUT with
// ?generation= updates (a 409 means it changed under you — reopen to reload).
(function () {
  function openEditor(dialog, opts) {
    var form = dialog.querySelector('form');
    var errBox = form.querySelector('.dialog-error');
    var saveBtn = form.querySelector('[data-editor-save]');

    function refuse(msg) {
      errBox.hidden = false;
      errBox.textContent = 'Refused: ' + String(msg || '').replace(/: (conflict|not found|draining)$/, '');
    }

    form.querySelector('[data-editor-title]').textContent = opts.title;
    form.querySelector('[name=name]').disabled = !!opts.current; // the name is the identity
    errBox.hidden = true;
    opts.fill();
    dialog.showModal();

    form.onsubmit = function (e) {
      e.preventDefault();
      var body;
      try { body = opts.collect(); } catch (err) { refuse(err.message); return; }
      var url = opts.base, method = 'POST';
      if (opts.current) {
        url = opts.base + '/' + encodeURIComponent(opts.current.name) + '?generation=' + opts.current.generation;
        method = 'PUT';
      }
      saveBtn.disabled = true;
      fetch(url, { method: method, headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body) })
        .then(function (resp) {
          if (!resp.ok) return resp.json().then(function (e) { throw new Error(e.error || resp.status); });
          window.location.reload();
        })
        .catch(function (err) { refuse(err.message); })
        .then(function () { saveBtn.disabled = false; });
    };
    form.querySelector('[data-editor-cancel]').onclick = function () { dialog.close(); };
  }

  function fetchJSON(url) {
    return fetch(url).then(function (resp) {
      if (!resp.ok) return resp.json().then(function (e) { throw new Error(e.error || resp.status); });
      return resp.json();
    });
  }
  function pageError(err) {
    var box = document.getElementById('editor-error');
    if (box) { box.hidden = false; box.textContent = String(err.message || err); }
  }

  // --- routines ---
  (function () {
    var dialog = document.querySelector('[data-routine-dialog]');
    if (!dialog) return;
    var form = dialog.querySelector('form');
    function field(n) { return form.querySelector('[name=' + n + ']'); }

    // Workflows join the target datalist (directives are server-rendered).
    var targetList = document.getElementById('target-names');
    if (targetList) fetchJSON('/api/v1/workflows').then(function (list) {
      (list || []).forEach(function (wf) {
        var o = document.createElement('option');
        o.value = 'workflow:' + wf.name;
        targetList.appendChild(o);
      });
    }).catch(function () {});


    // Workflows join the target datalist (directives are server-rendered).
    var targetList = document.getElementById('target-names');
    if (targetList) fetchJSON('/api/v1/workflows').then(function (list) {
      (list || []).forEach(function (wf) {
        var o = document.createElement('option');
        o.value = 'workflow:' + wf.name;
        targetList.appendChild(o);
      });
    }).catch(function () {});


    function open(current) {
      form.reset();
      openEditor(dialog, {
        title: current ? 'Edit routine ' + current.name : 'New routine',
        base: '/api/v1/routines',
        current: current,
        fill: function () {
          if (!current) return;
          ['name', 'target', 'objective', 'budget_class', 'schedule', 'autonomy'].forEach(function (n) {
            field(n).value = current[n] || '';
          });
          ['priority', 'timeout_seconds', 'max_turns', 'concurrency'].forEach(function (n) {
            field(n).value = current[n] || '';
          });
          field('max_budget_usd').value = current.max_budget_usd || '';
          field('repositories').value = (current.repositories || []).join(', ');
          ['schedule_enabled', 'integrate', 'require_sandbox'].forEach(function (n) {
            field(n).checked = !!current[n];
          });
        },
        collect: function () {
          var body = Object.assign({}, current);
          ['name', 'target', 'objective', 'budget_class', 'schedule', 'autonomy'].forEach(function (n) {
            body[n] = field(n).value.trim();
          });
          ['priority', 'timeout_seconds', 'max_turns', 'concurrency'].forEach(function (n) {
            body[n] = Number(field(n).value) || 0;
          });
          body.max_budget_usd = Number(field('max_budget_usd').value) || 0;
          body.repositories = field('repositories').value.split(',').map(function (s) { return s.trim(); }).filter(Boolean);
          ['schedule_enabled', 'integrate', 'require_sandbox'].forEach(function (n) {
            body[n] = field(n).checked;
          });
          delete body.mode; delete body.model; delete body.persona; delete body.prompt; delete body.effort;
          return body;
        },
      });
    }

    var newBtn = document.querySelector('[data-routine-new]');
    if (newBtn) newBtn.addEventListener('click', function () { open(null); });
    document.querySelectorAll('[data-routine-edit]').forEach(function (btn) {
      btn.addEventListener('click', function () {
        fetchJSON('/api/v1/routines/' + encodeURIComponent(btn.dataset.routineEdit)).then(open).catch(pageError);
      });
    });
    // The Prompts page builds its routine detail dynamically (prompts.js);
    // this hook lets its Edit button open the same dialog.
    window.ForgeRoutines = {
      edit: function (name) {
        fetchJSON('/api/v1/routines/' + encodeURIComponent(name)).then(open).catch(pageError);
      },
    };
  })();

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
      // Keep the first and last labels inside the axis so they never spill past
      // the page edge (the default centering half-overhangs both ends).
      if (i === 0) tick.style.transform = 'translateX(0)';
      else if (i === n - 1) tick.style.transform = 'translateX(-100%)';
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

// Ask Forge: the top-right chat popout. A prompt is routed by intent — a
// question runs read-only explore (the answer becomes a kb note); a change
// request runs intake, which files it as an issue and hands off to implement.
// The guess flips as you type; one click on Ask/Change overrides it. Either
// way the send is one POST /api/v1/tasks with the chosen mode, so the daemon's
// normal queue, budget, and dedupe rules all apply.
(function () {
  var pop = document.querySelector('[data-chat]');
  var toggle = document.querySelector('[data-chat-toggle]');
  if (!pop || !toggle) return;
  var text = pop.querySelector('[data-chat-text]');
  var repoSel = pop.querySelector('[data-chat-repo]');
  var send = pop.querySelector('[data-chat-send]');
  var hint = pop.querySelector('[data-chat-hint]');
  var status = pop.querySelector('[data-chat-status]');
  var routeBtns = Array.prototype.slice.call(pop.querySelectorAll('[data-chat-route]'));
  var route = 'explore', overridden = false, reposLoaded = false;

  var HINTS = {
    explore: 'Ask: runs read-only explore — the answer lands in a knowledge note.',
    intake: 'Change: runs intake — files the request, then hands off to implement.'
  };
  function setRoute(r, manual) {
    route = r;
    if (manual) overridden = true;
    routeBtns.forEach(function (b) { b.classList.toggle('on', b.dataset.chatRoute === r); });
    hint.textContent = HINTS[r];
  }
  var QUESTION = /^(what|why|how|where|when|who|which|is|are|was|were|does|do|did|can|could|should|would|will|explain)\b|\?\s*$/i;
  function infer() {
    if (!overridden) setRoute(QUESTION.test(text.value.trim()) ? 'explore' : 'intake', false);
  }

  function loadRepos() {
    if (reposLoaded) return;
    reposLoaded = true;
    fetch('/api/v1/repositories').then(function (r) { return r.ok ? r.json() : []; }).then(function (repos) {
      repoSel.textContent = '';
      repos.forEach(function (r) {
        var opt = document.createElement('option');
        opt.value = r.name;
        opt.textContent = r.name;
        repoSel.appendChild(opt);
      });
      if (!repos.length) {
        var none = document.createElement('option');
        none.value = '';
        none.textContent = 'no repositories';
        repoSel.appendChild(none);
        return;
      }
      var names = repos.map(function (r) { return r.name; });
      var last = null;
      try { last = window.localStorage.getItem('forge.chat.repo'); } catch (e) { /* storage blocked */ }
      if (last && names.indexOf(last) >= 0) repoSel.value = last;
      else if (names.indexOf('forge') >= 0) repoSel.value = 'forge'; // forge changes are the headline use
    }).catch(function () { reposLoaded = false; });
  }

  function open() {
    pop.hidden = false;
    toggle.setAttribute('aria-expanded', 'true');
    loadRepos();
    text.focus();
  }
  function close() {
    pop.hidden = true;
    toggle.setAttribute('aria-expanded', 'false');
  }
  toggle.addEventListener('click', function () { if (pop.hidden) open(); else close(); });
  pop.querySelector('[data-chat-close]').addEventListener('click', close);
  document.addEventListener('keydown', function (e) { if (e.key === 'Escape' && !pop.hidden) close(); });

  routeBtns.forEach(function (b) {
    b.addEventListener('click', function () { setRoute(b.dataset.chatRoute, true); });
  });
  text.addEventListener('input', function () { status.hidden = true; infer(); });

  function submit() {
    var prompt = text.value.trim();
    if (!prompt || !repoSel.value) return;
    send.disabled = true;
    status.hidden = true;
    try { window.localStorage.setItem('forge.chat.repo', repoSel.value); } catch (e) { /* storage blocked */ }
    fetch('/api/v1/tasks', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ prompt: prompt, repositories: [repoSel.value], mode: route })
    }).then(function (resp) {
      return resp.json().then(function (body) {
        if (!resp.ok) throw new Error(body.error || String(resp.status));
        status.hidden = false;
        status.textContent = (route === 'explore' ? 'Question filed as task ' : 'Change filed as task ');
        var a = document.createElement('a');
        a.href = '/tasks/' + body.work.id;
        a.textContent = body.work.id.slice(0, 8);
        status.appendChild(a);
        text.value = '';
        overridden = false;
      });
    }).catch(function (err) {
      status.hidden = false;
      status.textContent = 'Refused: ' + err.message;
    }).then(function () { send.disabled = false; });
  }
  send.addEventListener('click', submit);
  text.addEventListener('keydown', function (e) {
    if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) { e.preventDefault(); submit(); }
  });

  setRoute('explore', false);
})();

// --- Tasks page: infinite scroll, New-task dialog, and scope-by-DSL ---
// The Tasks list loads 100 rows at a time (newest first) and defaults to open
// tasks; a sentinel below the table pulls the next page into view, the New-task
// button files arbitrary work, and a scope token in the search (scope:all,
// scope:closed, or a state: naming a terminal state) widens past open tasks.
(function () {
  var sentinel = document.querySelector('[data-task-more]');
  var tbody = document.querySelector('[data-task-rows]');

  // Infinite scroll: fetch and append the next page when the sentinel nears the
  // viewport. Newly appended rows are run through the active search filter so a
  // committed chip keeps hiding what it should.
  if (sentinel && tbody && 'IntersectionObserver' in window) {
    var loading = false;
    function loadMore() {
      if (loading || sentinel.dataset.done) return;
      loading = true;
      var scope = sentinel.dataset.scope || 'open';
      var offset = Number(sentinel.dataset.offset) || 0;
      fetch('/tasks/rows?scope=' + encodeURIComponent(scope) + '&offset=' + offset)
        .then(function (resp) {
          if (!resp.ok) throw new Error(resp.status);
          var more = resp.headers.get('X-Has-More');
          return resp.text().then(function (html) { return { html: html, more: more }; });
        })
        .then(function (r) {
          var tpl = document.createElement('tbody');
          tpl.innerHTML = r.html.trim();
          var added = tpl.querySelectorAll('tr').length;
          while (tpl.firstChild) tbody.appendChild(tpl.firstChild);
          sentinel.dataset.offset = String(offset + added);
          if (!r.more || added === 0) sentinel.dataset.done = '1';
          if (window.ForgeSearch && window.ForgeSearch.reapply) window.ForgeSearch.reapply();
          loading = false;
          if (!sentinel.dataset.done && isNear(sentinel)) loadMore(); // fill a tall viewport
        })
        .catch(function () { loading = false; });
    }
    function isNear(el) {
      var r = el.getBoundingClientRect();
      return r.top < (window.innerHeight || document.documentElement.clientHeight) + 400;
    }
    var obs = new IntersectionObserver(function (entries) {
      if (entries.some(function (e) { return e.isIntersecting; })) loadMore();
    }, { rootMargin: '400px' });
    obs.observe(sentinel);
  }

  // New-task dialog: file arbitrary work (POST /api/v1/tasks, the ad-hoc path).
  var dialog = document.querySelector('[data-task-dialog]');
  if (dialog) {
    var form = dialog.querySelector('form');
    var errBox = form.querySelector('.dialog-error');
    var saveBtn = form.querySelector('[data-task-save]');
    function field(n) { return form.querySelector('[name=' + n + ']'); }
    var newBtn = document.querySelector('[data-task-new]');
    if (newBtn) newBtn.addEventListener('click', function () {
      form.reset();
      errBox.hidden = true;
      dialog.showModal();
    });
    form.querySelector('[data-task-cancel]').onclick = function () { dialog.close(); };
    form.onsubmit = function (e) {
      e.preventDefault();
      var body = {
        prompt: field('prompt').value.trim(),
        title: field('title').value.trim(),
        mode: field('mode').value.trim(),
        model: field('model').value.trim(),
        class: field('class').value,
        integrate: field('integrate').checked,
        repositories: field('repositories').value.split(',').map(function (s) { return s.trim(); }).filter(Boolean),
      };
      var mt = parseInt(field('max_turns').value, 10);
      if (mt > 0) body.max_turns = mt;
      var to = parseInt(field('timeout_seconds').value, 10);
      if (to > 0) body.timeout_seconds = to;
      saveBtn.disabled = true;
      fetch('/api/v1/tasks', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body) })
        .then(function (resp) {
          if (!resp.ok) return resp.json().then(function (er) { throw new Error(er.error || resp.status); });
          return resp.json().then(function (out) { window.location = '/tasks/' + out.work.id; });
        })
        .catch(function (err) {
          errBox.hidden = false;
          errBox.textContent = 'Refused: ' + String(err.message || err).replace(/: (conflict|not found|draining)$/, '');
        })
        .then(function () { saveBtn.disabled = false; });
    };
  }

  // Scope by DSL: a scope:open|closed|all token, or a state: naming a terminal
  // state, widens the server scope so closed tasks are actually fetched. The
  // search chips still filter client-side within whatever scope is loaded.
  if (sentinel) {
    var CLOSED = { succeeded: 1, failed: 1, cancelled: 1, merged: 1, partial: 1, unverified: 1 };
    function impliedScope(q) {
      var m = /(?:^|\s)scope:(open|closed|all)\b/.exec(q);
      if (m) return m[1];
      var st = /(?:^|\s)-?state:([a-z_]+)/g, x;
      while ((x = st.exec(q))) { if (CLOSED[x[1]]) return 'all'; }
      return null;
    }
    var rank = { open: 0, closed: 1, all: 2 };
    function maybeWiden(q) {
      var want = impliedScope(q);
      var cur = sentinel.dataset.scope || 'open';
      if (!want || want === cur) return;
      // Only ever widen automatically (open -> all); never silently narrow.
      if (rank[want] <= rank[cur] && !(want === 'closed' && cur === 'open')) return;
      var url = '/tasks?scope=' + want + (q ? '&q=' + encodeURIComponent(q) : '');
      window.location = url;
    }
    var input = document.querySelector('.searchbar input');
    if (input) {
      var check = function () { window.setTimeout(function () { maybeWiden(new URLSearchParams(location.search).get('q') || ''); }, 0); };
      input.addEventListener('change', check);
      input.addEventListener('keydown', function (e) { if (e.key === 'Enter') check(); });
    }
    maybeWiden(new URLSearchParams(location.search).get('q') || '');
  }
})();

// --- Settings: General (daemon health + log level) and Plugins management ---
(function () {
  function setErr(id, msg) {
    var box = document.getElementById(id);
    if (box) { box.hidden = !msg; box.textContent = msg ? String(msg) : ''; }
  }
  function humanDur(sec) {
    sec = Math.max(0, Math.floor(sec));
    var d = Math.floor(sec / 86400); sec -= d * 86400;
    var h = Math.floor(sec / 3600); sec -= h * 3600;
    var m = Math.floor(sec / 60);
    if (d) return d + 'd ' + h + 'h';
    if (h) return h + 'h ' + m + 'm';
    if (m) return m + 'm';
    return sec + 's';
  }
  function humanBytes(n) {
    if (!n) return '—';
    var u = ['B', 'KB', 'MB', 'GB', 'TB'], i = 0;
    while (n >= 1024 && i < u.length - 1) { n /= 1024; i++; }
    return n.toFixed(i ? 1 : 0) + ' ' + u[i];
  }

  // General page: fill the health panel + log-level input from the API.
  var general = document.querySelector('[data-settings-general]');
  if (general) {
    var hget = function (k) { return general.querySelector('[data-h="' + k + '"]'); };
    fetch('/api/v1/health').then(function (r) { return r.json(); }).then(function (h) {
      hget('daemon').textContent = h.daemon;
      hget('version').textContent = h.version;
      hget('schema').textContent = h.schema;
      hget('uptime').textContent = humanDur(h.uptime_s);
      var wk = h.worker || {};
      hget('worker').textContent = wk.registered
        ? (wk.connected ? 'connected' : 'registered, offline') + ' · ' + (wk.active_attempts || 0) + '/' + (wk.max_concurrent || 0) + ' slots'
        : 'none registered';
      hget('disk').textContent = humanBytes(h.disk_free_bytes);
      hget('backup').textContent = (h.backup && h.backup.age_s != null) ? humanDur(h.backup.age_s) + ' ago' : 'never';
    }).catch(function (e) { setErr('settings-error', 'Health unavailable: ' + e.message); });

    var form = document.querySelector('[data-loglevel-form]');
    var input = document.querySelector('[data-loglevel-input]');
    fetch('/api/v1/log-level').then(function (r) { return r.json(); }).then(function (b) { input.value = b.levels || ''; }).catch(function () {});
    if (form) form.addEventListener('submit', function (e) {
      e.preventDefault();
      fetch('/api/v1/log-level', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ levels: input.value.trim() }) })
        .then(function (resp) {
          if (!resp.ok) return resp.json().then(function (er) { throw new Error(er.error || resp.status); });
          return resp.json().then(function (b) { input.value = b.levels || ''; setErr('settings-error', ''); });
        })
        .catch(function (err) { setErr('settings-error', 'Refused: ' + err.message); });
    });
  }

  // Plugins page: enable/disable/uninstall/install, each posting then reloading.
  function pluginAction(url, method, confirmMsg) {
    if (confirmMsg && !window.confirm(confirmMsg)) return;
    fetch(url, { method: method }).then(function (resp) {
      if (!resp.ok) return resp.json().then(function (e) { throw new Error(e.error || resp.status); });
      window.location.reload();
    }).catch(function (err) { setErr('plugin-error', 'Refused: ' + err.message); });
  }
  document.querySelectorAll('[data-plugin-enable]').forEach(function (b) {
    b.addEventListener('click', function () { pluginAction('/api/v1/plugins/' + encodeURIComponent(b.dataset.pluginEnable) + '/enable', 'POST'); });
  });
  document.querySelectorAll('[data-plugin-disable]').forEach(function (b) {
    b.addEventListener('click', function () { pluginAction('/api/v1/plugins/' + encodeURIComponent(b.dataset.pluginDisable) + '/disable', 'POST'); });
  });
  document.querySelectorAll('[data-plugin-uninstall]').forEach(function (b) {
    b.addEventListener('click', function () { pluginAction('/api/v1/plugins/' + encodeURIComponent(b.dataset.pluginUninstall), 'DELETE', 'Uninstall plugin "' + b.dataset.pluginUninstall + '"?'); });
  });
  var installForm = document.querySelector('[data-plugin-install-form]');
  if (installForm) installForm.addEventListener('submit', function (e) {
    e.preventDefault();
    var name = installForm.querySelector('[name=name]').value.trim();
    fetch('/api/v1/plugins/install', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ name: name }) })
      .then(function (resp) {
        if (!resp.ok) return resp.json().then(function (er) { throw new Error(er.error || resp.status); });
        window.location.reload();
      }).catch(function (err) { setErr('plugin-error', 'Refused: ' + err.message); });
  });
})();

// --- Repo app lifecycle: Start/Stop/Rebuild a repo's [run] app, no agent ---
(function () {
  var card = document.querySelector('[data-app-card]');
  if (!card) return;
  var name = card.dataset.repo;
  var base = '/api/v1/repositories/' + encodeURIComponent(name) + '/app';
  var stateEl = card.querySelector('[data-app-state]');
  var metaEl = card.querySelector('[data-app-meta]');
  var controls = card.querySelector('[data-app-controls]');
  var unconfigured = card.querySelector('[data-app-unconfigured]');
  var openLink = card.querySelector('[data-app-open]');
  var buttons = card.querySelectorAll('[data-app-action]');
  var busy = false;

  function render(st) {
    stateEl.textContent = st.state;
    stateEl.className = 'state state-' + String(st.state).replace(/_/g, '-');
    unconfigured.hidden = st.configured;
    controls.hidden = !st.configured;
    var bits = [];
    if (st.port) bits.push('port ' + st.port);
    if (st.pid) bits.push('pid ' + st.pid);
    if (st.hot_reload) bits.push('hot-reload');
    if (st.message) bits.push(st.message);
    metaEl.textContent = bits.join(' · ');
    var running = st.state === 'running' || st.state === 'starting' || st.state === 'building';
    card.querySelector('[data-app-action="start"]').disabled = running;
    card.querySelector('[data-app-action="stop"]').disabled = st.state === 'stopped' || st.state === 'errored';
    if (st.url && st.state === 'running') { openLink.hidden = false; openLink.href = st.url; } else { openLink.hidden = true; }
  }
  function refresh() {
    fetch(base).then(function (r) { return r.json(); }).then(render).catch(function () {});
  }
  buttons.forEach(function (btn) {
    if (!btn.dataset.appAction) return;
    btn.addEventListener('click', function () {
      if (busy) return;
      busy = true;
      var label = btn.textContent; btn.textContent = label + '…';
      fetch(base + '/' + btn.dataset.appAction, { method: 'POST' })
        .then(function (r) { if (!r.ok) return r.json().then(function (e) { throw new Error(e.error || r.status); }); return r.json(); })
        .then(render)
        .catch(function (err) {
          var box = document.getElementById('repo-error');
          if (box) { box.hidden = false; box.textContent = 'Refused: ' + err.message; }
        })
        .then(function () { busy = false; btn.textContent = label; });
    });
  });
  refresh();
  window.setInterval(refresh, 3000); // live state while transitioning
})();

// --- Mobile nav: hamburger toggles the vertical link popout ---
(function () {
  var toggle = document.querySelector('[data-nav-toggle]');
  var links = document.querySelector('[data-nav-links]');
  if (!toggle || !links) return;
  function setOpen(open) {
    links.classList.toggle('open', open);
    toggle.setAttribute('aria-expanded', open ? 'true' : 'false');
  }
  toggle.addEventListener('click', function (e) {
    e.stopPropagation();
    setOpen(!links.classList.contains('open'));
  });
  // Close when a link is chosen, on Escape, or on an outside click.
  links.addEventListener('click', function (e) { if (e.target.closest('a')) setOpen(false); });
  document.addEventListener('keydown', function (e) { if (e.key === 'Escape') setOpen(false); });
  document.addEventListener('click', function (e) {
    if (links.classList.contains('open') && !links.contains(e.target) && !toggle.contains(e.target)) setOpen(false);
  });
})();

// Workflow list cost estimates: each [data-wf-est] cell asks the estimate API
// what a run of that graph is expected to spend (p50 of real attempt history
// per routine node). Honest about coverage: nodes without history are counted,
// not priced.
(function () {
  var cells = document.querySelectorAll('[data-wf-est]');
  if (!cells.length) return;
  cells.forEach(function (cell) {
    fetch('/api/v1/workflows/' + encodeURIComponent(cell.dataset.wfEst) + '/estimate')
      .then(function (resp) { return resp.ok ? resp.json() : Promise.reject(new Error(resp.status)); })
      .then(function (est) {
        cell.textContent = '';
        if (!est.routine_nodes) { cell.textContent = '—'; return; }
        if (!est.known_nodes) { cell.textContent = 'no history yet'; cell.title = 'No priced attempts for these routines in the last 30 days.'; return; }
        var usd = est.known_usd;
        var text = '≈ $' + (usd < 0.095 ? usd.toFixed(3) : usd.toFixed(2));
        if (est.known_nodes < est.routine_nodes) text += ' (partial)';
        if (est.conditional) text += ' †';
        cell.textContent = text;
        var lines = est.nodes.map(function (n) {
          var v = n.usd == null ? 'no history' : '$' + n.usd.toFixed(3) + ' p50 of ' + n.samples + ' ' + (n.source === 'routine' ? 'runs' : n.model + ' attempts');
          return n.node + ' (' + n.routine + (n.model ? ' on ' + n.model : '') + '): ' + v + (n.loop_cap ? ' — may loop ×' + n.loop_cap : '');
        });
        if (est.known_nodes < est.routine_nodes) lines.push('Partial: ' + est.known_nodes + ' of ' + est.routine_nodes + ' routine nodes have history.');
        if (est.conditional) lines.push('† Branching: not every node necessarily runs, and loops can repeat nodes.');
        cell.title = lines.join('\n');
      })
      .catch(function () { cell.textContent = ''; });
  });
})();

// The Prompts page (routines.html): the library tree is server-rendered;
// this file drives the detail pane — fragment and persona inspection,
// composed-prompt previews, and routine testing against the real assembly
// path (GET /api/v1/routines/{name}/preview). Same rules as app.js: no
// framework, data- attributes, DOM built with createElement so user content
// never meets innerHTML.
(function () {
  'use strict';

  var page = document.querySelector('[data-prompts-page]');
  if (!page) return;
  var detail = page.querySelector('[data-prompt-detail]');
  var errBox = document.getElementById('editor-error');

  function fetchJSON(url) {
    return fetch(url).then(function (resp) {
      if (!resp.ok) return resp.json().then(function (e) { throw new Error(e.error || resp.status); });
      return resp.json();
    });
  }
  function fail(err) {
    errBox.hidden = false;
    errBox.textContent = String(err.message || err);
  }
  function clearFail() { errBox.hidden = true; }

  var routines = [];
  var routinesReady = fetchJSON('/api/v1/routines').then(function (list) { routines = list || []; }).catch(function () {});

  function el(tag, cls, text) {
    var node = document.createElement(tag);
    if (cls) node.className = cls;
    if (text !== undefined) node.textContent = text;
    return node;
  }
  function chip(text) { return el('span', 'chip', text); }
  function label(text) { return el('label', 'pr-label', text); }
  function pre(text) {
    var p = el('pre', 'result');
    p.textContent = text;
    return p;
  }
  function button(text, cls, onClick) {
    var b = el('button', 'btn' + (cls ? ' ' + cls : ''), text);
    b.type = 'button';
    b.addEventListener('click', onClick);
    return b;
  }

  // ---- fragment / persona detail ----

  function promptURL(name, query) {
    return '/api/v1/prompts/' + name.split('/').map(encodeURIComponent).join('/') + (query || '');
  }

  function showFragment(name) {
    clearFail();
    fetchJSON(promptURL(name)).then(function (f) {
      detail.textContent = '';
      var head = el('div', 'pr-head');
      head.appendChild(el('h2', '', f.name));
      head.appendChild(chip(f.persona ? 'persona' : 'fragment'));
      if (f.model) head.appendChild(chip('model: ' + f.model));
      (f.modes || []).forEach(function (m) { head.appendChild(chip('mode: ' + m)); });
      detail.appendChild(head);
      if (f.path) {
        var meta = el('p', 'meta');
        meta.textContent = 'On disk: ';
        meta.appendChild(el('code', '', f.path));
        meta.appendChild(document.createTextNode(' — edits here commit to the library repo.'));
        detail.appendChild(meta);
      }
      sourceEditor(f);
      if (f.persona) {
        personaComposer(f);
        personaTester(f);
      }
    }).catch(fail);
  }

  // sourceEditor: the raw file, with an in-place edit → validate → commit →
  // hot-reload flow. An edit that would break the library is refused with the
  // loader's error and the file stays as it was.
  function sourceEditor(f) {
    var wrap = el('div');
    detail.appendChild(wrap);
    function view() {
      wrap.textContent = '';
      var row = el('div', 'pr-head');
      row.appendChild(label('Source'));
      row.appendChild(button('Edit', '', edit));
      wrap.appendChild(row);
      wrap.appendChild(pre(rawOf(f)));
    }
    function edit() {
      wrap.textContent = '';
      wrap.appendChild(label('Editing ' + f.name + ' — Save validates the whole library, commits, and reloads'));
      var ta = document.createElement('textarea');
      ta.className = 'pr-source';
      ta.rows = Math.min(24, Math.max(8, rawOf(f).split('\n').length + 2));
      ta.value = rawOf(f);
      wrap.appendChild(ta);
      var row = el('div', 'pr-controls');
      row.appendChild(button('Save', 'primary', function (e) {
        var btn = e.currentTarget;
        btn.disabled = true;
        fetch(promptURL(f.name), { method: 'PUT', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ content: ta.value }) })
          .then(function (resp) {
            if (!resp.ok) return resp.json().then(function (er) { throw new Error(er.error || resp.status); });
            clearFail();
            showFragment(f.name); // re-render from the fresh library
          })
          .catch(function (err) { fail(err); btn.disabled = false; });
      }));
      row.appendChild(button('Cancel', '', view));
      wrap.appendChild(row);
    }
    view();
  }

  // rawOf reconstructs the file text from the API's split view (frontmatter +
  // core + mode sections) so the editor round-trips what is on disk.
  function rawOf(f) {
    if (f.raw !== undefined) return f.raw;
    return f.body || '';
  }

  // personaTester: run the persona through the full assembly path — mode,
  // task text, objective, repository — and see the byte-exact prompt an agent
  // wearing it would read.
  function personaTester(f) {
    detail.appendChild(el('h3', '', 'Test: the prompt an agent wearing this persona would read'));
    var controls = el('div', 'pr-controls pr-test');
    var modeInput = document.createElement('input');
    modeInput.value = (f.modes && f.modes[0]) || 'run';
    modeInput.placeholder = 'mode';
    modeInput.className = 'pr-mode';
    var task = document.createElement('textarea');
    task.rows = 2;
    task.placeholder = 'Task text (the routine prompt) — {{objective}} and {{repo}} substitute as usual';
    var objective = document.createElement('input');
    objective.placeholder = 'objective (optional)';
    var repo = document.createElement('input');
    repo.placeholder = 'repository (optional)';
    repo.setAttribute('list', 'repo-names');
    var out = el('div');
    controls.appendChild(modeInput);
    controls.appendChild(task);
    controls.appendChild(objective);
    controls.appendChild(repo);
    controls.appendChild(button('Preview', 'primary', function () {
      var q = '?test=1&mode=' + encodeURIComponent(modeInput.value.trim()) +
        '&task=' + encodeURIComponent(task.value) +
        '&objective=' + encodeURIComponent(objective.value.trim()) +
        '&repo=' + encodeURIComponent(repo.value.trim());
      fetchJSON(promptURL(f.name, q)).then(function (r) {
        out.textContent = '';
        var t = r.test || {};
        var line = el('p', 'meta');
        line.textContent = 'model ' + (t.model || '?') + ' · mode ' + t.mode +
          (t.composition ? ' · composed from ' + (t.composition.fragments || []).map(function (fr) { return fr.name; }).join(', ') : '') +
          ' · ' + (t.prompt || '').length + ' bytes';
        out.appendChild(line);
        out.appendChild(pre(t.prompt || ''));
      }).catch(fail);
    }));
    detail.appendChild(controls);
    detail.appendChild(out);
  }

  // personaComposer: pick a mode, see the exact composed text and manifest.
  function personaComposer(f) {
    detail.appendChild(el('h3', '', 'Composed'));
    var row = el('div', 'pr-controls');
    var modeSel = document.createElement('select');
    ['(core only)'].concat(f.modes || []).forEach(function (m, i) {
      var o = document.createElement('option');
      o.value = i === 0 ? '' : m;
      o.textContent = i === 0 ? '(core only)' : 'mode: ' + m;
      modeSel.appendChild(o);
    });
    row.appendChild(modeSel);
    var out = el('div');
    function compose() {
      fetchJSON(promptURL(f.name, '?resolved=1&mode=' + encodeURIComponent(modeSel.value))).then(function (r) {
        out.textContent = '';
        var manifest = el('p', 'meta');
        manifest.textContent = 'Composed from ' + (r.composition.fragments || []).map(function (fr) { return fr.name; }).join(', ') +
          (r.composition.commit ? ' @ ' + r.composition.commit.slice(0, 8) + (r.composition.dirty ? ' (dirty)' : '') : ' (uncommitted tree)');
        out.appendChild(manifest);
        out.appendChild(pre(r.resolved));
      }).catch(fail);
    }
    modeSel.addEventListener('change', compose);
    detail.appendChild(row);
    detail.appendChild(out);
    compose();
  }

  // ---- routine detail: summary, edit/run, and the composed-prompt tester ----

  function showRoutine(name) {
    clearFail();
    routinesReady.then(function () {
      var rt = routines.filter(function (r) { return r.name === name; })[0];
      if (!rt) { fail(new Error('routine ' + name + ' not found')); return; }
      detail.textContent = '';
      var head = el('div', 'pr-head');
      head.appendChild(el('h2', '', rt.name));
      head.appendChild(chip('routine'));
      head.appendChild(chip('mode: ' + rt.mode));
      if (rt.model) head.appendChild(chip('model: ' + rt.model));
      if (rt.persona) head.appendChild(chip('persona: ' + rt.persona));
      if (rt.schedule) head.appendChild(chip(rt.schedule + (rt.schedule_enabled ? '' : ' (off)')));
      detail.appendChild(head);
      var meta = el('p', 'meta', 'gen ' + rt.generation + ' · ' + (rt.repositories || []).join(', ') + ' · ' + rt.budget_class + ' · priority ' + rt.priority);
      detail.appendChild(meta);

      var actions = el('div', 'pr-controls');
      actions.appendChild(button('Edit', '', function () {
        if (window.ForgeRoutines) window.ForgeRoutines.edit(rt.name);
      }));
      actions.appendChild(button('Run', '', function (e) {
        var btn = e.currentTarget;
        btn.disabled = true;
        fetch('/api/v1/routines/' + encodeURIComponent(rt.name) + '/run', { method: 'POST' })
          .then(function (resp) {
            if (!resp.ok) return resp.json().then(function (er) { throw new Error(er.error || resp.status); });
            btn.textContent = 'Run ✓';
            window.setTimeout(function () { btn.textContent = 'Run'; btn.disabled = false; }, 2000);
          })
          .catch(function (err) { fail(err); btn.disabled = false; });
      }));
      detail.appendChild(actions);

      detail.appendChild(label('Task text (the routine prompt)'));
      detail.appendChild(pre(rt.prompt || '(empty)'));

      // The tester: objective + repository → the byte-exact rendered prompt.
      detail.appendChild(el('h3', '', 'Test: the prompt the agent will read'));
      var controls = el('div', 'pr-controls pr-test');
      var objective = document.createElement('textarea');
      objective.rows = 2;
      objective.placeholder = 'Objective — substitutes {{objective}} (blank = the self-directed fallback)';
      var repoSel = document.createElement('select');
      (rt.repositories && rt.repositories.length ? rt.repositories : ['']).forEach(function (r) {
        var o = document.createElement('option');
        o.value = r;
        o.textContent = r || '(no repository)';
        repoSel.appendChild(o);
      });
      var out = el('div');
      controls.appendChild(objective);
      controls.appendChild(repoSel);
      controls.appendChild(button('Preview', 'primary', function () {
        fetchJSON('/api/v1/routines/' + encodeURIComponent(rt.name) + '/preview?objective=' + encodeURIComponent(objective.value.trim()) + '&repo=' + encodeURIComponent(repoSel.value)).then(function (p) {
          out.textContent = '';
          var line = el('p', 'meta');
          line.textContent = 'model ' + (p.model || '?') + ' · mode ' + p.mode +
            (p.composition ? ' · composed from ' + (p.composition.fragments || []).map(function (fr) { return fr.name; }).join(', ') : ' · no persona') +
            ' · ' + p.prompt.length + ' bytes';
          out.appendChild(line);
          out.appendChild(pre(p.prompt));
        }).catch(fail);
      }));
      detail.appendChild(controls);
      detail.appendChild(out);
    });
  }

  // ---- selection ----

  function select(sel) {
    page.querySelectorAll('.pr-item').forEach(function (b) {
      b.classList.toggle('on', b.dataset.sel === sel);
    });
    var kind = sel.split(':')[0];
    var name = sel.slice(kind.length + 1);
    if (kind === 'routine') showRoutine(name); else showFragment(name);
  }
  page.querySelectorAll('[data-sel]').forEach(function (b) {
    b.addEventListener('click', function () { select(b.dataset.sel); });
  });

  window.ForgePrompts = { select: select };
})();

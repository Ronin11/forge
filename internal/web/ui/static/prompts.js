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
  var modelAliases = ['haiku', 'sonnet', 'opus'];
  var libCommit = '';
  var modelsReady = fetchJSON('/api/v1/personas').then(function (lib) {
    if (lib.models && lib.models.length) modelAliases = lib.models;
    libCommit = lib.commit || '';
  }).catch(function () {});

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

  // ---- deep links: the selection (and composer mode) live in the URL ----

  function urlFor(sel, mode) {
    return '/routines?sel=' + encodeURIComponent(sel) + (mode ? '&mode=' + encodeURIComponent(mode) : '');
  }
  function currentParams() {
    var q = new URLSearchParams(window.location.search);
    return { sel: q.get('sel') || '', mode: q.get('mode') || '' };
  }
  // fragLink is an in-page link to another prompt; clicks route through
  // select() (delegated below) so navigation stays instant, while the href
  // keeps middle-click and copy-link honest.
  function fragLink(name) {
    var a = el('a', '', name);
    a.href = urlFor('prompt:' + name);
    a.setAttribute('data-nav', 'prompt:' + name);
    return a;
  }
  detail.addEventListener('click', function (e) {
    var a = e.target.closest && e.target.closest('a[data-nav]');
    if (!a) return;
    e.preventDefault();
    select(a.getAttribute('data-nav'), { push: true });
  });

  // ---- fragment / persona detail ----

  function promptURL(name, query) {
    return '/api/v1/prompts/' + name.split('/').map(encodeURIComponent).join('/') + (query || '');
  }

  function showFragment(name, initialMode) {
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
        personaComposer(f, initialMode);
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
    runPanel(detail, f.model, function () {
      return { persona: f.name, mode: modeInput.value.trim(), task: task.value, objective: objective.value.trim(), repo: repo.value.trim() };
    }, 'persona:' + f.name, function (t) {
      if (t.mode) modeInput.value = t.mode;
      task.value = t.task || '';
      objective.value = t.objective || '';
      repo.value = t.repo || '';
    });
    optimizePanel(detail, 'persona:' + f.name, f.model, function () {
      return { mode: modeInput.value.trim(), task: task.value, objective: objective.value.trim(), repo: repo.value.trim() };
    }, function (content) {
      fetch(promptURL(f.name), { method: 'PUT', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ content: content }) })
        .then(function (resp) {
          if (!resp.ok) return resp.json().then(function (er) { throw new Error(er.error || resp.status); });
          clearFail();
          showFragment(f.name);
        })
        .catch(fail);
    });
  }

  function relTime(iso) {
    var s = (Date.now() - new Date(iso).getTime()) / 1000;
    if (s < 90) return Math.round(s) + 's ago';
    if (s < 5400) return Math.round(s / 60) + 'm ago';
    if (s < 129600) return Math.round(s / 3600) + 'h ago';
    return Math.round(s / 86400) + 'd ago';
  }

  // testRecord renders one saved run: when, model, inputs, output — and a
  // Load-inputs button that refills the tester, which is the iterate loop.
  function testRecord(t, setInputs, modelSel) {
    var box = el('div', 'pr-run');
    var line = el('p', 'meta');
    var drift = t.composition && libCommit && t.composition.commit && t.composition.commit !== libCommit;
    line.textContent = relTime(t.created_at) + ' · ' + t.model + ' · ' + (t.elapsed_ms / 1000).toFixed(1) + 's' +
      (t.mode ? ' · mode ' + t.mode : '') + (t.repo ? ' · repo ' + t.repo : '') +
      (drift ? ' · library has changed since this run' : '');
    box.appendChild(line);
    if (t.task || t.objective) {
      var inputs = el('p', 'meta');
      inputs.textContent = (t.task ? 'task: ' + t.task.slice(0, 120) : '') + (t.objective ? '  ·  objective: ' + t.objective.slice(0, 120) : '');
      box.appendChild(inputs);
    }
    if (setInputs) box.appendChild(button('Load these inputs', '', function () {
      setInputs(t);
      if (modelSel) modelSel.value = modelAliases.indexOf(t.model) >= 0 ? t.model : '';
    }));
    box.appendChild(pre(t.output || '(no output recorded)'));
    return box;
  }

  // runPanel appends a model picker, a Run button, an output pane, and the
  // subject's run history; body() assembles the prompt-test request at click
  // time. One click = one real model completion at the chosen size — a
  // prompt smoke, not an agent run.
  function runPanel(parent, defaultModel, body, subject, setInputs) {
    var row = el('div', 'pr-controls');
    var modelSel = document.createElement('select');
    modelsReady.then(function () {
      var def = document.createElement('option');
      def.value = '';
      def.textContent = defaultModel ? 'model: ' + defaultModel + ' (default)' : 'model: (routine default)';
      modelSel.appendChild(def);
      modelAliases.forEach(function (m) {
        var o = document.createElement('option');
        o.value = m;
        o.textContent = 'model: ' + m;
        modelSel.appendChild(o);
      });
    });
    var out = el('div');
    row.appendChild(modelSel);
    row.appendChild(button('Run test', '', function (e) {
      var btn = e.currentTarget;
      btn.disabled = true;
      btn.textContent = 'Running…';
      out.textContent = '';
      out.appendChild(el('p', 'meta', 'Waiting for the model — a real completion, typically a few seconds…'));
      var req = body();
      req.model = modelSel.value;
      fetch('/api/v1/prompt-test', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(req) })
        .then(function (resp) {
          if (!resp.ok) return resp.json().then(function (er) { throw new Error(er.error || resp.status); });
          return resp.json();
        })
        .then(function (r) {
          out.textContent = '';
          out.appendChild(el('p', 'meta', 'ran on ' + r.model + ' · ' + (r.elapsed_ms / 1000).toFixed(1) + 's · prompt ' + r.prompt.length + ' bytes → output ' + r.output.length + ' bytes'));
          out.appendChild(label('Model output'));
          out.appendChild(pre(r.output || '(empty)'));
        })
        .catch(function (err) { out.textContent = ''; fail(err); })
        .then(function () { btn.disabled = false; btn.textContent = 'Run test'; loadHistory(); });
    }));
    parent.appendChild(row);
    parent.appendChild(out);

    // History: the latest run in full, older ones collapsed — coming back to
    // a prompt shows what it last did with which inputs.
    var history = el('div');
    parent.appendChild(history);
    function loadHistory() {
      if (!subject) return;
      fetchJSON('/api/v1/prompt-tests?subject=' + encodeURIComponent(subject)).then(function (tests) {
        history.textContent = '';
        if (!tests.length) return;
        history.appendChild(label('Last test run'));
        history.appendChild(testRecord(tests[0], setInputs, modelSel));
        if (tests.length > 1) {
          var older = document.createElement('details');
          var sum = document.createElement('summary');
          sum.textContent = (tests.length - 1) + ' earlier run(s)';
          older.appendChild(sum);
          tests.slice(1).forEach(function (t) { older.appendChild(testRecord(t, setInputs, modelSel)); });
          history.appendChild(older);
        }
      }).catch(function () {});
    }
    loadHistory();
  }

  // ---- LLM-assisted optimization ----

  // optimizePanel: state a goal, pick the model it must run well on and the
  // (big) optimizer model, and start an experiment — the optimizer proposes
  // variants, every variant plus the untouched baseline runs the tester
  // inputs on the target model, and the optimizer judges the outputs blind.
  // Nothing changes until a variant's Apply, which goes through the same
  // validated save path as a hand edit.
  function optimizePanel(parent, subject, defaultTarget, buildTest, apply) {
    parent.appendChild(el('h3', '', 'Optimize: have a big model propose and test variants'));
    var box = el('div', 'pr-optimize');
    parent.appendChild(box);
    var controls = el('div', 'pr-controls');
    var goal = document.createElement('textarea');
    goal.rows = 2;
    goal.placeholder = 'Goal — what should this do better? e.g. "handle empty repos without inventing work" or "hold up on sonnet"';
    var target = document.createElement('select');
    var optimizer = document.createElement('select');
    var count = document.createElement('input');
    count.type = 'number';
    count.min = 1;
    count.max = 12;
    count.value = 8;
    count.title = 'how many variants to try';
    count.className = 'pr-variants';
    modelsReady.then(function () {
      modelAliases.forEach(function (m) {
        [target, optimizer].forEach(function (sel, i) {
          var o = document.createElement('option');
          o.value = m;
          o.textContent = (i ? 'optimizer: ' : 'run on: ') + m;
          sel.appendChild(o);
        });
      });
      if (defaultTarget && modelAliases.indexOf(defaultTarget) >= 0) target.value = defaultTarget;
      // The optimizer defaults to the biggest model available.
      if (modelAliases.indexOf('opus') >= 0) optimizer.value = 'opus';
      else if (modelAliases.length) optimizer.value = modelAliases[modelAliases.length - 1];
    });
    var out = el('div');
    controls.appendChild(goal);
    controls.appendChild(target);
    controls.appendChild(optimizer);
    controls.appendChild(count);
    var startBtn = button('Start experiment', 'primary', function () {
      if (!goal.value.trim()) { fail(new Error('state the goal first — what should this do better?')); return; }
      clearFail();
      startBtn.disabled = true;
      fetch('/api/v1/experiments', {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          subject: subject, goal: goal.value.trim(), target_model: target.value,
          optimizer_model: optimizer.value, variants: +count.value || 8, test: buildTest(),
        }),
      })
        .then(function (resp) {
          if (!resp.ok) return resp.json().then(function (er) { throw new Error(er.error || resp.status); });
          return resp.json();
        })
        .then(function (r) { poll(r.id); })
        .catch(function (err) { fail(err); startBtn.disabled = false; });
    });
    controls.appendChild(startBtn);
    box.appendChild(controls);
    box.appendChild(out);

    function poll(id) {
      if (!box.isConnected) return; // the detail pane moved on
      fetchJSON('/api/v1/experiments?id=' + encodeURIComponent(id)).then(function (pe) {
        render(pe);
        if (pe.status === 'running') window.setTimeout(function () { poll(id); }, 3000);
      }).catch(fail);
    }

    function candidateBox(c) {
      var cb = el('div', 'pr-run pr-cand');
      var head = el('p', '');
      head.appendChild(el('strong', '', c.title));
      if (c.baseline) head.appendChild(chip('current'));
      if (c.error) head.appendChild(chip('not run'));
      else head.appendChild(chip('score ' + c.score));
      cb.appendChild(head);
      if (c.rationale) cb.appendChild(el('p', 'meta', c.rationale));
      if (c.error) cb.appendChild(el('p', 'meta', c.error));
      if (c.judge_rationale) cb.appendChild(el('p', 'meta', 'judge: ' + c.judge_rationale));
      function fold(title, text) {
        var d = document.createElement('details');
        var sum = document.createElement('summary');
        sum.textContent = title;
        d.appendChild(sum);
        d.appendChild(pre(text));
        cb.appendChild(d);
      }
      if (c.output) fold('model output (' + c.output.length + ' bytes)', c.output);
      if (!c.baseline) {
        fold('proposed content', c.content);
        if (!c.error) cb.appendChild(button('Apply this variant', '', function (e) {
          e.currentTarget.disabled = true;
          apply(c.content);
        }));
      }
      return cb;
    }

    function render(pe) {
      out.textContent = '';
      startBtn.disabled = pe.status === 'running';
      if (pe.goal) {
        out.appendChild(el('p', 'meta', 'experiment ' + relTime(pe.created_at) + ' · goal: ' + pe.goal +
          ' · ran on ' + pe.target_model + ', optimized by ' + pe.optimizer_model));
      }
      if (pe.status === 'running') {
        out.appendChild(el('p', 'meta', 'running — ' + (pe.progress || 'starting') + ' …'));
        return;
      }
      if (pe.status === 'failed') {
        out.appendChild(el('p', 'meta', 'failed: ' + (pe.error || 'unknown error')));
        return;
      }
      var results = pe.results || {};
      if (results.summary) out.appendChild(el('p', '', results.summary));
      (results.candidates || []).forEach(function (c) { out.appendChild(candidateBox(c)); });
    }

    // Coming back to the page shows the latest experiment where it stands —
    // and picks the polling back up if one is still running.
    fetchJSON('/api/v1/experiments?subject=' + encodeURIComponent(subject)).then(function (list) {
      if (!list || !list.length) return;
      render(list[0]);
      if (list[0].status === 'running') window.setTimeout(function () { poll(list[0].id); }, 3000);
    }).catch(function () {});
  }

  // personaComposer: pick a mode, see the exact composed text and manifest.
  function personaComposer(f, initialMode) {
    detail.appendChild(el('h3', '', 'Composed'));
    var row = el('div', 'pr-controls');
    var modeSel = document.createElement('select');
    ['(core only)'].concat(f.modes || []).forEach(function (m, i) {
      var o = document.createElement('option');
      o.value = i === 0 ? '' : m;
      o.textContent = i === 0 ? '(core only)' : 'mode: ' + m;
      modeSel.appendChild(o);
    });
    if (initialMode && (f.modes || []).indexOf(initialMode) >= 0) modeSel.value = initialMode;
    row.appendChild(modeSel);
    var out = el('div');
    function compose() {
      fetchJSON(promptURL(f.name, '?resolved=1&mode=' + encodeURIComponent(modeSel.value))).then(function (r) {
        out.textContent = '';
        var manifest = el('p', 'meta');
        manifest.appendChild(document.createTextNode('Composed from '));
        (r.composition.fragments || []).forEach(function (fr, i) {
          if (i > 0) manifest.appendChild(document.createTextNode(', '));
          manifest.appendChild(fragLink(fr.name));
        });
        manifest.appendChild(document.createTextNode(
          r.composition.commit ? ' @ ' + r.composition.commit.slice(0, 8) + (r.composition.dirty ? ' (dirty)' : '') : ' (uncommitted tree)'));
        out.appendChild(manifest);
        out.appendChild(pre(r.resolved));
      }).catch(fail);
    }
    modeSel.addEventListener('change', function () {
      window.history.replaceState({}, '', urlFor('prompt:' + f.name, modeSel.value));
      compose();
    });
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
      if (rt.persona) {
        var pchip = chip('persona: ');
        pchip.appendChild(fragLink(rt.persona));
        head.appendChild(pchip);
      }
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
          line.appendChild(document.createTextNode('model ' + (p.model || '?') + ' · mode ' + p.mode + ' · '));
          if (p.composition) {
            line.appendChild(document.createTextNode('composed from '));
            (p.composition.fragments || []).forEach(function (fr, i) {
              if (i > 0) line.appendChild(document.createTextNode(', '));
              line.appendChild(fragLink(fr.name));
            });
          } else {
            line.appendChild(document.createTextNode('no persona'));
          }
          line.appendChild(document.createTextNode(' · ' + p.prompt.length + ' bytes'));
          out.appendChild(line);
          out.appendChild(pre(p.prompt));
        }).catch(fail);
      }));
      detail.appendChild(controls);
      detail.appendChild(out);
      runPanel(detail, rt.model, function () {
        return { routine: rt.name, objective: objective.value.trim(), repo: repoSel.value };
      }, 'routine:' + rt.name, function (t) {
        objective.value = t.objective || '';
        if (t.repo) repoSel.value = t.repo;
      });
      optimizePanel(detail, 'routine:' + rt.name, rt.model, function () {
        return { objective: objective.value.trim(), repo: repoSel.value };
      }, function (content) {
        // Applying a routine variant replaces only the task prompt, against
        // the routine's current generation.
        fetchJSON('/api/v1/routines/' + encodeURIComponent(rt.name)).then(function (fresh) {
          fresh.prompt = content;
          return fetch('/api/v1/routines/' + encodeURIComponent(rt.name) + '?generation=' + fresh.generation, {
            method: 'PUT', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(fresh),
          });
        })
          .then(function (resp) {
            if (!resp.ok) return resp.json().then(function (er) { throw new Error(er.error || resp.status); });
            clearFail();
            return fetchJSON('/api/v1/routines').then(function (list) { routines = list || []; showRoutine(rt.name); });
          })
          .catch(fail);
      });
    });
  }

  // ---- selection ----

  function select(sel, opts) {
    opts = opts || {};
    page.querySelectorAll('.pr-item').forEach(function (b) {
      b.classList.toggle('on', b.dataset.sel === sel);
    });
    if (opts.push) window.history.pushState({}, '', urlFor(sel, opts.mode));
    var kind = sel.split(':')[0];
    var name = sel.slice(kind.length + 1);
    if (kind === 'routine') showRoutine(name); else showFragment(name, opts.mode);
  }
  page.querySelectorAll('[data-sel]').forEach(function (b) {
    b.addEventListener('click', function (e) {
      e.preventDefault();
      select(b.dataset.sel, { push: true });
    });
  });
  window.addEventListener('popstate', function () {
    var p = currentParams();
    if (p.sel) select(p.sel, { mode: p.mode });
  });
  // Deep link: /routines?sel=prompt:<name>[&mode=<mode>] selects on load.
  var boot = currentParams();
  if (boot.sel) select(boot.sel, { mode: boot.mode });

  window.ForgePrompts = { select: select };
})();

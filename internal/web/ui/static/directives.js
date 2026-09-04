// The Directives page (directives.html): the library tree is server-rendered;
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

  var modelAliases = ['haiku', 'sonnet', 'opus'];
  var modelPrices = {}; // alias → {input, output} in $/MTok
  var libCommit = '';
  var modelsReady = fetchJSON('/api/v1/personas').then(function (lib) {
    if (lib.models && lib.models.length) modelAliases = lib.models;
    modelPrices = lib.model_prices || {};
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
    return '/directives?sel=' + encodeURIComponent(sel) + (mode ? '&mode=' + encodeURIComponent(mode) : '');
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
    return '/api/v1/directives/' + name.split('/').map(encodeURIComponent).join('/') + (query || '');
  }

  function showFragment(name, initialMode) {
    clearFail();
    fetchJSON(promptURL(name)).then(function (f) {
      detail.textContent = '';
      var head = el('div', 'pr-head');
      head.appendChild(el('h2', '', f.name));
      head.appendChild(chip(f.persona ? 'persona' : (f.directive ? 'directive' : (f.script ? 'script' : 'fragment'))));
      if (f.model) head.appendChild(chip('model: ' + f.model));
      if (f.directive && f.mode) head.appendChild(chip('mode: ' + f.mode));
      if (f.tool) head.appendChild(chip('tool'));
      if (f.script && f.timeout_ms) head.appendChild(chip(f.timeout_ms + 'ms cap'));
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
      if (f.directive) directiveTester(f);
      if (f.script) scriptTester(f);
    }).catch(fail);
  }

  // scriptTester: run the script in the sandbox with a JSON params payload —
  // pure compute, instant, no model involved.
  function scriptTester(f) {
    if (f.description) detail.appendChild(el('p', 'meta', f.description));
    detail.appendChild(el('h3', '', 'Test: run in the sandbox'));
    var controls = el('div', 'pr-controls pr-test');
    var input = document.createElement('textarea');
    input.rows = 3;
    input.placeholder = f.input_schema ? 'JSON input (schema: ' + f.input_schema.slice(0, 120) + ')' : 'JSON input → input.params (optional)';
    var out = el('div');
    controls.appendChild(input);
    controls.appendChild(button('Run script', 'primary', function (e) {
      var btn = e.currentTarget;
      btn.disabled = true;
      var body = { name: f.name };
      var raw = input.value.trim();
      if (raw) {
        try { body.input = JSON.parse(raw); } catch (err) { fail(new Error('input is not JSON: ' + err.message)); btn.disabled = false; return; }
      }
      clearFail();
      fetch('/api/v1/script-test', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body) })
        .then(function (resp) {
          if (!resp.ok) return resp.json().then(function (er) { throw new Error(er.error || resp.status); });
          return resp.json();
        })
        .then(function (r) {
          out.textContent = '';
          if (r.error) {
            out.appendChild(el('p', 'meta', 'failed in ' + r.elapsed_ms + 'ms'));
            out.appendChild(pre(r.error));
          } else {
            out.appendChild(el('p', 'meta', 'ran in ' + r.elapsed_ms + 'ms'));
            out.appendChild(pre(JSON.stringify(r.output, null, 2)));
          }
        })
        .catch(fail)
        .then(function () { btn.disabled = false; });
    }));
    detail.appendChild(controls);
    detail.appendChild(out);
  }

  // directiveTester: the composed body, then the exact prompt a run of this
  // directive would read — objective and repository from the inputs — with a
  // real-model run panel recording under directive:<name>.
  function directiveTester(f) {
    detail.appendChild(el('h3', '', 'Composed body'));
    var composed = el('div');
    detail.appendChild(composed);
    fetchJSON(promptURL(f.name, '?resolved=1')).then(function (r) {
      var manifest = el('p', 'meta');
      manifest.appendChild(document.createTextNode('Composed from '));
      (r.composition.fragments || []).forEach(function (fr, i) {
        if (i > 0) manifest.appendChild(document.createTextNode(', '));
        manifest.appendChild(fragLink(fr.name));
      });
      composed.appendChild(manifest);
      composed.appendChild(pre(r.resolved));
    }).catch(fail);

    detail.appendChild(el('h3', '', 'Test: the prompt a run of this directive would read'));
    var controls = el('div', 'pr-controls pr-test');
    var objective = document.createElement('textarea');
    objective.rows = 2;
    objective.placeholder = 'Objective — substitutes {{objective}} (blank = the self-directed fallback)';
    var repo = document.createElement('input');
    repo.placeholder = 'repository (optional)';
    repo.setAttribute('list', 'repo-names');
    var out = el('div');
    controls.appendChild(objective);
    controls.appendChild(repo);
    controls.appendChild(button('Preview', 'primary', function () {
      var q = '?test=1&objective=' + encodeURIComponent(objective.value.trim()) + '&repo=' + encodeURIComponent(repo.value.trim());
      fetchJSON(promptURL(f.name, q)).then(function (r) {
        out.textContent = '';
        var t = r.test || {};
        out.appendChild(el('p', 'meta', 'model ' + (t.model || '?') + ' · mode ' + t.mode + ' · ' + (t.prompt || '').length + ' bytes'));
        out.appendChild(pre(t.prompt || ''));
      }).catch(fail);
    }));
    detail.appendChild(controls);
    detail.appendChild(out);
    runPanel(detail, f.model, function () {
      return { directive: f.name, objective: objective.value.trim(), repo: repo.value.trim() };
    }, 'directive:' + f.name, function (t) {
      objective.value = t.objective || '';
      repo.value = t.repo || '';
    });
    optimizePanel(detail, 'directive:' + f.name, f.model, function () {
      return { objective: objective.value.trim(), repo: repo.value.trim() };
    }, function (content) {
      fetch(promptURL(f.name), { method: 'PUT', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ content: content }) })
        .then(function (resp) {
          if (!resp.ok) return resp.json().then(function (er) { throw new Error(er.error || resp.status); });
          clearFail();
          showFragment(f.name);
        })
        .catch(fail);
    }, {
      baselineChars: rawOf(f).length,
      promptChars: fetchJSON(promptURL(f.name, '?test=1')).then(function (r) { return ((r.test || {}).prompt || '').length; }),
    });
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
      applyPersona(content);
    }, {
      baselineChars: rawOf(f).length,
      promptChars: fetchJSON(promptURL(f.name, '?test=1&mode=' + encodeURIComponent((f.modes && f.modes[0]) || 'run') + '&task='))
        .then(function (r) { return ((r.test || {}).prompt || '').length; }),
    });
    function applyPersona(content) {
      fetch(promptURL(f.name), { method: 'PUT', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ content: content }) })
        .then(function (resp) {
          if (!resp.ok) return resp.json().then(function (er) { throw new Error(er.error || resp.status); });
          clearFail();
          showFragment(f.name);
        })
        .catch(fail);
    }
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
      fetch('/api/v1/directive-test', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(req) })
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
      fetchJSON('/api/v1/directive-tests?subject=' + encodeURIComponent(subject)).then(function (tests) {
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
  // sizing carries what the cost estimate needs: the subject's content
  // length in characters and a promise of the full composed prompt's length
  // (preambles and fragments included — what a test run actually sends).
  function optimizePanel(parent, subject, defaultTarget, buildTest, apply, sizing) {
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
      if (modelAliases.indexOf('fable') >= 0) optimizer.value = 'fable';
      else if (modelAliases.indexOf('opus') >= 0) optimizer.value = 'opus';
      else if (modelAliases.length) optimizer.value = modelAliases[modelAliases.length - 1];
      estimate();
    });

    // The cost estimate: list prices × a rough token model. Input tokens
    // come from the real composed prompt (chars/4); run output is assumed
    // ~700 tokens; the generation call's output is the dominant optimizer
    // cost since every variant is a complete rewrite of the content.
    var promptTokens = Math.ceil((sizing && sizing.baselineChars || 2000) / 4);
    var baselineTokens = Math.ceil((sizing && sizing.baselineChars || 2000) / 4);
    if (sizing && sizing.promptChars) sizing.promptChars.then(function (n) {
      if (n) { promptTokens = Math.ceil(n / 4); estimate(); }
    }).catch(function () {});
    var costLine = el('p', 'meta pr-cost');
    costLine.title = 'List-price estimate: composed prompt ≈ chars/4 input tokens per run, ~700 output tokens per run, ' +
      'plus one generation call (writes every variant in full) and one judging call on the optimizer. Actual spend varies with output length.';
    function estimate() {
      var tp = modelPrices[target.value];
      var op = modelPrices[optimizer.value];
      var V = Math.min(12, Math.max(1, +count.value || 8));
      if (!tp || !op) { costLine.textContent = ''; return; }
      var runOut = 700;
      var usd = (V + 1) * (promptTokens * tp.input + runOut * tp.output) / 1e6 +
        ((baselineTokens + 600) * op.input + (V * baselineTokens + 300) * op.output) / 1e6 +
        (((V + 1) * runOut + 400) * op.input + 250 * op.output) / 1e6;
      costLine.textContent = 'expected cost ≈ $' + (usd < 0.095 ? usd.toFixed(3) : usd.toFixed(2)) +
        ' — ' + (V + 1) + ' runs on ' + target.value + ' + 2 ' + optimizer.value + ' calls';
    }
    [target, optimizer, count].forEach(function (input) {
      input.addEventListener('change', estimate);
      input.addEventListener('input', estimate);
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
    box.appendChild(costLine);
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

  // ---- selection ----

  function select(sel, opts) {
    opts = opts || {};
    page.querySelectorAll('.pr-item').forEach(function (b) {
      b.classList.toggle('on', b.dataset.sel === sel);
    });
    if (opts.push) window.history.pushState({}, '', urlFor(sel, opts.mode));
    var kind = sel.split(':')[0];
    var name = sel.slice(kind.length + 1);
    showFragment(name, opts.mode);
  }
  // The tree filter: hides non-matching items (name match) and folders that
  // end up empty. Server search ranks better; this is the quick narrow.
  var filter = page.querySelector('[data-tree-filter]');
  if (filter) filter.addEventListener('input', function () {
    var q = filter.value.trim().toLowerCase();
    page.querySelectorAll('.pr-tree li').forEach(function (li) {
      var item = li.querySelector('.pr-item');
      if (!item) return;
      li.hidden = q !== '' && item.textContent.toLowerCase().indexOf(q) < 0;
    });
    if (q !== '') page.querySelectorAll('.pr-sec').forEach(function (d) { d.open = true; });
  });

  // Collapsible sections, remembered per folder.
  page.querySelectorAll('.pr-sec').forEach(function (d) {
    var key = 'forge.tree.' + d.dataset.sec;
    try { if (localStorage.getItem(key) === 'closed') d.open = false; } catch (e) { /* storage unavailable */ }
    d.addEventListener('toggle', function () {
      try { localStorage.setItem(key, d.open ? 'open' : 'closed'); } catch (e) { /* storage unavailable */ }
    });
  });

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
  // Deep link: /directives?sel=prompt:<name>[&mode=<mode>] selects on load.
  var boot = currentParams();
  if (boot.sel) select(boot.sel, { mode: boot.mode });

  window.ForgeDirectives = { select: select };
})();

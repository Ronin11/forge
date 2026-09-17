(() => {
  const $ = s => document.querySelector(s);
  const esc = s => String(s ?? '').replace(/[&<>"]/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;'}[c]));
  const usd = n => '$' + (Number(n) || 0).toFixed(2);
  const secs = ms => ((ms || 0) / 1000).toFixed(0) + 's';
  const PAGE = 100;
  let offset = 0, feed = [], es = null, view = null;

  // Which events invalidate which view — matches docs/CLIENT.md's "What
  // to re-read on which event".
  const INVALIDATES = {
    list: ['task_queued', 'task_started', 'task_done', 'attempt_done', 'pushed'],
    detail: ['task_done', 'attempt_done', 'deploy_finished'],
    run: ['task_done', 'attempt_done', 'op'],
  };

  async function call(method, path) {
    const r = await fetch(path, { method, credentials: 'same-origin' });
    if (r.status === 401) { document.body.innerHTML = '<p style="margin:2em">Not signed in: open the link forge-web printed when it started.</p>'; throw new Error('401'); }
    return r.json();
  }
  const get = path => call('GET', path);
  const post = path => call('POST', path);

  // ---- routing: /tasks, /tasks/:id, /tasks/:id/run, /plugins, /projects,
  // /projects/:name, /initiatives/:id, /graph, /stats
  function route() {
    if (location.pathname === '/plugins') return { page: 'plugins' };
    if (location.pathname === '/projects') return { page: 'projects' };
    if (location.pathname === '/stats') return { page: 'stats' };
    if (location.pathname === '/graph') return { page: 'graph', repo: new URLSearchParams(location.search).get('repo') || '' };
    let m = location.pathname.match(/^\/projects\/([^/]+)\/?$/);
    if (m) return { page: 'project', name: decodeURIComponent(m[1]) };
    m = location.pathname.match(/^\/initiatives\/(\d+)\/?$/);
    if (m) return { page: 'initiative', id: Number(m[1]) };
    m = location.pathname.match(/^\/tasks(?:\/(\d+)(\/run)?)?\/?$/);
    if (!m) { history.replaceState(null, '', '/tasks'); return route(); }
    return { page: 'tasks', id: m[1] ? Number(m[1]) : null, run: !!m[2] };
  }
  function go(path) { history.pushState(null, '', path); render(); }
  document.addEventListener('click', ev => {
    const a = ev.target.closest('a[href^="/tasks"], a[href="/plugins"], a[href^="/projects"], a[href^="/initiatives"], a[href^="/graph"], a[href="/stats"]');
    if (a && !ev.metaKey && !ev.ctrlKey) { ev.preventDefault(); go(a.getAttribute('href')); }
  });
  window.addEventListener('popstate', render);

  function nav(r) {
    const taskLinks = r.page === 'tasks' && r.id !== null
      ? ` <a href="/tasks/${r.id}" ${!r.run ? 'style="font-weight:600"' : ''}>task ${r.id}</a> <a href="/tasks/${r.id}/run" ${r.run ? 'style="font-weight:600"' : ''}>workflow run</a>`
      : '';
    const projects = r.page === 'projects' || r.page === 'project';
    $('#nav').innerHTML = `<a href="/tasks" ${r.page === 'tasks' && r.id === null ? 'style="font-weight:600"' : ''}>tasks</a>${taskLinks} <a href="/projects" ${projects ? 'style="font-weight:600"' : ''}>projects</a> <a href="/plugins" ${r.page === 'plugins' ? 'style="font-weight:600"' : ''}>plugins</a> <a href="/stats" ${r.page === 'stats' ? 'style="font-weight:600"' : ''}>stats</a>`;
  }

  async function render() {
    const r = route();
    nav(r);
    if (view && view.teardown) view.teardown();
    view = r.page === 'plugins' ? pluginsView()
      : r.page === 'projects' ? projectsView()
      : r.page === 'project' ? projectView(r.name)
      : r.page === 'initiative' ? initiativeView(r.id)
      : r.page === 'graph' ? graphView(r.repo)
      : r.page === 'stats' ? statsView()
      : (r.id === null ? listView() : (r.run ? runView(r.id) : detailView(r.id)));
    await view.show();
  }

  // ---- shared: worker + live stream
  function renderWorker(w) {
    $('#worker').textContent = w.running ? `worker pid ${w.pid}${w.stale_binary ? ' (stale binary)' : ''}` : 'worker not running';
    $('#worker').className = w.running ? '' : 'failed';
  }
  async function snapshotHead() {
    const s = await get('/api/snapshot');
    renderWorker(s.worker || {});
    if (!es) { offset = s.events_offset || 0; subscribe(); }
    return s;
  }
  function subscribe() {
    es = new EventSource(`/api/events?since=${offset}`);
    es.onopen = () => { $('#live').textContent = 'live'; $('#live').className = 'succeeded'; };
    es.onerror = () => { $('#live').textContent = 'reconnecting'; $('#live').className = 'failed'; };
    es.onmessage = m => {
      let e; try { e = JSON.parse(m.data); } catch { return; }
      feed.push(e); if (feed.length > 5000) feed.splice(0, feed.length - 5000);
      if (view && view.onEvent) view.onEvent(e);
    };
  }

  // ---- list view
  function listView() {
    let rows = [], done = false, loading = false, filters = { q: '', state: '', workflow: '', project: '' };
    let observer = null, debounce = null;
    const qs = before => {
      const p = new URLSearchParams({ limit: PAGE });
      if (before) p.set('before', before);
      for (const [k, v] of Object.entries(filters)) if (v) p.set(k, v);
      return '/api/tasks?' + p;
    };
    async function page(reset) {
      if (loading) return; loading = true;
      try {
        const before = reset ? null : (rows.length ? Math.min(...rows.map(t => t.id)) : null);
        if (!reset && done) return;
        const got = await get(qs(before));
        if (reset) rows = got; else rows = rows.concat(got.filter(t => !rows.some(r => r.id === t.id)));
        done = got.length < PAGE;
        drawRows();
      } finally { loading = false; }
    }
    async function refreshHead() {
      // Newest page again: updates rows already shown, prepends new ones.
      const got = await get(qs(null));
      const known = new Map(rows.map(t => [t.id, t]));
      for (const t of got) known.set(t.id, t);
      rows = [...known.values()].sort((a, b) => b.id - a.id);
      drawRows();
    }
    function drawRows() {
      const wfs = [...new Set(rows.map(t => t.workflow))].sort();
      const sel = $('#f-workflow');
      if (sel && sel.options.length !== wfs.length + 1) {
        const cur = sel.value;
        sel.innerHTML = '<option value="">any workflow</option>' + wfs.map(w => `<option ${w === cur ? 'selected' : ''}>${esc(w)}</option>`).join('');
      }
      $('#tasks').innerHTML = rows.map(t => `
        <tr class="task" data-id="${t.id}">
          <td class="num">${t.id}</td>
          <td class="state ${esc(t.state)}">${esc(t.state)}</td>
          <td>${esc(t.workflow)}</td>
          <td class="num">${t.attempts}</td>
          <td class="num">${usd(t.cost_usd)}</td>
          <td class="num">${t.initiative != null ? `<a href="/initiatives/${t.initiative}">${t.initiative}</a>` : ''}</td>
          <td class="mute" style="white-space:nowrap">${esc((t.created || '').slice(5, 16))}</td>
          <td class="task-text" title="${esc(t.task)}">${esc(t.task)}</td>
        </tr>`).join('');
      $('#sentinel').textContent = done ? (rows.length ? `${rows.length} task(s)` : 'no tasks match') : 'loading more…';
    }
    function renderRequests(reqs) {
      $('#requests').innerHTML = reqs.length ? reqs.map(r => `
        <div class="card req"><b><a href="/tasks/${r.id}">${r.id}</a></b> <span class="mute">${esc(r.kind)}${r.path ? ' · ' + esc(r.path) : ''}</span>
          <div>${esc(r.text)}</div>
          ${r.tried ? `<details><summary>tried</summary><div class="mute">${esc(r.tried)}</div></details>` : ''}
          <div class="mute">answer with <code>forge answer ${r.id} "…"</code></div></div>`).join('')
        : '<div class="card mute">No open questions.</div>';
    }
    return {
      async show() {
        $('#main').innerHTML = `
          <h2>Requests</h2><div id="requests"></div>
          <h2>Tasks</h2>
          <div class="filters">
            <input type="search" id="f-q" placeholder="search text or id" value="${esc(filters.q)}">
            <select id="f-state"><option value="">any state</option>${['queued','running','succeeded','failed','blocked','unverified','withdrawn'].map(s => `<option>${s}</option>`).join('')}</select>
            <select id="f-workflow"><option value="">any workflow</option></select>
            <select id="f-project"><option value="">any project</option></select>
          </div>
          <table><thead><tr><th>id</th><th>state</th><th>wf</th><th class="num">att</th><th class="num">cost</th><th>init</th><th>created</th><th>task</th></tr></thead><tbody id="tasks"></tbody></table>
          <div id="sentinel" class="sentinel">loading…</div>`;
        $('#f-q').addEventListener('input', ev => { clearTimeout(debounce); debounce = setTimeout(() => { filters.q = ev.target.value.trim(); page(true); }, 250); });
        $('#f-state').addEventListener('change', ev => { filters.state = ev.target.value; page(true); });
        $('#f-workflow').addEventListener('change', ev => { filters.workflow = ev.target.value; page(true); });
        $('#f-project').addEventListener('change', ev => { filters.project = ev.target.value; page(true); });
        get('/api/projects').then(rows => {
          $('#f-project').innerHTML = '<option value="">any project</option>' + rows.map(p => `<option value="${esc(p.name)}">${esc(p.name)}</option>`).join('');
        }).catch(() => {});
        $('#tasks').addEventListener('click', ev => {
          if (ev.target.closest('a')) return;
          const tr = ev.target.closest('tr.task');
          if (tr) go(`/tasks/${tr.dataset.id}`);
        });
        observer = new IntersectionObserver(entries => { if (entries.some(e => e.isIntersecting)) page(false); }, { rootMargin: '400px' });
        observer.observe($('#sentinel'));
        const s = await snapshotHead();
        renderRequests(s.requests || []);
        await page(true);
      },
      onEvent(e) {
        if (INVALIDATES.list.includes(e.type)) {
          refreshHead().catch(() => {});
          snapshotHead().then(s => renderRequests(s.requests || [])).catch(() => {});
        }
      },
      teardown() { if (observer) observer.disconnect(); clearTimeout(debounce); },
    };
  }

  // ---- detail view
  function detailView(id) {
    function renderFeed() {
      const rowsEl = $('#feed'); if (!rowsEl) return;
      const rows = feed.filter(e => e.task === id).slice(-300);
      rowsEl.innerHTML = rows.map(e => `<div><span class="t">${new Date((e.ts || 0) * 1000).toLocaleTimeString()}</span>${esc(e.text || e.type)}</div>`).join('');
      rowsEl.lastElementChild?.scrollIntoView({ block: 'nearest' });
    }
    async function draw() {
      const d = await get(`/api/task/${id}`);
      const t = d.task || {};
      const attempts = (d.attempts || []).map(a => `
        <div class="card"><b>attempt ${a.attempt_no}</b> <span class="mute">[${esc(a.step)}]</span>
          <span class="state ${esc(a.state)}">${esc(a.state)}</span>
          <span class="mute">· ${a.num_turns} turns · ${a.tool_calls} tools · ${secs(a.agent_ms)} · ${usd(a.cost_usd)} · ${a.commits} commit(s)${a.dirty ? ' · DIRTY' : ''}</span>
          ${a.reason ? `<div>${esc(a.reason)}</div>` : ''}
          ${a.outputs && a.outputs.summary ? `<div class="mute">${esc(a.outputs.summary)}</div>` : ''}
          ${(a.verdict || []).filter(c => !c.ok).map(c => `<div class="failed">✗ ${esc(c.level)} ${esc(c.name)}${c.tail ? ': ' + esc(c.tail).slice(0, 300) : ''}</div>`).join('')}
        </div>`).join('');
      const lineage = (t.lineage || []).map(l => l.id === t.id ? `<b>${l.id} ${esc(l.state)}</b>` : `<a href="/tasks/${l.id}">${l.id}</a> ${esc(l.state)}`).join(' → ');
      const refs = (t.refs || []).map(r => `<a href="${esc(r.url)}" target="_blank" rel="noopener">${esc(r.kind)}${r.label ? ': ' + esc(r.label) : ''}</a>`).join(' · ');
      const deploys = (d.deploys || []).map(dep => {
        const status = dep.check_ok === true ? 'ok' : dep.check_ok === false ? (dep.rolled_back_to ? `rolled back to ${esc(dep.rolled_back_to.slice(0, 8))}` : 'failed') : 'running';
        return `<div>${esc(dep.target)} ${esc((dep.sha || '').slice(0, 8))} ${status}</div>`;
      }).join('');
      const assessment = d.assessment ? `
        <div><span class="k">score</span>${d.assessment.score}/10 (${(d.assessment.findings || []).length} finding(s))</div>
        ${(d.assessment.findings || []).map(fnd => `<div class="mute">${esc(fnd.severity)} ${esc(fnd.path)}: ${esc(fnd.finding)}</div>`).join('')}` : '';
      const diag = (d.diagnosis || []).map(x => `<div class="card"><span class="k">what</span>${esc(x.what)}<br><span class="k">action</span>${esc(x.action)}</div>`).join('');
      const retry = (t.state === 'failed' || t.state === 'blocked') ? `<button id="retry">retry</button>` : '';
      $('#detail').innerHTML = `
        <h2>Task ${t.id} <span class="state ${esc(t.state)}">${esc(t.state)}</span> <a href="/tasks/${t.id}/run">workflow run →</a> ${retry}</h2>
        <div class="card">
          <div><span class="k">repo</span>${esc(t.repo)} <a href="/graph?repo=${encodeURIComponent(t.repo)}">graph</a></div>
          <div><span class="k">branch</span>${esc(t.branch)} <span class="mute">from ${esc(t.base_branch)} @ ${esc((t.base_sha || '').slice(0, 8))}</span></div>
          <div><span class="k">workflow</span>${esc(t.workflow)} <span class="mute">${esc((t.workflow_hash || '').slice(0, 8))}</span> · ${esc(t.model)} · ${t.max_turns} turns · ${t.max_attempts} attempts</div>
          ${(t.project || t.initiative != null) ? `<div><span class="k">project</span>${t.project ? `<a href="/projects/${encodeURIComponent(t.project)}">${esc(t.project)}</a>` : '-'}${t.initiative != null ? ` · <a href="/initiatives/${t.initiative}">initiative ${t.initiative}</a>` : ''}</div>` : ''}
          ${lineage ? `<div><span class="k">lineage</span>${lineage}</div>` : ''}
          ${refs ? `<div><span class="k">refs</span>${refs}</div>` : ''}
          ${t.reason ? `<div><span class="k">reason</span>${esc(t.reason)}</div>` : ''}
          ${assessment}
          ${deploys ? `<div><span class="k">deploys</span>${deploys}</div>` : ''}
        </div>
        <div class="card"><pre style="margin:0">${esc(t.text)}</pre></div>
        ${t.plan ? `<h2>Plan</h2><div class="card"><pre style="margin:0">${esc(t.plan)}</pre></div>` : ''}
        ${diag}
        ${attempts}
        ${t.journal ? `<h2>Journal</h2><div class="card"><pre style="margin:0">${esc(t.journal)}</pre></div>` : ''}`;
      const b = $('#retry');
      if (b) b.addEventListener('click', async () => { b.disabled = true; const r = await post(`/api/retry/${t.id}`); alert(r.output || r.error || 'retried'); go('/tasks'); });
    }
    return {
      async show() {
        $('#main').innerHTML = `<div class="two"><section><div id="detail" class="mute" style="margin:16px">loading…</div></section><section><h2>Events · task ${id}</h2><div id="feed" class="feed"></div></section></div>`;
        await snapshotHead();
        await draw();
        renderFeed();
      },
      onEvent(e) {
        if (e.task === id) { renderFeed(); if (INVALIDATES.detail.includes(e.type)) draw().catch(() => {}); }
      },
    };
  }

  // ---- plugins view
  function pluginsView() {
    let rows = [];
    function running(p) {
      if (p.state === 'running') return `running pid ${p.pid}, up ${p.uptime_secs}s`;
      if (p.state === 'restarting') return `restarting (x${p.restart_count || 0})`;
      return p.last_exit ? `stopped: ${p.last_exit}` : 'stopped';
    }
    function drawRows() {
      $('#plugin-rows').innerHTML = rows.map(p => `
        <tr data-name="${esc(p.name)}">
          <td>${esc(p.name)}</td>
          <td class="mute">${esc(p.description)}</td>
          <td class="mute">${esc((p.capabilities || []).join(', '))}</td>
          <td><span class="state ${p.enabled ? 'succeeded' : 'mute'}">${p.enabled ? 'enabled' : 'disabled'}</span></td>
          <td class="mute">${esc(running(p))}</td>
          <td>
            <button class="toggle" data-action="${p.enabled ? 'disable' : 'enable'}">${p.enabled ? 'disable' : 'enable'}</button>
            <a href="/api/plugins/${encodeURIComponent(p.name)}/logs" target="_blank" rel="noopener">logs</a>
          </td>
        </tr>`).join('');
    }
    async function refresh() {
      rows = await get('/api/plugins');
      drawRows();
    }
    return {
      async show() {
        $('#main').innerHTML = `
          <h2>Plugins</h2>
          <table><thead><tr><th>name</th><th>description</th><th>capabilities</th><th>enabled</th><th>state</th><th></th></tr></thead><tbody id="plugin-rows"></tbody></table>`;
        $('#plugin-rows').addEventListener('click', async ev => {
          const btn = ev.target.closest('button.toggle');
          if (!btn) return;
          const name = btn.closest('tr').dataset.name;
          btn.disabled = true;
          await post(`/api/plugins/${encodeURIComponent(name)}/${btn.dataset.action}`);
          await refresh();
        });
        await refresh();
      },
    };
  }

  // ---- projects view
  function projectsView() {
    function drawRows(rows) {
      $('#project-rows').innerHTML = rows.map(p => `
        <tr data-name="${esc(p.name)}">
          <td><a href="/projects/${encodeURIComponent(p.name)}">${esc(p.name)}</a></td>
          <td class="mute">${esc(p.purpose)}</td>
          <td class="num">${p.queued}</td>
          <td class="num">${p.running}</td>
          <td class="num">${p.succeeded}</td>
          <td class="num">${p.failed}</td>
          <td class="num">${usd(p.cost_usd)}</td>
        </tr>`).join('') || '<tr><td colspan="7" class="mute">no projects</td></tr>';
    }
    return {
      async show() {
        $('#main').innerHTML = `
          <h2>Projects</h2>
          <table><thead><tr><th>name</th><th>purpose</th><th class="num">queued</th><th class="num">running</th><th class="num">succeeded</th><th class="num">failed</th><th class="num">cost</th></tr></thead><tbody id="project-rows"></tbody></table>`;
        drawRows(await get('/api/projects'));
      },
    };
  }

  // ---- stats view: attempts, outcomes, cost and wall time per (role,
  // provider, model)
  function statsView() {
    const pct = v => v == null ? '-' : (v * 100).toFixed(0) + '%';
    const num = v => v == null ? '-' : v;
    function drawRows(rows) {
      $('#stats-rows').innerHTML = rows.map(r => `
        <tr>
          <td>${esc(r.role)}</td>
          <td>${esc(r.provider)}</td>
          <td>${esc(r.model)}</td>
          <td class="num">${r.attempts}</td>
          <td class="num">${pct(r.succeeded_share)}</td>
          <td class="num">${r.mean_turns.toFixed(1)}</td>
          <td class="num">${usd(r.mean_cost_usd)}</td>
          <td class="num">${r.mean_secs.toFixed(0)}</td>
          <td class="num">${num(r.landed)}</td>
          <td class="num">${num(r.broke_base)}</td>
          <td class="num">${pct(r.broke_base_share)}</td>
        </tr>`).join('') || '<tr><td colspan="11" class="mute">no data</td></tr>';
    }
    return {
      async show() {
        $('#main').innerHTML = `
          <h2>Stats by role</h2>
          <table><thead><tr><th>role</th><th>provider</th><th>model</th><th class="num">att</th><th class="num">succeed%</th><th class="num">turns</th><th class="num">cost</th><th class="num">secs</th><th class="num">landed</th><th class="num">broke</th><th class="num">broke%</th></tr></thead><tbody id="stats-rows"></tbody></table>`;
        const d = await get('/api/stats');
        drawRows(d.by_role || []);
      },
    };
  }

  // ---- one project: its initiatives and backlog
  function projectView(name) {
    async function draw() {
      const enc = encodeURIComponent(name);
      const [p, initiatives, backlog] = await Promise.all([
        get(`/api/projects/${enc}`),
        get(`/api/projects/${enc}/initiatives`),
        get(`/api/projects/${enc}/backlog`),
      ]);
      const iniRows = initiatives.map(i => `
        <tr><td><a href="/initiatives/${i.id}">${i.id}</a></td>
          <td>${esc(i.state)}${i.held_rule ? ' (' + esc(i.held_rule) + ')' : ''}</td>
          <td>${esc(i.outcome)}</td>
          <td class="num">${usd(i.cost_usd)}</td></tr>`).join('');
      const backlogRows = backlog.map(b => `
        <div class="card"><span class="mute">#${b.id} · ${b.done_at ? 'done' : 'open'}</span> ${esc(b.text)}</div>`).join('');
      const repoRows = (p.repos || []).map(r => `
        <div><span class="k">repo</span>${esc(r.repo)} <a href="/graph?repo=${encodeURIComponent(r.repo)}">graph</a></div>`).join('');
      $('#main').innerHTML = `
        <h2>Project ${esc(p.name)}</h2>
        <div class="card">
          <div>${esc(p.purpose)}</div>
          <div class="mute">workflow ${esc(p.workflow || 'direct')} · per-task ${p.per_task_usd != null ? usd(p.per_task_usd) : 'default'} · per-initiative ${p.per_initiative_usd != null ? usd(p.per_initiative_usd) : 'unlimited'}</div>
          <div class="mute">tasks queued=${p.queued} running=${p.running} succeeded=${p.succeeded} failed=${p.failed} unverified=${p.unverified} blocked=${p.blocked} withdrawn=${p.withdrawn} · ${usd(p.cost_usd)}</div>
          ${repoRows}
        </div>
        <h2>Initiatives</h2>
        <table><thead><tr><th>id</th><th>state</th><th>outcome</th><th class="num">cost</th></tr></thead><tbody>${iniRows || '<tr><td colspan="4" class="mute">no initiatives</td></tr>'}</tbody></table>
        <h2>Backlog</h2>
        ${backlogRows || '<div class="card mute">no backlog items</div>'}`;
    }
    return {
      async show() { $('#main').innerHTML = '<div class="mute" style="margin:16px">loading…</div>'; await draw(); },
    };
  }

  // ---- one initiative: the outcome, its tasks, and the generated report
  function initiativeView(id) {
    async function draw() {
      const d = await get(`/api/initiatives/${id}`);
      const taskRows = (d.tasks || []).map(t => `
        <tr><td><a href="/tasks/${t.id}">${t.id}</a></td>
          <td class="state ${esc(t.state)}">${esc(t.state)}${t.retries ? ` <span class="mute">(${t.retries} ${t.retries === 1 ? 'retry' : 'retries'})</span>` : ''}</td>
          <td>${esc(t.reason)}</td></tr>`).join('');
      const refused = (d.refused || []).map(r => `<div>${esc(r.rule)}: ${r.count}</div>`).join('');
      const rulings = (d.rulings || []).map(r => `
        <div class="card"><b>task ${r.task_id}</b> ${esc(r.question)}<div class="mute">${esc(r.answer)}</div></div>`).join('');
      const questions = (d.questions || []).map(q => `
        <div class="card"><b>task ${q.task_id}</b> ${esc(q.question)}<div class="mute">${q.answer ? esc(q.answer) : 'unanswered'}</div></div>`).join('');
      $('#main').innerHTML = `
        <h2>Initiative ${d.id} <span class="mute">· <a href="/projects/${encodeURIComponent(d.project)}">${esc(d.project)}</a></span></h2>
        <div class="card">
          <div><b>${esc(d.outcome)}</b></div>
          <div class="mute">state ${esc(d.state)}${d.held_rule ? ' (' + esc(d.held_rule) + ')' : ''} · ${usd(d.cost_usd)}${d.budget_usd != null ? ' of ' + usd(d.budget_usd) : ''}${d.elapsed_secs != null ? ' · ' + secs(d.elapsed_secs * 1000) + ' elapsed' : ''}</div>
        </div>
        <h2>Tasks</h2>
        <table><thead><tr><th>id</th><th>state</th><th>reason</th></tr></thead><tbody>${taskRows || '<tr><td colspan="3" class="mute">no tasks</td></tr>'}</tbody></table>
        ${refused ? `<h2>Refused</h2><div class="card">${refused}</div>` : ''}
        ${rulings ? `<h2>Rulings</h2>${rulings}` : ''}
        ${questions ? `<h2>Questions</h2>${questions}` : ''}`;
    }
    return {
      async show() { $('#main').innerHTML = '<div class="mute" style="margin:16px">loading…</div>'; await draw(); },
    };
  }

  // ---- run view: the task inside its workflow
  function runView(id) {
    const kvs = obj => Object.entries(obj).filter(([, v]) => v !== null && v !== undefined && v !== '' && !(Array.isArray(v) && !v.length) && v !== false)
      .map(([k, v]) => {
        const s = typeof v === 'string' ? v : JSON.stringify(v);
        const long = s.length > 160 || s.includes('\n');
        return `<div class="k">${esc(k)}</div><div>${long ? `<details><summary>${esc(s.slice(0, 80))}…</summary><pre>${esc(s)}</pre></details>` : esc(s)}</div>`;
      }).join('');
    async function draw() {
      const d = await get(`/api/task/${id}`);
      const t = d.task || {};
      const steps = (d.resolved && d.resolved.steps) || [];
      const ops = d.ops || [], attempts = d.attempts || [];
      // The kernel numbers the clone 0, the workflow's steps 1..n, and the
      // landing (integrate, push, land) after them.
      const n = steps.length;
      const pick = (o, a) => ({ ops: ops.filter(o), attempts: attempts.filter(a) });
      const groups = [{ title: 'clone', st: null, ...pick(o => o.seq === 0, a => a.step_seq === 0) }];
      for (let i = 1; i <= n; i++) groups.push({ title: null, st: steps[i - 1], ...pick(o => o.seq === i, a => a.step_seq === i) });
      groups.push({ title: 'landing', st: null, ...pick(o => o.seq > n, a => a.step_seq > n) });
      const cols = [];
      for (const g of groups) {
        const { st, ops: o, attempts: at } = g;
        if (!st && !o.length && !at.length) continue;
        const act = st ? st.action : null;
        const head = act ? `<h3>${esc(act.name)} <small>${esc(act.kind)}${act.contract && act.contract !== act.name ? ' · ' + esc(act.contract) : ''} · ${esc((act.hash || '').slice(0, 8))}${st.model ? ' · ' + esc(st.model) : ''}${st.max_turns ? ' · ' + st.max_turns + ' turns' : ''}</small></h3><div class="mute" style="font-size:12px">${esc(act.description || '')}</div>`
                            : `<h3>${esc(g.title)} <small>kernel</small></h3>`;
        const opsHtml = o.length ? `<div class="ops">${o.map(x => `<div class="${x.ok ? 'succeeded' : 'failed'}">${x.ok ? '✓' : '✗'} ${esc(x.name)}${x.kernel ? '' : ' [user]'} <span class="mute">${secs(x.ms)}${x.detail ? ' · ' + esc(x.detail).slice(0, 120) : ''}</span>${x.output ? `<details><summary>output</summary><pre>${esc(x.output)}</pre></details>` : ''}</div>`).join('')}</div>` : '';
        const atHtml = at.map(a => {
          const inputs = { model: a.inputs.model, max_turns: a.inputs.max_turns, timeout_secs: a.inputs.timeout_secs, base: (a.inputs.base_sha || '').slice(0, 8), start: (a.inputs.start_sha || '').slice(0, 8), resumed: a.inputs.resumed, prompt_chars: a.inputs.prompt_chars, feedback: a.inputs.feedback, interface: a.inputs.interface, plan: a.inputs.plan, context: a.inputs.context, journal: a.inputs.journal, overlay_refs: a.inputs.overlay_refs, protected: a.inputs.protected, namespace: a.inputs.namespace, task_checks: a.inputs.task_checks, checks_shown: a.inputs.checks_shown };
          const outputs = { summary: a.outputs.summary, changed_files: a.outputs.changed_files, dirty_files: a.outputs.dirty_files, end: (a.outputs.end_sha || '').slice(0, 8), checks_run: a.outputs.checks_run, claims: a.outputs.claims, first_edit_call: a.outputs.first_edit_call, interface: a.outputs.interface, verify_ref: a.outputs.verify_ref, tools: a.outputs.tools && a.outputs.tools.by_tool ? Object.entries(a.outputs.tools.by_tool).map(([k, v]) => `${k} ${v.calls ?? v}`).join(', ') : a.outputs.tools, tokens: a.tokens ? `in ${a.tokens.input} · out ${a.tokens.output} · cache read ${a.tokens.cache_read} · cache write ${a.tokens.cache_creation}` : null };
          const rows = (a.verdict || []).map(c => `<div class="${c.ok ? 'succeeded' : 'failed'}">${c.ok ? '✓' : '✗'} ${esc(c.level)} ${esc(c.name)} <span class="mute">${c.ms ? secs(c.ms) : ''}</span>${!c.ok && c.tail ? `<details><summary>detail</summary><pre>${esc(c.tail)}</pre></details>` : ''}</div>`).join('');
          return `<div class="attempt"><h4>attempt ${a.attempt_no} <span class="state ${esc(a.state)}">${esc(a.state)}</span> <span class="mute">${a.num_turns} turns · ${a.tool_calls} tools · ${secs(a.agent_ms)} · ${usd(a.cost_usd)} · ${a.commits} commit(s)${a.dirty ? ' · DIRTY' : ''}</span></h4>
            ${a.reason ? `<div class="mute">${esc(a.reason)}</div>` : ''}
            <details open><summary>inputs</summary><div class="kv">${kvs(inputs)}</div></details>
            <details open><summary>outputs</summary><div class="kv">${kvs(outputs)}</div></details>
            <details open><summary>verdict</summary><div class="rows">${rows || '<span class="mute">none</span>'}</div></details>
            <div class="mute" style="font-size:12px">log ${esc(a.log_path)}</div>
          </div>`;
        }).join('');
        cols.push(`<div class="step">${head}${opsHtml}${atHtml}</div>`);
      }
      const diag = (d.diagnosis || []).map(x => `<div class="card"><span class="k">what</span>${esc(x.what)}<br><span class="k">action</span>${esc(x.action)}</div>`).join('');
      $('#main').innerHTML = `
        <h2>Task ${t.id} <span class="state ${esc(t.state)}">${esc(t.state)}</span> · workflow ${esc(t.workflow)} <span class="mute">${esc((t.workflow_hash || '').slice(0, 8))}</span> · <a href="/tasks/${t.id}">task view</a></h2>
        <div class="card"><pre style="margin:0">${esc(t.text)}</pre></div>
        <div class="pipeline">${cols.join('<div class="arrow">→</div>')}</div>
        ${diag ? `<h2>Diagnosis</h2>${diag}` : ''}
        ${t.workflow_text ? `<h2>Workflow file</h2><div class="card"><pre style="margin:0">${esc(t.workflow_text)}</pre></div>` : ''}`;
    }
    return {
      async show() { $('#main').innerHTML = '<div class="mute" style="margin:16px">loading…</div>'; await snapshotHead(); await draw(); },
      onEvent(e) { if (e.task === id && INVALIDATES.run.includes(e.type)) draw().catch(() => {}); },
    };
  }

  // ---- graph view: the structure layer. Files grouped into columns by
  // top-level directory, ordered within a column by path; edges from
  // `forge-repomap edges` as lines between columns. The only overlay is
  // data the client already has: for each of the repo's last 20 tasks
  // (the task list), which files its attempts reported changed.
  function graphView(repo) {
    let graph = null, touches = new Map(), filter = '', selected = null;

    function neighboursOf(path) {
      const set = new Set([path]);
      for (const e of graph.edges) {
        if (e.from === path) set.add(e.to);
        if (e.to === path) set.add(e.from);
      }
      return set;
    }
    function columns() {
      const byDir = new Map();
      for (const n of graph.nodes) {
        const i = n.path.indexOf('/');
        const dir = i === -1 ? '(root)' : n.path.slice(0, i);
        if (!byDir.has(dir)) byDir.set(dir, []);
        byDir.get(dir).push(n);
      }
      return [...byDir.keys()].sort().map(dir => ({ dir, nodes: byDir.get(dir).sort((a, b) => a.path.localeCompare(b.path)) }));
    }
    function draw() {
      const svg = $('#graph-svg');
      if (!svg || !graph) return;
      const colW = 240, rowH = 20, top = 22, left = 10, nodeW = 210, nodeH = 15;
      const cols = columns();
      const pos = new Map();
      cols.forEach((c, ci) => c.nodes.forEach((n, ri) => pos.set(n.path, { x: left + ci * colW, y: top + ri * rowH })));
      const rows = Math.max(1, ...cols.map(c => c.nodes.length));
      const width = left + cols.length * colW + 20;
      const height = top + rows * rowH + 20;
      const q = filter.trim().toLowerCase();
      const shown = new Set(graph.nodes.filter(n => !q || n.path.toLowerCase().includes(q)).map(n => n.path));
      const active = selected ? neighboursOf(selected) : null;
      const visible = path => shown.has(path) && (!active || active.has(path));
      const headers = cols.map((c, ci) => `<text x="${left + ci * colW}" y="14" font-size="11" fill="var(--mute)">${esc(c.dir)}</text>`).join('');
      const edgesSvg = graph.edges
        .filter(e => pos.has(e.from) && pos.has(e.to) && visible(e.from) && visible(e.to))
        .map(e => {
          const a = pos.get(e.from), b = pos.get(e.to);
          return `<line x1="${a.x + nodeW}" y1="${a.y + nodeH / 2}" x2="${b.x}" y2="${b.y + nodeH / 2}" stroke="var(--run)" stroke-width="1" opacity="0.35" />`;
        }).join('');
      const nodesSvg = graph.nodes.filter(n => visible(n.path)).map(n => {
        const p = pos.get(n.path);
        const t = touches.get(n.path) || [];
        const title = `${n.path} — ${n.symbols} symbol(s)${t.length ? ` — touched by tasks ${t.join(', ')}` : ''}`;
        return `<g class="gnode" data-path="${esc(n.path)}" transform="translate(${p.x},${p.y})">
          <rect width="${nodeW}" height="${nodeH}" rx="3" fill="${n.path === selected ? 'var(--sel)' : 'var(--panel)'}" stroke="var(--line)"></rect>
          <text x="4" y="${nodeH - 4}" font-size="10" fill="var(--fg)">${esc(n.path.split('/').pop())}</text>
          <title>${esc(title)}</title>
        </g>`;
      }).join('');
      svg.setAttribute('width', width);
      svg.setAttribute('height', height);
      svg.innerHTML = headers + edgesSvg + nodesSvg;
    }
    async function loadTouches() {
      let tasks = [];
      try { tasks = await get('/api/tasks?' + new URLSearchParams({ repo, limit: 20 })); } catch { tasks = []; }
      const map = new Map();
      await Promise.all(tasks.map(async t => {
        let d;
        try { d = await get(`/api/task/${t.id}`); } catch { return; }
        const files = new Set();
        for (const a of d.attempts || []) for (const f of (a.outputs && a.outputs.changed_files) || []) files.add(f);
        for (const f of files) { if (!map.has(f)) map.set(f, []); map.get(f).push(t.id); }
      }));
      return map;
    }
    return {
      async show() {
        $('#main').innerHTML = `
          <h2>Graph</h2>
          <div class="filters">
            <span class="mute">${esc(repo) || 'no repo given'}</span>
            <input type="search" id="f-graph" placeholder="filter files" ${repo ? '' : 'disabled'}>
          </div>
          <div style="overflow:auto; padding:0 16px 24px"><svg id="graph-svg"></svg></div>`;
        if (!repo) return;
        $('#f-graph').addEventListener('input', ev => { filter = ev.target.value; draw(); });
        $('#graph-svg').addEventListener('click', ev => {
          const g = ev.target.closest('.gnode');
          const path = g ? g.dataset.path : null;
          selected = (path && path !== selected) ? path : null;
          draw();
        });
        try { graph = await get(`/api/graph?repo=${encodeURIComponent(repo)}`); } catch { graph = null; }
        if (!graph || !Array.isArray(graph.nodes)) graph = { nodes: [], edges: [] };
        draw();
        touches = await loadTouches();
        draw();
      },
    };
  }

  $('#refresh').addEventListener('click', () => render());
  setInterval(() => snapshotHead().catch(() => {}), 30000);
  render().catch(console.error);
})();

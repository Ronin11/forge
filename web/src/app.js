(() => {
  const $ = s => document.querySelector(s);
  const esc = s => String(s ?? '').replace(/[&<>"]/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;'}[c]));
  const usd = n => '$' + (Number(n) || 0).toFixed(2);
  const secs = ms => ((ms || 0) / 1000).toFixed(0) + 's';
  const { fmtTime, fmtSpan, fmtAgo } = ForgeTime;
  const { draftStatusFor } = ForgeWorkflows;
  const PAGE = 100;
  let offset = 0, feed = [], es = null, view = null;

  // Which events invalidate which view — matches docs/CLIENT.md's "What
  // to re-read on which event".
  const INVALIDATES = {
    list: ['task_queued', 'task_started', 'task_done', 'attempt_done', 'pushed'],
    detail: ['task_done', 'attempt_done', 'deploy_finished'],
    run: ['task_done', 'attempt_done', 'op'],
    jobs: ['job_started', 'job_finished'],
    // A workflow's measured profile moves when a task lands under it
    // (build workflows) or a job using it finishes (run workflows).
    workflows: ['task_done', 'job_finished'],
    // A task going blocked or reaching any other final state (including
    // unverified) is a `task_done` event whose `state` names which
    // (src/engine.rs's `finish`, docs/CLIENT.md's Events table has no
    // separate "blocked" variant); `task_blocked` is listened for too in
    // case a future kernel event narrows this, at no cost today.
    requests: ['task_blocked', 'task_done'],
    deploys: ['deploy_started', 'deploy_finished'],
    // A message's own record never changes after the fact, but the
    // questions/decisions/jobs sections beside it do: a blocked task
    // going open or resolved, and a message-triggered job starting or
    // finishing.
    messages: ['task_blocked', 'task_done', 'job_started', 'job_finished'],
    // The activity page (web/src/activity.js's own view below) has no
    // entry here: it redraws on every event unconditionally, since it is
    // the raw stream itself and its running-attempts panel needs
    // `tool_call`/`agent_done` too, neither of which any other page ever
    // treats as list-dirty.
  };

  async function call(method, path) {
    const r = await fetch(path, { method, credentials: 'same-origin' });
    if (r.status === 401) { document.body.innerHTML = '<p style="margin:2em">Not signed in: open the link forge-web printed when it started.</p>'; throw new Error('401'); }
    return r.json();
  }
  const get = path => call('GET', path);
  const post = path => call('POST', path);
  async function postBody(path, body, contentType) {
    const r = await fetch(path, { method: 'POST', credentials: 'same-origin', headers: contentType ? { 'Content-Type': contentType } : {}, body });
    if (r.status === 401) { document.body.innerHTML = '<p style="margin:2em">Not signed in: open the link forge-web printed when it started.</p>'; throw new Error('401'); }
    return r.json();
  }

  // ---- routing: /tasks, /tasks/:id, /tasks/:id/run, /requests, /jobs,
  // /jobs/:id, /plugins, /projects, /projects/:name, /initiatives,
  // /initiatives/:id, /deploys, /activity, /messages, /doctor, /graph,
  // /graph/modules, /stats, /workflows, /workflows/:name — every page
  // the nav names (web/src/shell.js's NAV_PAGES); a page with no view of
  // its own yet still routes, to `stubView` below.
  function route() {
    if (location.pathname === '/plugins') return { page: 'plugins' };
    if (location.pathname === '/requests') return { page: 'requests' };
    if (location.pathname === '/projects') return { page: 'projects' };
    if (location.pathname === '/initiatives') return { page: 'initiatives-list' };
    if (location.pathname === '/deploys') return { page: 'deploys' };
    if (location.pathname === '/activity') return { page: 'activity' };
    if (location.pathname === '/messages') return { page: 'messages' };
    if (location.pathname === '/doctor') return { page: 'doctor' };
    if (location.pathname === '/stats') return { page: 'stats' };
    let m = location.pathname.match(/^\/graph(\/modules)?\/?$/);
    if (m) return { page: 'graph', modules: !!m[1], repo: new URLSearchParams(location.search).get('repo') || '' };
    m = location.pathname.match(/^\/projects\/([^/]+)\/?$/);
    if (m) return { page: 'project', name: decodeURIComponent(m[1]) };
    m = location.pathname.match(/^\/initiatives\/(\d+)\/?$/);
    if (m) return { page: 'initiative', id: Number(m[1]) };
    m = location.pathname.match(/^\/jobs(?:\/(\d+))?\/?$/);
    if (m) return { page: 'jobs', id: m[1] ? Number(m[1]) : null };
    m = location.pathname.match(/^\/workflows(?:\/([^/]+))?\/?$/);
    if (m) {
      const name = m[1] ? decodeURIComponent(m[1]) : null;
      if (name === 'new') return { page: 'workflow-new' };
      return { page: 'workflows', name, project: new URLSearchParams(location.search).get('project') || null };
    }
    m = location.pathname.match(/^\/tasks(?:\/(\d+)(\/run)?)?\/?$/);
    if (!m) { history.replaceState(null, '', '/tasks'); return route(); }
    if (m[1]) return { page: 'tasks', id: Number(m[1]), run: !!m[2] };
    const { filters, before } = ForgeSearch.filtersFromSearch(location.search);
    return { page: 'tasks', id: null, run: false, filters, before };
  }
  function go(path) { history.pushState(null, '', path); render(); }
  // Every href the nav (or a page's own content) can carry: each of the
  // shell's NAV_PAGES as a prefix, so a sub-route (`/tasks/1`) and a link
  // carrying a query string (`/graph?repo=...`) both route client-side.
  const NAV_SELECTOR = ForgeShell.NAV_PAGES.map(p => `a[href^="${p.href}"]`).join(', ');
  document.addEventListener('click', ev => {
    const a = ev.target.closest(NAV_SELECTOR);
    if (a && !ev.metaKey && !ev.ctrlKey) { ev.preventDefault(); go(a.getAttribute('href')); }
  });
  window.addEventListener('popstate', render);

  // ---- keyboard shortcuts (shared by every page): '/' focuses search,
  // 'g' then a letter jumps to a page (ForgeShell.SHORTCUT_TARGETS).
  function focusSearch() {
    const el = $('#f-q');
    if (el) { el.focus(); el.select(); return; }
    go('/tasks');
  }
  let gPending = false, gTimer = null;
  document.addEventListener('keydown', ev => {
    const tag = (ev.target && ev.target.tagName) || '';
    const typing = tag === 'INPUT' || tag === 'TEXTAREA' || (ev.target && ev.target.isContentEditable);
    if (ev.key === '/' && !typing) { ev.preventDefault(); focusSearch(); return; }
    if (typing || ev.metaKey || ev.ctrlKey || ev.altKey) return;
    if (gPending) {
      gPending = false; clearTimeout(gTimer);
      const href = ForgeShell.SHORTCUT_TARGETS[ev.key];
      if (href) { ev.preventDefault(); go(href); }
      return;
    }
    if (ev.key === 'g') { gPending = true; gTimer = setTimeout(() => { gPending = false; }, 1500); }
  });

  function nav(r) {
    const taskLinks = r.page === 'tasks' && r.id !== null
      ? ` <a href="/tasks/${r.id}" ${!r.run ? 'style="font-weight:600"' : ''}>task ${r.id}</a> <a href="/tasks/${r.id}/run" ${r.run ? 'style="font-weight:600"' : ''}>workflow run</a>`
      : '';
    const graphQuery = r.page === 'graph' && r.repo ? `?repo=${encodeURIComponent(r.repo)}` : '';
    const graphLinks = r.page === 'graph'
      ? ` <a href="/graph${graphQuery}" ${!r.modules ? 'style="font-weight:600"' : ''}>files</a> <a href="/graph/modules${graphQuery}" ${r.modules ? 'style="font-weight:600"' : ''}>modules</a>`
      : '';
    const activeKey = r.page === 'project' ? 'projects'
      : (r.page === 'initiative' || r.page === 'initiatives-list') ? 'initiatives'
      : r.page === 'workflow-new' ? 'workflows'
      : r.page;
    $('#nav').innerHTML = ForgeShell.renderNav(activeKey, taskLinks + graphLinks);
  }

  async function render() {
    const r = route();
    nav(r);
    if (view && view.teardown) view.teardown();
    view = r.page === 'plugins' ? pluginsView()
      : r.page === 'requests' ? requestsView()
      : r.page === 'projects' ? projectsView()
      : r.page === 'project' ? projectView(r.name)
      : r.page === 'initiative' ? initiativeView(r.id)
      : r.page === 'initiatives-list' ? stubView('Initiatives')
      : r.page === 'deploys' ? deploysView()
      : r.page === 'activity' ? activityView()
      : r.page === 'messages' ? messagesView()
      : r.page === 'doctor' ? doctorView()
      : r.page === 'graph' ? (r.modules ? graphModulesView(r.repo) : graphView(r.repo))
      : r.page === 'stats' ? statsView()
      : r.page === 'jobs' ? (r.id === null ? jobsView() : jobView(r.id))
      : r.page === 'workflow-new' ? promptView()
      : r.page === 'workflows' ? (r.name === null ? workflowsView() : workflowView(r.name, r.project))
      : (r.id === null ? listView(r.filters, r.before) : (r.run ? runView(r.id) : detailView(r.id)));
    await view.show();
  }

  // ---- shared: the header strip (worker, rate gauges, queue, spend, a
  // last-updated stamp) and the live stream that drives it
  let headData = { worker: {}, tasks: [], doctor: [] };
  let liveStatus = { text: 'connecting', cls: 'mute' };
  function applyLiveStatus() {
    const el = $('#live');
    if (el) { el.textContent = liveStatus.text; el.className = liveStatus.cls; }
  }
  function drawHeadStrip(now) {
    $('#head-strip').innerHTML = ForgeShell.renderHeaderStrip(
      { worker: headData.worker, tasks: headData.tasks, doctor: headData.doctor, now }, fmtTime);
    applyLiveStatus();
  }
  async function refreshDoctor() {
    // Not part of the stable contract (docs/CLIENT.md), so a read that
    // fails (no home, no store yet) just leaves the gauges at '—'
    // instead of breaking the rest of the header.
    try { headData.doctor = await get('/api/doctor'); } catch { headData.doctor = []; }
  }
  async function snapshotHead() {
    const s = await get('/api/snapshot');
    headData.worker = s.worker || {};
    headData.tasks = s.tasks || [];
    await refreshDoctor();
    drawHeadStrip(Date.now() / 1000);
    if (!es) { offset = s.events_offset || 0; subscribe(); }
    return s;
  }
  function subscribe() {
    es = new EventSource(`/api/events?since=${offset}`);
    es.onopen = () => { liveStatus = { text: 'live', cls: 'succeeded' }; applyLiveStatus(); };
    es.onerror = () => { liveStatus = { text: 'reconnecting', cls: 'failed' }; applyLiveStatus(); };
    es.onmessage = m => {
      let e; try { e = JSON.parse(m.data); } catch { return; }
      feed.push(e); if (feed.length > 5000) feed.splice(0, feed.length - 5000);
      // The last-updated stamp tracks the event stream itself, not just
      // the header's own 30s poll (docs: "a last-updated stamp driven by
      // the event stream").
      const upd = $('#updated');
      if (upd && e.ts != null) upd.textContent = `updated ${fmtTime(e.ts)}`;
      if (view && view.onEvent) view.onEvent(e);
    };
  }

  // ---- a page named by the nav with no view of its own yet: a later
  // task of the Web UI initiative builds it.
  function stubView(title) {
    return {
      async show() {
        $('#main').innerHTML = `<h2>${esc(title)}</h2><div class="stub">Not built yet.</div>`;
      },
    };
  }

  // ---- requests view: the inbox. Everything waiting on a person: open
  // questions, dependency blocks, and workflow requests (forge requests
  // --json — RequestRow already carries the dependency's state as part
  // of its own text, e.g. "waits on task 14 (failed: ...)"), plus every
  // unverified task (forge log --json --state unverified) since the only
  // thing left waiting on those is the land decision. Its own page on
  // the client contract (task 530, "the inbox").
  function requestsView() {
    // A question row's lineage and its last attempt's summary aren't on
    // RequestRow itself (docs/CLIENT.md) — fetched per question from
    // `/api/task/<id>` (`forge trace ID --json`) and attached before
    // rendering; a row this fails to load for just renders without them.
    async function enrichQuestions(rows) {
      await Promise.all(rows.filter(r => r.kind === 'question').map(async r => {
        let d;
        try { d = await get(`/api/task/${r.id}`); } catch { return; }
        r.lineage = (d.task && d.task.lineage) || [];
        const attempts = d.attempts || [];
        const last = attempts[attempts.length - 1];
        r.last_summary = last ? ((last.outputs && last.outputs.summary) || last.reason || '') : '';
      }));
      return rows;
    }
    async function draw() {
      const [reqs, unverified] = await Promise.all([
        get('/api/requests').then(enrichQuestions),
        get('/api/tasks?state=unverified').catch(() => []),
      ]);
      $('#req-rows').innerHTML = ForgeRequests.renderRequestRows(reqs);
      const heading = $('#req-unverified-h2'), rows = $('#req-unverified');
      if (heading) heading.style.display = unverified.length ? '' : 'none';
      if (rows) rows.innerHTML = ForgeRequests.renderUnverifiedRows(unverified);
    }
    async function onSubmit(ev) {
      const answerForm = ev.target.closest('form.req-answer');
      const withdrawForm = ev.target.closest('form.req-withdraw');
      if (answerForm) {
        ev.preventDefault();
        const value = answerForm.querySelector('.req-answer-text').value.trim();
        if (!value) return;
        const btn = answerForm.querySelector('button');
        btn.disabled = true;
        try {
          const r = await postBody(`/api/answer/${answerForm.dataset.id}`, JSON.stringify({ text: value }), 'application/json');
          if (r.error) alert(r.error); else await draw();
        } finally { btn.disabled = false; }
      } else if (withdrawForm) {
        ev.preventDefault();
        const value = withdrawForm.querySelector('.req-withdraw-reason').value.trim();
        if (!value) return;
        const btn = withdrawForm.querySelector('button');
        btn.disabled = true;
        try {
          const r = await postBody(`/api/withdraw/${withdrawForm.dataset.id}`, JSON.stringify({ reason: value }), 'application/json');
          if (r.error) alert(r.error); else await draw();
        } finally { btn.disabled = false; }
      }
    }
    async function onClick(ev) {
      const btn = ev.target.closest('button.req-land');
      if (!btn) return;
      btn.disabled = true;
      try {
        const r = await post(`/api/land/${btn.dataset.id}`);
        if (r.error) alert(r.error); else await draw();
      } finally { btn.disabled = false; }
    }
    return {
      async show() {
        // A wrapper div, not `#main` itself, carries the delegated
        // listeners: `#main`'s own innerHTML is reassigned on every visit
        // to this page, but the element itself persists across
        // navigations, so a listener attached directly to it would
        // accumulate one instance per visit. This inner div is a fresh
        // node each time, same as `#tasks`/`#plugin-rows` elsewhere.
        $('#main').innerHTML = `
          <div id="req-page">
            <h2>Requests</h2>
            <div id="req-rows" class="mute">loading…</div>
            <h2 id="req-unverified-h2" style="display:none">Unverified — needs landing</h2>
            <div id="req-unverified"></div>
          </div>`;
        $('#req-page').addEventListener('submit', onSubmit);
        $('#req-page').addEventListener('click', onClick);
        await draw();
      },
      onEvent(e) { if (INVALIDATES.requests.includes(e.type)) draw().catch(() => {}); },
    };
  }

  // ---- list view: a query box and filters mapped one to one onto `forge
  // log --json`'s own (web UI task 9, "search") — text, state, repository,
  // workflow, project, initiative — carried in the URL (`ForgeSearch`, see
  // web/src/search.js) so a filtered view can be linked or reloaded, paged
  // backward with the `before` cursor the same way `forge log --before`
  // itself pages, and its columns sortable (client-side, over whatever
  // page is loaded — sorting doesn't refetch).
  const SORT_COLUMNS = [
    { key: 'id', label: 'id', numeric: true, value: t => t.id },
    { key: 'state', label: 'state', value: t => t.state },
    { key: 'workflow', label: 'wf', value: t => t.workflow },
    { key: 'attempts', label: 'att', numeric: true, value: t => t.attempts },
    { key: 'cost_usd', label: 'cost', numeric: true, value: t => t.cost_usd },
    { key: 'initiative', label: 'init', numeric: true, value: t => t.initiative },
    { key: 'created_at', label: 'created', numeric: true, value: t => t.created_at },
    { key: 'task', label: 'task', value: t => t.task },
  ];
  function sortRows(list, s) {
    const col = SORT_COLUMNS.find(c => c.key === s.key);
    if (!col) return list;
    const sign = s.dir === 'asc' ? 1 : -1;
    return [...list].sort((a, b) => {
      const av = col.value(a), bv = col.value(b);
      if (av == null && bv == null) return 0;
      if (av == null) return 1;
      if (bv == null) return -1;
      if (av < bv) return -sign;
      if (av > bv) return sign;
      return 0;
    });
  }
  function listView(initialFilters, initialBefore) {
    let rows = [], done = false, loading = false, sort = null;
    let filters = { ...ForgeSearch.emptyFilters(), ...(initialFilters || {}) };
    let pendingBefore = initialBefore || null;
    let observer = null, debounce = null;
    const qs = before => {
      const p = ForgeSearch.paramsFromFilters(filters, before);
      p.set('limit', PAGE);
      return '/api/tasks?' + p;
    };
    function syncUrl(before) {
      const p = ForgeSearch.paramsFromFilters(filters, before);
      const q = p.toString();
      const path = '/tasks' + (q ? `?${q}` : '');
      if (path !== location.pathname + location.search) history.replaceState(null, '', path);
    }
    async function page(reset) {
      if (loading) return; loading = true;
      try {
        const before = reset ? pendingBefore : (rows.length ? Math.min(...rows.map(t => t.id)) : null);
        if (!reset && done) return;
        const got = await get(qs(before));
        if (reset) rows = got; else rows = rows.concat(got.filter(t => !rows.some(r => r.id === t.id)));
        done = got.length < PAGE;
        pendingBefore = before;
        syncUrl(before);
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
    function onFilterChange() {
      pendingBefore = null;
      page(true);
    }
    function theadHtml() {
      return SORT_COLUMNS.map(c => {
        const active = sort && sort.key === c.key ? sort : null;
        const arrow = active ? (active.dir === 'asc' ? ' ▲' : ' ▼') : '';
        return `<th data-key="${c.key}" class="sortable${c.numeric ? ' num' : ''}">${esc(c.label)}${arrow}</th>`;
      }).join('');
    }
    function drawRows() {
      const wfs = [...new Set(rows.map(t => t.workflow))].sort();
      const sel = $('#f-workflow');
      if (sel && sel.options.length !== wfs.length + 1) {
        const cur = sel.value;
        sel.innerHTML = '<option value="">any workflow</option>' + wfs.map(w => `<option ${w === cur ? 'selected' : ''}>${esc(w)}</option>`).join('');
      }
      const sorted = sort ? sortRows(rows, sort) : rows;
      $('#tasks-table').innerHTML = `<thead><tr>${theadHtml()}</tr></thead><tbody id="tasks">${sorted.map(t => `
        <tr class="task" data-id="${t.id}">
          <td class="num">${t.id}</td>
          <td class="state ${esc(t.state)}">${esc(t.state)}</td>
          <td>${esc(t.workflow)}</td>
          <td class="num">${t.attempts}</td>
          <td class="num">${usd(t.cost_usd)}</td>
          <td class="num">${t.initiative != null ? `<a href="/initiatives/${t.initiative}">${t.initiative}</a>` : ''}</td>
          <td class="mute" style="white-space:nowrap">${fmtTime(t.created_at)}</td>
          <td class="task-text" title="${esc(t.task)}">${esc(t.task)}</td>
        </tr>`).join('')}</tbody>`;
      $('#sentinel').textContent = done ? (rows.length ? `${rows.length} task(s)` : 'no tasks match') : 'loading more…';
    }
    return {
      async show() {
        $('#main').innerHTML = `
          <h2>Tasks</h2>
          <div class="filters">
            <input type="search" id="f-q" placeholder="search text or id" value="${esc(filters.q)}">
            <select id="f-state"><option value="">any state</option>${['queued','running','succeeded','failed','blocked','unverified','withdrawn'].map(s => `<option ${s === filters.state ? 'selected' : ''}>${s}</option>`).join('')}</select>
            <select id="f-workflow"><option value="">any workflow</option></select>
            <select id="f-project"><option value="">any project</option></select>
            <input type="text" id="f-repo" placeholder="repo path" value="${esc(filters.repo)}">
            <input type="number" id="f-initiative" placeholder="initiative id" value="${esc(filters.initiative)}">
          </div>
          <table id="tasks-table"><thead><tr>${theadHtml()}</tr></thead><tbody id="tasks"></tbody></table>
          <div id="sentinel" class="sentinel">loading…</div>`;
        $('#f-q').addEventListener('input', ev => { clearTimeout(debounce); debounce = setTimeout(() => { filters.q = ev.target.value.trim(); onFilterChange(); }, 250); });
        $('#f-state').addEventListener('change', ev => { filters.state = ev.target.value; onFilterChange(); });
        $('#f-workflow').addEventListener('change', ev => { filters.workflow = ev.target.value; onFilterChange(); });
        $('#f-project').addEventListener('change', ev => { filters.project = ev.target.value; onFilterChange(); });
        $('#f-repo').addEventListener('input', ev => { clearTimeout(debounce); debounce = setTimeout(() => { filters.repo = ev.target.value.trim(); onFilterChange(); }, 250); });
        $('#f-initiative').addEventListener('input', ev => { clearTimeout(debounce); debounce = setTimeout(() => { filters.initiative = ev.target.value.trim(); onFilterChange(); }, 250); });
        if (filters.workflow) $('#f-workflow').innerHTML = `<option value="">any workflow</option><option selected>${esc(filters.workflow)}</option>`;
        get('/api/projects').then(rows => {
          $('#f-project').innerHTML = '<option value="">any project</option>' + rows.map(p => `<option value="${esc(p.name)}" ${p.name === filters.project ? 'selected' : ''}>${esc(p.name)}</option>`).join('');
        }).catch(() => {});
        $('#tasks-table').addEventListener('click', ev => {
          const th = ev.target.closest('th[data-key]');
          if (th) {
            const key = th.dataset.key;
            sort = sort && sort.key === key && sort.dir === 'asc' ? { key, dir: 'desc' } : { key, dir: 'asc' };
            drawRows();
            return;
          }
          if (ev.target.closest('a')) return;
          const tr = ev.target.closest('tr.task');
          if (tr) go(`/tasks/${tr.dataset.id}`);
        });
        observer = new IntersectionObserver(entries => { if (entries.some(e => e.isIntersecting)) page(false); }, { rootMargin: '400px' });
        observer.observe($('#sentinel'));
        await snapshotHead();
        await page(true);
      },
      onEvent(e) {
        if (INVALIDATES.list.includes(e.type)) {
          refreshHead().catch(() => {});
          snapshotHead().catch(() => {});
        }
      },
      teardown() { if (observer) observer.disconnect(); clearTimeout(debounce); },
    };
  }

  // ---- detail view: the full task page (task 531, "the task page in
  // full") — everything `forge trace --json` carries, rendered by
  // `web/src/task.js`, plus the operator actions from the inbox (task
  // 530) where each applies to this task's own state.
  function detailView(id) {
    let compare = null; // only ever known live, from this task's own `task_done` event
    function renderFeed() {
      const rowsEl = $('#feed'); if (!rowsEl) return;
      const rows = feed.filter(e => e.task === id).slice(-300);
      rowsEl.innerHTML = rows.map(e => `<div><span class="t">${fmtTime(e.ts)}</span>${esc(e.text || e.type)}</div>`).join('');
      rowsEl.lastElementChild?.scrollIntoView({ block: 'nearest' });
    }
    async function draw() {
      const d = await get(`/api/task/${id}`);
      $('#detail').innerHTML = `<div style="margin:0 16px 6px"><a href="/tasks/${id}/run">workflow run →</a></div>` +
        ForgeTask.renderTaskDetail(d, fmtTime, { compare });
    }
    async function onSubmit(ev) {
      const answerForm = ev.target.closest('form.req-answer');
      const withdrawForm = ev.target.closest('form.req-withdraw');
      if (answerForm) {
        ev.preventDefault();
        const value = answerForm.querySelector('.req-answer-text').value.trim();
        if (!value) return;
        const btn = answerForm.querySelector('button');
        btn.disabled = true;
        try {
          const r = await postBody(`/api/answer/${answerForm.dataset.id}`, JSON.stringify({ text: value }), 'application/json');
          if (r.error) alert(r.error); else await draw();
        } finally { btn.disabled = false; }
      } else if (withdrawForm) {
        ev.preventDefault();
        const value = withdrawForm.querySelector('.req-withdraw-reason').value.trim();
        if (!value) return;
        const btn = withdrawForm.querySelector('button');
        btn.disabled = true;
        try {
          const r = await postBody(`/api/withdraw/${withdrawForm.dataset.id}`, JSON.stringify({ reason: value }), 'application/json');
          if (r.error) alert(r.error); else await draw();
        } finally { btn.disabled = false; }
      }
    }
    async function onClick(ev) {
      const land = ev.target.closest('button.req-land');
      const retry = ev.target.closest('button.task-retry');
      if (land) {
        land.disabled = true;
        try {
          const r = await post(`/api/land/${land.dataset.id}`);
          if (r.error) alert(r.error); else await draw();
        } finally { land.disabled = false; }
      } else if (retry) {
        retry.disabled = true;
        try {
          const r = await post(`/api/retry/${retry.dataset.id}`);
          alert(r.output || r.error || 'retried');
          go('/tasks');
        } finally { retry.disabled = false; }
      }
    }
    return {
      async show() {
        $('#main').innerHTML = `<div class="two"><section><div id="detail" class="mute" style="margin:16px">loading…</div></section><section><h2>Events · task ${id}</h2><div id="feed" class="feed"></div></section></div>`;
        $('#detail').addEventListener('submit', onSubmit);
        $('#detail').addEventListener('click', onClick);
        await snapshotHead();
        await draw();
        renderFeed();
      },
      onEvent(e) {
        if (e.task === id) {
          renderFeed();
          if (e.type === 'task_done' && e.compare) { compare = e.compare; }
          if (INVALIDATES.detail.includes(e.type)) draw().catch(() => {});
        }
      },
    };
  }

  // ---- plugins view
  function pluginsView() {
    let rows = [];
    function running(p) {
      if (p.state === 'running') return `running pid ${p.pid}, up ${fmtSpan(p.uptime_secs)} (since ${fmtTime(Date.now() / 1000 - p.uptime_secs)})`;
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

  // ---- jobs view: automation runs (docs/JOBS.md), served like /tasks
  // through forge-client's job_list
  function jobsView() {
    function drawRows(rows) {
      $('#job-rows').innerHTML = rows.map(j => `
        <tr class="task" data-id="${j.id}">
          <td class="num">${j.id}</td>
          <td>${j.project ? `<a href="/projects/${encodeURIComponent(j.project)}">${esc(j.project)}</a>` : ''}</td>
          <td>${esc(j.workflow)}</td>
          <td class="state ${esc(j.state)}">${esc(j.state)}${j.dry_run ? ' <span class="mute">(dry run)</span>' : ''}</td>
          <td class="num">${usd(j.cost_usd)}</td>
          <td class="mute" style="white-space:nowrap">${fmtTime(j.started_at)}</td>
          <td class="mute" style="white-space:nowrap">${j.due_at ? `${fmtTime(j.due_at)} (${fmtAgo(j.due_at)})` : ''}</td>
        </tr>`).join('') || '<tr><td colspan="7" class="mute">no jobs</td></tr>';
    }
    async function refresh() { drawRows(await get('/api/jobs')); }
    return {
      async show() {
        $('#main').innerHTML = `
          <h2>Jobs</h2>
          <table><thead><tr><th>id</th><th>project</th><th>workflow</th><th>state</th><th class="num">cost</th><th>started</th><th>due</th></tr></thead><tbody id="job-rows"></tbody></table>`;
        $('#job-rows').addEventListener('click', ev => {
          const tr = ev.target.closest('tr.task');
          if (tr) go(`/jobs/${tr.dataset.id}`);
        });
        await refresh();
      },
      onEvent(e) {
        if (INVALIDATES.jobs.includes(e.type)) refresh().catch(() => {});
      },
    };
  }

  // ---- one job: its steps and effects, served like a task's detail
  // through forge-client's job_show
  function jobView(id) {
    async function draw() {
      const d = await get(`/api/job/${id}`);
      const steps = (d.steps || []).map(s => `
        <tr><td class="num">${s.seq}</td><td>${esc(s.action)}</td><td>${esc(s.kind)}</td>
          <td class="mute">${esc(s.provider)}${s.model ? ' · ' + esc(s.model) : ''}</td>
          <td class="num">${usd(s.cost_usd)}</td><td class="num">${s.exit_code ?? ''}</td></tr>`).join('');
      const effects = (d.effects || []).map(e => `
        <tr><td class="num">${e.seq}</td><td>${esc(e.kind)}</td><td>${esc(e.target)}</td>
          <td>${esc(e.summary)}${e.dry_run ? ' <span class="mute">(dry run)</span>' : ''}</td></tr>`).join('');
      $('#main').innerHTML = `
        <h2>Job ${d.id} <span class="state ${esc(d.state)}">${esc(d.state)}</span> <a href="/jobs">← jobs</a></h2>
        <div class="card">
          <div><span class="k">project</span>${d.project ? `<a href="/projects/${encodeURIComponent(d.project)}">${esc(d.project)}</a>` : ''}</div>
          <div><span class="k">workflow</span>${esc(d.workflow)} <span class="mute">${esc((d.workflow_hash || '').slice(0, 8))} · ${esc(d.workflow_source)}</span></div>
          <div><span class="k">trigger</span>${esc(d.trigger_kind)}${d.trigger_ref ? ' ' + esc(d.trigger_ref) : ''}</div>
          <div><span class="k">cost</span>${usd(d.cost_usd)}${d.dry_run ? ' · dry run' : ''}</div>
          <div><span class="k">started</span>${fmtTime(d.started_at)}${d.finished_at ? ` · finished ${fmtTime(d.finished_at)}` : ''}${d.due_at ? ` · due ${fmtTime(d.due_at)} (${fmtAgo(d.due_at)})` : ''}</div>
        </div>
        <h2>Steps</h2>
        <table><thead><tr><th>seq</th><th>action</th><th>kind</th><th>provider/model</th><th class="num">cost</th><th class="num">exit</th></tr></thead>
          <tbody>${steps || '<tr><td colspan="6" class="mute">no steps</td></tr>'}</tbody></table>
        <h2>Effects</h2>
        <table><thead><tr><th>seq</th><th>kind</th><th>target</th><th>summary</th></tr></thead>
          <tbody>${effects || '<tr><td colspan="4" class="mute">no effects</td></tr>'}</tbody></table>`;
    }
    return {
      async show() { $('#main').innerHTML = '<div class="mute" style="margin:16px">loading…</div>'; await draw(); },
      onEvent(e) { if (INVALIDATES.jobs.includes(e.type)) draw().catch(() => {}); },
    };
  }

  // ---- workflows view: every workflow (operator catalog and each
  // project's repository), through forge-client's workflow_list
  function workflowsView() {
    async function refresh() {
      const rows = await get('/api/workflows');
      $('#workflow-rows').innerHTML = ForgeWorkflows.renderWorkflowRows(rows);
    }
    return {
      async show() {
        $('#main').innerHTML = `
          <h2>Workflows <a href="/workflows/new">+ new</a></h2>
          <table><thead><tr><th>name</th><th>kind</th><th>source</th><th>steps</th><th>measured</th></tr></thead><tbody id="workflow-rows"></tbody></table>`;
        $('#workflow-rows').addEventListener('click', ev => {
          if (ev.target.closest('a')) return;
          const tr = ev.target.closest('tr.task');
          if (!tr) return;
          const project = tr.dataset.project;
          go(project ? `/workflows/${encodeURIComponent(tr.dataset.name)}?project=${encodeURIComponent(project)}` : `/workflows/${encodeURIComponent(tr.dataset.name)}`);
        });
        await refresh();
      },
      onEvent(e) { if (INVALIDATES.workflows.includes(e.type)) refresh().catch(() => {}); },
    };
  }

  // ---- the candidate editor: textarea, lint problems (debounced through
  // the server's lint verb), and Save — shared by the /workflows/<name>
  // editor and the /workflows/new prompter once its draft has loaded, so
  // Save behaves identically either way.
  function editorHtml(text, saveLabel) {
    return `
      <div class="card"><textarea id="wf-text" spellcheck="false" style="width:100%;height:50vh">${esc(text)}</textarea></div>
      <div class="card" id="wf-problems"></div>
      <div class="card">
        <input id="wf-message" placeholder="commit message" style="width:50%">
        <button id="wf-save">${saveLabel}</button>
        <span id="wf-save-result" class="mute"></span>
      </div>`;
  }
  // Wires the elements `editorHtml` renders (assumed already in the DOM),
  // linting once immediately; returns a teardown that cancels the pending
  // debounce.
  function wireEditor(name, project) {
    let lintTimer = null, lintSeq = 0;
    async function lint() {
      const mySeq = ++lintSeq;
      const candidate = $('#wf-text').value;
      let doc;
      try { doc = await postBody(`/api/workflows/${encodeURIComponent(name)}/lint`, candidate, 'text/plain'); }
      catch { return; }
      if (mySeq !== lintSeq) return; // a newer keystroke already started another lint
      const el = $('#wf-problems');
      if (el) el.innerHTML = ForgeWorkflows.renderLintProblems(doc.problems || []);
    }
    async function save() {
      const btn = $('#wf-save'); if (!btn) return;
      btn.disabled = true;
      try {
        const doc = await postBody(`/api/workflows/${encodeURIComponent(name)}`, JSON.stringify({
          text: $('#wf-text').value, message: $('#wf-message').value, project,
        }), 'application/json');
        $('#wf-save-result').textContent = doc.error ? doc.error
          : doc.result === 'filed' ? `filed as task ${doc.task_id}`
          : `committed ${(doc.hash || '').slice(0, 8)}`;
      } catch (e) {
        $('#wf-save-result').textContent = String(e);
      } finally { btn.disabled = false; }
    }
    $('#wf-text').addEventListener('input', () => { clearTimeout(lintTimer); lintTimer = setTimeout(lint, 400); });
    $('#wf-save').addEventListener('click', save);
    lint();
    return () => clearTimeout(lintTimer);
  }

  // ---- one workflow: the file text in the shared editor, the resolved
  // steps and measured profile beside it
  function workflowView(name, project) {
    let teardownEditor = null;
    function draw(d) {
      const saveLabel = d.source === 'repo' ? 'file as a task' : 'save';
      $('#main').innerHTML = `
        <h2>Workflow ${esc(d.name)} <span class="mute">${esc(d.kind)} · ${d.source === 'repo' ? esc(project) : 'catalog'}</span> <a href="/workflows">← workflows</a></h2>
        <div class="two">
          <section>${editorHtml(d.text, saveLabel)}</section>
          <section>
            <h2>Steps</h2>
            <div class="card" id="wf-steps">${ForgeWorkflows.renderSteps(d.steps)}</div>
            <h2>Measured</h2>
            <div class="card" id="wf-profile">${ForgeWorkflows.profileLine(d.measured)}</div>
          </section>
        </div>`;
      teardownEditor = wireEditor(name, project);
    }
    async function refreshMeasured() {
      const q = project ? `?project=${encodeURIComponent(project)}` : '';
      let d;
      try { d = await get(`/api/workflows/${encodeURIComponent(name)}${q}`); } catch { return; }
      const el = $('#wf-profile');
      if (el) el.innerHTML = ForgeWorkflows.profileLine(d.measured);
    }
    return {
      async show() {
        $('#main').innerHTML = '<div class="mute" style="margin:16px">loading…</div>';
        const q = project ? `?project=${encodeURIComponent(project)}` : '';
        draw(await get(`/api/workflows/${encodeURIComponent(name)}${q}`));
      },
      onEvent(e) { if (INVALIDATES.workflows.includes(e.type)) refreshMeasured().catch(() => {}); },
      teardown() { if (teardownEditor) teardownEditor(); },
    };
  }

  // ---- the prompter: /workflows/new, a description box and a "Draft it"
  // control that starts the author-workflow job (docs/WORKFLOWS.md,
  // "Authoring") and shows its progress from the live event stream;
  // when the job ends `ok` the draft loads into the shared editor above
  // its rationale and open questions, and when it ends `needs_human` the
  // human rung's question and a link to it show instead.
  function promptView() {
    // `pending` is true only between the POST to `/api/workflows/draft`
    // and that same request's own response landing — the window in which
    // a live `job_started` for this workflow is worth showing as a
    // provisional status (draftStatusFor, web/src/workflows.js). The id
    // that response returns is the only authoritative one: `finish` is
    // driven by it alone, never by the stream, since a `job_finished`
    // event carries no token saying whose request it belongs to.
    let jobId = null, pending = false, teardownEditor = null;

    function setStatus(text, cls) {
      const el = $('#draft-status');
      if (el) { el.textContent = text; el.className = cls || 'mute'; }
    }

    // The task `job::ask` files for a `needs_human` job: the newest
    // blocked "job question" task in the job's project — the human
    // rung's own question text is that task's `reason` (docs/JOBS.md,
    // "The human rung"). A one-shot read, not polling: triggered once by
    // this job's own `job_finished`.
    async function findQuestion(project) {
      let rows;
      try { rows = await get(`/api/tasks?project=${encodeURIComponent(project)}&state=blocked`); }
      catch { return null; }
      const row = (rows || []).filter(t => t.task === 'job question').sort((a, b) => b.id - a.id)[0];
      if (!row) return null;
      const t = await get(`/api/task/${row.id}`).catch(() => null);
      if (!t || !t.task) return null;
      return { text: t.task.reason, task_id: row.id };
    }

    async function finish(id) {
      let doc;
      try { doc = await get(`/api/job/${id}`); }
      catch { setStatus('could not load the job', 'failed'); return; }
      setStatus(`job ${id} ${doc.state}`, doc.state === 'ok' ? 'succeeded' : (doc.state === 'running' || doc.state === 'queued' ? 'mute' : 'failed'));
      let question = null;
      if (doc.state === 'needs_human') question = await findQuestion(doc.project);
      $('#draft-result').innerHTML = ForgeWorkflows.renderDraftPanel(doc, question);
      const d = ForgeWorkflows.draftOutput(doc);
      const editorEl = $('#draft-editor');
      if (d && editorEl) {
        editorEl.style.display = '';
        editorEl.innerHTML = editorHtml(d.toml, 'save');
        if (teardownEditor) teardownEditor();
        teardownEditor = wireEditor(d.name, null);
      }
    }

    async function start() {
      const btn = $('#draft-go'); if (!btn) return;
      const description = $('#draft-desc').value.trim();
      if (!description) return;
      btn.disabled = true;
      jobId = null;
      $('#draft-result').innerHTML = '';
      const editorEl = $('#draft-editor');
      if (editorEl) { editorEl.style.display = 'none'; editorEl.innerHTML = ''; }
      setStatus('starting…');
      try {
        pending = true;
        const r = await postBody('/api/workflows/draft', JSON.stringify({ description }), 'application/json');
        if (r.error) { setStatus(r.error, 'failed'); return; }
        // `r.job` (from the route's own blocked-until-finished `forge job
        // start`, web/src/main.rs's draft_workflow_route) is the only
        // authoritative id — it may differ from any provisional id an
        // event set above, in which case `finish` below replaces whatever
        // status that showed with this job's real one.
        jobId = r.job;
        if (jobId != null) await finish(jobId);
      } catch (e) {
        setStatus(String(e), 'failed');
      } finally { btn.disabled = false; pending = false; }
    }

    return {
      async show() {
        $('#main').innerHTML = `
          <h2>New workflow <a href="/workflows">← workflows</a></h2>
          <div class="card">
            <textarea id="draft-desc" placeholder="describe the automation or the change you want" style="width:100%;height:8em"></textarea>
            <div><button id="draft-go">Draft it</button> <span id="draft-status" class="mute"></span></div>
          </div>
          <div id="draft-result"></div>
          <section id="draft-editor" style="display:none"></section>`;
        $('#draft-go').addEventListener('click', start);
      },
      onEvent(e) {
        const text = draftStatusFor({ pending }, e);
        if (text !== null) { jobId = e.job_id; setStatus(text); }
      },
      teardown() { if (teardownEditor) teardownEditor(); },
    };
  }

  // ---- stats view: the full record (web UI task 5) — a 30-day chart of
  // daily landings and daily spend above tabbed, sortable tables:
  // workflows (the verified-rate interval as a bar, with the regression
  // mark), quality, by-role, human attention, time to live, and factors
  // (hidden when `forge stats --json` carries no `factors` key at all).
  function statsView() {
    let doc = null;
    let activeTab = null;
    let sort = null;

    function draw() {
      const tabs = ForgeStats.visibleTabs(doc);
      if (!tabs.some(t => t.id === activeTab)) activeTab = tabs.length ? tabs[0].id : null;
      const nav = tabs.map(t => `<button type="button" class="tab${t.id === activeTab ? ' active' : ''}" data-tab="${t.id}">${esc(t.label)}</button>`).join('');
      const chart = ForgeStats.renderDailyChart(doc.daily || []);
      const body = activeTab ? ForgeStats.renderTab(activeTab, doc, sort) : '<div class="mute">no data</div>';
      $('#main').innerHTML = `
        <h2>Stats</h2>
        <div class="chart-wrap"><svg id="stats-chart" width="${chart.width}" height="${chart.height}" viewBox="0 0 ${chart.width} ${chart.height}">${chart.svgHtml}</svg></div>
        <div class="tabs" id="stats-tabs">${nav}</div>
        <div id="stats-body">${body}</div>`;
      $('#stats-tabs').addEventListener('click', ev => {
        const b = ev.target.closest('button[data-tab]');
        if (!b) return;
        activeTab = b.dataset.tab;
        sort = null;
        draw();
      });
      $('#stats-body').addEventListener('click', ev => {
        const th = ev.target.closest('th[data-table]');
        if (!th) return;
        const table = th.dataset.table, key = th.dataset.key;
        const dir = sort && sort.table === table && sort.key === key && sort.dir === 'asc' ? 'desc' : 'asc';
        sort = { table, key, dir };
        draw();
      });
    }
    return {
      async show() {
        doc = await get('/api/stats');
        draw();
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

  // ---- one initiative: the outcome, its tasks (state and cost), what
  // verification refused, what the supervisor ruled, what reached the
  // operator, deploys, cost against budget as a bar, elapsed time, and —
  // while held — the reason (task 532, "the initiative page in full").
  // Rendering itself lives in `web/src/initiative.js` (`renderInitiativeDoc`),
  // tested under `node` without a DOM by `web/tests/initiative_render.rs`.
  function initiativeView(id) {
    let taskIds = new Set();
    async function draw() {
      const d = await get(`/api/initiatives/${id}`);
      taskIds = new Set((d.tasks || []).map(t => t.id));
      $('#ini-page').innerHTML = ForgeInitiative.renderInitiativeDoc(d, fmtSpan);
    }
    async function onSubmit(ev) {
      const setForm = ev.target.closest('form.ini-set');
      const withdrawForm = ev.target.closest('form.req-withdraw');
      if (setForm) {
        ev.preventDefault();
        const budget = setForm.querySelector('.ini-budget').value.trim();
        const stopAfter = setForm.querySelector('.ini-stop-after').value.trim();
        const body = {};
        if (budget !== '') body.budget = Number(budget);
        if (stopAfter !== '') body.stop_after = Number(stopAfter);
        if (!Object.keys(body).length) return;
        const btn = setForm.querySelector('button');
        btn.disabled = true;
        try {
          const r = await postBody(`/api/initiatives/${id}`, JSON.stringify(body), 'application/json');
          if (r.error) alert(r.error); else await draw();
        } finally { btn.disabled = false; }
      } else if (withdrawForm) {
        ev.preventDefault();
        const value = withdrawForm.querySelector('.req-withdraw-reason').value.trim();
        if (!value) return;
        const btn = withdrawForm.querySelector('button');
        btn.disabled = true;
        try {
          const r = await postBody(`/api/withdraw/${withdrawForm.dataset.id}`, JSON.stringify({ reason: value }), 'application/json');
          if (r.error) alert(r.error); else await draw();
        } finally { btn.disabled = false; }
      }
    }
    return {
      async show() {
        // A wrapper div, not `#main` itself, carries the delegated
        // listener — same reasoning as `requestsView`'s `#req-page`:
        // `#main` persists across navigations, this div is fresh each
        // visit.
        $('#main').innerHTML = '<div id="ini-page" class="mute" style="margin:16px">loading…</div>';
        $('#ini-page').addEventListener('submit', onSubmit);
        await draw();
      },
      onEvent(e) {
        // The initiative's own record closing (`initiative_settled`,
        // tagged with this id) or any event about one of its own tasks
        // that would also invalidate that task's detail page.
        if (e.type === 'initiative_settled' && e.id === id) { draw().catch(() => {}); return; }
        if (taskIds.has(e.task) && INVALIDATES.detail.includes(e.type)) draw().catch(() => {});
      },
    };
  }

  // ---- deploys: every project's targets — method, host, the last
  // deploy's check/smoke/look verdicts — each with its own full deploy
  // log and, per deploy, the deploy-look screenshot shown inline (task
  // 534, "deploys"). Rendering lives in web/src/deploys.js
  // (renderDeploys), tested under node without a DOM by
  // web/tests/deploys_render.rs against tests/fixtures/deploys.json.
  function deploysView() {
    async function draw() {
      const targets = await get('/api/deploys');
      $('#deploys-page').innerHTML = ForgeDeploys.renderDeploys(targets, fmtTime);
    }
    async function onSubmit(ev) {
      const form = ev.target.closest('form.deploy-now');
      if (!form) return;
      ev.preventDefault();
      const { project, target } = form.dataset;
      if (!confirm(`Deploy ${project}/${target} now?`)) return;
      const btn = form.querySelector('button');
      btn.disabled = true;
      try {
        const r = await post(`/api/deploys/run/${encodeURIComponent(project)}/${encodeURIComponent(target)}`);
        if (r.error) alert(r.error);
        await draw();
      } finally { btn.disabled = false; }
    }
    return {
      async show() {
        // A wrapper div, not `#main` itself, carries the delegated
        // listener — same reasoning as `requestsView`'s `#req-page`.
        $('#main').innerHTML = '<h2>Deploys</h2><div id="deploys-page" class="mute" style="margin:16px">loading…</div>';
        $('#deploys-page').addEventListener('submit', onSubmit);
        await draw();
      },
      onEvent(e) { if (INVALIDATES.deploys.includes(e.type)) draw().catch(() => {}); },
    };
  }

  // ---- activity: the live event stream as a feed, newest first,
  // filtered by project/kind/task, paged back through `/api/activity`
  // (`forge events --since`), plus a running-attempts panel built by
  // replaying attempt events (web UI task 8, "activity"). Rendering
  // lives in web/src/activity.js (renderFeed/reduceRunning/
  // renderRunningAttempts), tested under node without a DOM by
  // web/tests/activity_render.rs against tests/fixtures/activity.json.
  // History before the snapshot's own `events_offset` comes from
  // `/api/activity`; everything from `events_offset` on is the shared
  // live `feed` array every other view already subscribes to — the two
  // never overlap, so `combined()` below is just their concatenation,
  // both ascending.
  function activityView() {
    const filters = { project: '', kind: '', task: '' };
    let history = [];
    let cursor = null, historyDone = false, loadingMore = false;
    const taskProjects = {};
    let observer = null;

    async function loadTaskProjects() {
      try {
        for (const t of await get('/api/tasks?limit=500')) {
          if (t.project) taskProjects[t.id] = t.project;
        }
      } catch { /* best-effort: a task outside this window just shows no project */ }
    }
    const combined = () => history.concat(feed);

    function draw() {
      const runningIds = (headData.tasks || []).filter(t => t.state === 'running').map(t => t.id);
      const running = ForgeActivity.reduceRunning(combined(), runningIds);
      $('#activity-running').innerHTML = ForgeActivity.renderRunningAttempts(running);
      $('#activity-feed').innerHTML = ForgeActivity.renderFeed(combined(), filters, fmtTime, taskProjects);
    }
    async function loadMore() {
      if (loadingMore || historyDone) return;
      loadingMore = true;
      try {
        const before = cursor != null ? cursor : offset;
        const q = new URLSearchParams({ before, limit: 200 });
        const doc = await get(`/api/activity?${q}`);
        history = (doc.events || []).concat(history);
        cursor = doc.next_before;
        historyDone = !doc.events || !doc.events.length || doc.next_before == null;
        draw();
        $('#activity-sentinel').textContent = historyDone ? 'start of the log' : 'loading more…';
      } finally { loadingMore = false; }
    }
    return {
      async show() {
        $('#main').innerHTML = `
          <h2>Activity</h2>
          <h3>Running attempts</h3>
          <div id="activity-running" class="mute">loading…</div>
          <h3>Feed</h3>
          <div class="filters">
            <select id="f-a-kind"><option value="">any kind</option>${ForgeActivity.KINDS.map(k => `<option>${esc(k)}</option>`).join('')}</select>
            <select id="f-a-project"><option value="">any project</option></select>
            <input type="text" id="f-a-task" placeholder="task id" style="width:8em">
          </div>
          <div id="activity-feed" class="mute">loading…</div>
          <div id="activity-sentinel" class="sentinel">loading…</div>`;
        $('#f-a-kind').addEventListener('change', ev => { filters.kind = ev.target.value; draw(); });
        $('#f-a-project').addEventListener('change', ev => { filters.project = ev.target.value; draw(); });
        $('#f-a-task').addEventListener('input', ev => { filters.task = ev.target.value.trim(); draw(); });
        get('/api/projects').then(rows => {
          $('#f-a-project').innerHTML = '<option value="">any project</option>' + rows.map(p => `<option value="${esc(p.name)}">${esc(p.name)}</option>`).join('');
        }).catch(() => {});
        await snapshotHead();
        await loadTaskProjects();
        cursor = offset;
        await loadMore();
        observer = new IntersectionObserver(entries => { if (entries.some(e => e.isIntersecting)) loadMore(); }, { rootMargin: '400px' });
        observer.observe($('#activity-sentinel'));
      },
      onEvent() { draw(); },
      teardown() { if (observer) observer.disconnect(); },
    };
  }

  // ---- messages: per project the message record (`/api/messages/
  // <project>`, `forge message list PROJECT --json`), the concierge's
  // own decisions on inbound messages, the questions addressed to
  // contacts and their state, and the jobs a message triggered — a
  // filter by contact and a search over text (web UI task 10,
  // "messages"). Rendering lives in web/src/messages.js
  // (renderMessagesDoc), tested under node without a DOM by
  // web/tests/messages_render.rs against tests/fixtures/messages.json.
  function messagesView() {
    let project = '';
    let doc = { messages: [], decisions: [], questions: [], jobs: [] };
    const filters = { contact: '', q: '' };

    function draw() {
      $('#messages-page').innerHTML = project
        ? ForgeMessages.renderMessagesDoc(doc, filters, fmtTime)
        : '<div class="mute">Pick a project.</div>';
    }
    async function load() {
      if (!project) { draw(); return; }
      doc = await get(`/api/messages/${encodeURIComponent(project)}`);
      $('#f-m-contact').innerHTML = '<option value="">any contact</option>'
        + ForgeMessages.contactsOf(doc.messages).map(c => `<option>${esc(c)}</option>`).join('');
      draw();
    }
    return {
      async show() {
        $('#main').innerHTML = `
          <h2>Messages</h2>
          <div class="filters">
            <select id="f-m-project"><option value="">choose a project</option></select>
            <select id="f-m-contact"><option value="">any contact</option></select>
            <input type="text" id="f-m-q" placeholder="search text" style="width:16em">
          </div>
          <div id="messages-page" class="mute">loading…</div>`;
        $('#f-m-project').addEventListener('change', ev => { project = ev.target.value; load().catch(() => {}); });
        $('#f-m-contact').addEventListener('change', ev => { filters.contact = ev.target.value; draw(); });
        $('#f-m-q').addEventListener('input', ev => { filters.q = ev.target.value.trim(); draw(); });
        try {
          const rows = await get('/api/projects');
          $('#f-m-project').innerHTML = '<option value="">choose a project</option>'
            + rows.map(p => `<option value="${esc(p.name)}">${esc(p.name)}</option>`).join('');
          if (rows.length === 1) { project = rows[0].name; $('#f-m-project').value = project; }
        } catch { /* best-effort: an empty picker just leaves the page asking */ }
        await load();
      },
      onEvent(e) { if (project && INVALIDATES.messages.includes(e.type)) load().catch(() => {}); },
    };
  }

  // ---- doctor: every `forge doctor --json` check as a row, held
  // initiatives with the same budget/stop-after control the initiative
  // page uses (task 4's `POST /api/initiatives/<id>`), retained
  // worktrees with a gc control (`POST /api/gc`, `forge gc`), the rate
  // windows, spend, and the learning lines — refreshed on demand and
  // every 60 seconds (web UI task 7, "doctor"). Rendering lives in
  // web/src/doctor.js (renderDoctorDoc), tested under node without a DOM
  // by web/tests/doctor_render.rs against tests/fixtures/doctor.json.
  function doctorView() {
    let timer = null;
    async function draw() {
      const [checks, initiatives] = await Promise.all([
        get('/api/doctor'),
        get('/api/initiatives').catch(() => []),
      ]);
      const held = (initiatives || []).filter(i => i.state === 'held');
      $('#doctor-page').innerHTML = ForgeDoctor.renderDoctorDoc(checks, held, fmtTime);
    }
    async function onSubmit(ev) {
      const refreshForm = ev.target.closest('form.doctor-refresh');
      const gcForm = ev.target.closest('form.doctor-gc');
      const iniForm = ev.target.closest('form.ini-set');
      if (refreshForm) {
        ev.preventDefault();
        await draw();
        return;
      }
      if (gcForm) {
        ev.preventDefault();
        if (!confirm('Run forge gc now?')) return;
        const btn = gcForm.querySelector('button');
        btn.disabled = true;
        try {
          const r = await post('/api/gc');
          if (r.error) alert(r.error);
          await draw();
        } finally { btn.disabled = false; }
        return;
      }
      if (iniForm) {
        ev.preventDefault();
        const budget = iniForm.querySelector('.ini-budget').value.trim();
        const stopAfter = iniForm.querySelector('.ini-stop-after').value.trim();
        const body = {};
        if (budget !== '') body.budget = Number(budget);
        if (stopAfter !== '') body.stop_after = Number(stopAfter);
        if (!Object.keys(body).length) return;
        const btn = iniForm.querySelector('button');
        btn.disabled = true;
        try {
          const r = await postBody(`/api/initiatives/${iniForm.dataset.id}`, JSON.stringify(body), 'application/json');
          if (r.error) alert(r.error); else await draw();
        } finally { btn.disabled = false; }
      }
    }
    return {
      async show() {
        // A wrapper div, not `#main` itself, carries the delegated
        // listener — same reasoning as `requestsView`'s `#req-page`.
        $('#main').innerHTML = '<div id="doctor-page" class="mute" style="margin:16px">loading…</div>';
        $('#doctor-page').addEventListener('submit', onSubmit);
        await draw();
        timer = setInterval(() => draw().catch(() => {}), 60000);
      },
      teardown() { if (timer) clearInterval(timer); },
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

  // ---- graph pages: a project selector shared by the file view and the
  // module view, since both just draw whatever `?repo=` names.
  async function projectRepos() {
    let rows = [];
    try { rows = await get('/api/projects'); } catch { rows = []; }
    return rows
      .map(p => ({ name: p.name, repo: (p.repos && p.repos[0] && p.repos[0].repo) || '' }))
      .filter(p => p.repo);
  }
  function wireProjectSelector(modules, repo) {
    const sel = $('#graph-project');
    if (!sel) return;
    projectRepos().then(projects => {
      const opts = ['<option value="">— pick a project —</option>']
        .concat(projects.map(p => `<option value="${esc(p.repo)}" ${p.repo === repo ? 'selected' : ''}>${esc(p.name)}</option>`));
      sel.innerHTML = opts.join('');
    });
    sel.addEventListener('change', () => {
      const next = sel.value;
      go(`/graph${modules ? '/modules' : ''}${next ? `?repo=${encodeURIComponent(next)}` : ''}`);
    });
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
            <select id="graph-project"><option value="">loading projects…</option></select>
            <span class="mute">${esc(repo) || 'no repo given'}</span>
            <input type="search" id="f-graph" placeholder="filter files" ${repo ? '' : 'disabled'}>
          </div>
          <div style="overflow:auto; padding:0 16px 24px"><svg id="graph-svg"></svg></div>`;
        wireProjectSelector(false, repo);
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

  // ---- graph modules view: the repository graph at module granularity
  // (docs/LATER.md, "The code visualiser"), from `forge graph --json`
  // through `/api/graph/modules` — module nodes sized by lines, the
  // record's overlay as a cost colour and a demotion badge
  // (`src/graph.js`'s `renderModuleGraph`). Hovering a node lists its
  // tasks through the SVG's own `<title>`.
  function graphModulesView(repo) {
    let graph = null, filter = '', selected = null;
    function draw() {
      const svg = $('#graph-svg');
      if (!svg || !graph) return;
      const { svgHtml, width, height } = ForgeGraph.renderModuleGraph(graph, { filterQuery: filter, selected });
      svg.setAttribute('width', width);
      svg.setAttribute('height', height);
      svg.innerHTML = svgHtml;
    }
    return {
      async show() {
        $('#main').innerHTML = `
          <h2>Graph — modules</h2>
          <div class="filters">
            <select id="graph-project"><option value="">loading projects…</option></select>
            <span class="mute">${esc(repo) || 'no repo given'}</span>
            <input type="search" id="f-graph" placeholder="filter modules" ${repo ? '' : 'disabled'}>
          </div>
          <div style="overflow:auto; padding:0 16px 24px"><svg id="graph-svg"></svg></div>`;
        wireProjectSelector(true, repo);
        if (!repo) return;
        $('#f-graph').addEventListener('input', ev => { filter = ev.target.value; draw(); });
        $('#graph-svg').addEventListener('click', ev => {
          const g = ev.target.closest('.mnode');
          const path = g ? g.dataset.path : null;
          selected = (path && path !== selected) ? path : null;
          draw();
        });
        try { graph = await get(`/api/graph/modules?repo=${encodeURIComponent(repo)}`); } catch { graph = null; }
        if (!graph || !Array.isArray(graph.nodes)) graph = { nodes: [], edges: [] };
        draw();
      },
    };
  }

  $('#refresh').addEventListener('click', () => render());
  setInterval(() => snapshotHead().catch(() => {}), 30000);
  render().catch(console.error);
})();

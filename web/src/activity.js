// Pure rendering for the /activity page (web UI task 8, "activity"): the
// live event stream as a feed, newest first, filterable by project, kind
// and task, plus a running-attempts panel built by replaying the same
// stream. No DOM, no fetch, so web/tests/activity.test.js can run it
// under node exactly like doctor.js/deploys.js, against a fixture built
// from tests/fixtures/activity.json.
(function (root, factory) {
  const api = factory();
  if (typeof module === 'object' && module.exports) module.exports = api;
  else root.ForgeActivity = api;
})(globalThis, () => {
  const esc = s => String(s ?? '').replace(/[&<>"]/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));
  const usd = n => '$' + (Number(n) || 0).toFixed(2);

  // The raw `type` of every event the feed shows, mapped to the label the
  // task names (docs/CLIENT.md's Events table, restricted to this set —
  // `task_queued`, `tool_call`, `agent_done`, `git_counted`,
  // `push_failed`, `push_skipped`, `note`, `op`, `project_created` and
  // `task_withdrawn` never appear as a feed row, only as raw material for
  // the running-attempts panel below or the terminal's own printer).
  const FEED_KIND_OF = {
    task_started: 'task started',
    attempt_started: 'attempt started',
    attempt_done: 'attempt done',
    check: 'check',
    pushed: 'pushed',
    task_done: 'task done',
    deploy_started: 'deploy started',
    deploy_finished: 'deploy finished',
    job_started: 'job started',
    job_finished: 'job finished',
    initiative_settled: 'initiative settled',
  };

  // Every kind the feed can show, in the task's own order — the filter
  // dropdown's option list.
  const KINDS = [
    'task started', 'attempt started', 'attempt done', 'check', 'pushed',
    'task done', 'question', 'deploy started', 'deploy finished',
    'job started', 'job finished', 'initiative settled',
  ];

  // A `task_done` event is a question, not a plain "task done" row, when
  // the task blocked on `request_kind`'s own "needs input" reason
  // (src/view.rs's `request_kind`, mirrored here since the raw event
  // carries no `kind` field of its own).
  function isQuestion(e) {
    return e.type === 'task_done' && e.state === 'blocked'
      && typeof e.reason === 'string' && e.reason.startsWith('needs input:');
  }

  // The feed kind of one event, or `null` when it isn't one the feed
  // shows at all.
  function kindOf(e) {
    if (isQuestion(e)) return 'question';
    return FEED_KIND_OF[e.type] || null;
  }

  // The project an event belongs to: `deploy_*`/`job_*` events carry it
  // directly; every other event only carries `task`, so the caller's own
  // id → project map (built from the task list, e.g. `/api/tasks`) fills
  // it in.
  function projectOf(e, taskProjects) {
    if (e.project) return e.project;
    if (taskProjects && e.task != null && taskProjects[e.task]) return taskProjects[e.task];
    return null;
  }

  function matchesFilters(e, filters, taskProjects) {
    const kind = kindOf(e);
    if (!kind) return false;
    if (!filters) return true;
    if (filters.kind && filters.kind !== kind) return false;
    if (filters.project && projectOf(e, taskProjects) !== filters.project) return false;
    if (filters.task && String(e.task) !== String(filters.task)) return false;
    return true;
  }

  // A badge's state class, reusing the palette every other page already
  // draws task states in (`.state .succeeded/.failed/.blocked`, styles.css).
  function badgeClass(e, kind) {
    if (kind === 'check') return e.ok ? 'succeeded' : 'failed';
    if (kind === 'deploy finished') return e.ok ? 'succeeded' : 'failed';
    if (kind === 'question') return 'blocked';
    if (kind === 'attempt done' || kind === 'task done') return e.state || 'mute';
    if (kind === 'job finished') return e.state === 'ok' ? 'succeeded' : (e.state === 'needs_human' ? 'blocked' : 'failed');
    if (kind === 'initiative settled') return e.state === 'done' ? 'succeeded' : 'mute';
    return 'mute';
  }

  function renderFeedRow(e, fmtTime, taskProjects) {
    const kind = kindOf(e);
    const cls = badgeClass(e, kind);
    const project = projectOf(e, taskProjects);
    const taskLink = e.task ? ` <a href="/tasks/${e.task}">task ${e.task}</a>` : '';
    return `<div class="card activity-row" data-kind="${esc(kind)}" data-task="${e.task ?? ''}">
      <span class="mute">${esc(fmtTime(e.ts))}</span>
      <span class="state ${esc(cls)}">${esc(kind)}</span>
      ${project ? `<span class="mute">${esc(project)}</span>` : ''}${taskLink}
      <div>${esc(e.text)}</div>
    </div>`;
  }

  // `events` is ascending (oldest first — the order `events.jsonl`/`forge
  // events`/the live `feed` array already carry); the feed itself is
  // always newest first, so this reverses after filtering.
  function renderFeed(events, filters, fmtTime, taskProjects) {
    const rows = (events || [])
      .filter(e => matchesFilters(e, filters, taskProjects))
      .slice()
      .reverse()
      .map(e => renderFeedRow(e, fmtTime, taskProjects))
      .join('');
    return rows || '<div class="mute">no matching activity</div>';
  }

  // Replays `events` (ascending) into the running-attempts panel's own
  // state: one entry per task with an attempt currently in flight,
  // starting from `seedTaskIds` (tasks the last snapshot already showed
  // as `running`, so a task whose `attempt_started` fell outside the
  // window of events this client has loaded still shows up). `tool_call`
  // only ever refines an entry `attempt_started` already opened — a tool
  // call for a task this replay has not seen start is not a task the
  // panel knows is still running (it may already be done), so it is
  // ignored rather than guessed into existence.
  function reduceRunning(events, seedTaskIds) {
    const running = new Map();
    for (const id of seedTaskIds || []) {
      running.set(id, { task: id, n: null, of: null, toolCalls: 0, lastTool: null, turns: null, cost: null });
    }
    for (const e of events || []) {
      const id = e.task;
      if (id == null) continue;
      if (e.type === 'attempt_started') {
        running.set(id, { task: id, n: e.n, of: e.of, toolCalls: 0, lastTool: null, turns: null, cost: null });
      } else if (e.type === 'tool_call') {
        const r = running.get(id);
        if (r) { r.toolCalls += 1; r.lastTool = e.name; }
      } else if (e.type === 'agent_done') {
        const r = running.get(id);
        if (r) { r.turns = e.turns; r.cost = e.cost_usd; }
      } else if (e.type === 'attempt_done' || e.type === 'task_done') {
        running.delete(id);
      }
    }
    return [...running.values()].sort((a, b) => a.task - b.task);
  }

  function renderRunningAttemptRow(r) {
    const step = r.n != null ? `attempt ${r.n} of ${r.of}` : '—';
    return `<tr>
      <td><a href="/tasks/${r.task}">${r.task}</a></td>
      <td>${esc(step)}</td>
      <td class="num">${r.turns != null ? r.turns : '—'}</td>
      <td class="num">${r.toolCalls}</td>
      <td class="num">${r.cost != null ? usd(r.cost) : '—'}</td>
      <td class="mute">${esc(r.lastTool || '')}</td>
    </tr>`;
  }

  function renderRunningAttempts(running) {
    if (!running || !running.length) return '<div class="mute">no attempts running</div>';
    const rows = running.map(renderRunningAttemptRow).join('');
    return `<table><thead><tr><th>task</th><th>step</th><th class="num">turns</th><th class="num">tool calls</th><th class="num">cost</th><th>last tool call</th></tr></thead><tbody>${rows}</tbody></table>`;
  }

  return {
    KINDS, kindOf, projectOf, matchesFilters, renderFeedRow, renderFeed,
    reduceRunning, renderRunningAttempts,
  };
});

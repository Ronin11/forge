// Seeds the test daemon over its HTTP API, mirroring internal/controlplane/ui_test.go
// and server_test.go: register a synthetic worker (which advertises the "demo"
// repository), then drive the real worker protocol — claim → heartbeats → events →
// complete — to leave tasks in rich states. No worker process and no executor run;
// the daemon's own spawned worker advertises no repositories, so it can never claim
// these tasks (scheduler.Pick skips repositories another worker advertises).
import { readFileSync, writeFileSync, mkdirSync } from 'fs';
import path from 'path';

export const WORKER_ID = '0123456789abcdef0123456789abcdef';

async function api(base, method, p, body, token, expect) {
  const headers = { 'Content-Type': 'application/json' };
  if (token) headers.Authorization = `Bearer ${token}`;
  const res = await fetch(base + p, {
    method,
    headers,
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await res.text();
  if (res.status !== expect) throw new Error(`${method} ${p} = ${res.status} (want ${expect}): ${text}`);
  return text ? JSON.parse(text) : null;
}

// Worker registration: over TCP this route requires the daemon token.
function registerBody() {
  return {
    worker_id: WORKER_ID,
    name: 'laptop',
    version: 'ui-test',
    max_concurrent: 2,
    active: 0,
    executors: ['claude-code'],
    capabilities: { sandbox: 'ready' },
    repositories: [{ name: 'demo', path: '/tmp/forge-ui-demo', origin_identity: 'github.com/x/demo', project: 'default' }],
    retained: [],
  };
}

// The span_end events the facts table and the timeline render: fetch 1.2ms,
// agent 5s, verify 800ms (durations in microseconds).
function spanEvents() {
  const t = new Date().toISOString();
  return [
    { seq: 0, time: t, kind: 'span_start', span_id: 'fetch', name: 'fetch', elapsed_us: 100 },
    { seq: 1, time: t, kind: 'span_end', span_id: 'fetch', name: 'fetch', duration_us: 1200, elapsed_us: 1300 },
    { seq: 2, time: t, kind: 'span_start', span_id: 'agent-1', name: 'agent-1', elapsed_us: 1500 },
    { seq: 3, time: t, kind: 'span_end', span_id: 'agent-1', name: 'agent-1', duration_us: 5_000_000, elapsed_us: 5_001_500 },
    { seq: 4, time: t, kind: 'span_start', span_id: 'verify', name: 'verify', elapsed_us: 5_100_000 },
    { seq: 5, time: t, kind: 'span_end', span_id: 'verify', name: 'verify', duration_us: 800_000, elapsed_us: 5_900_000 },
  ];
}

function completeBody(state, lease, extra) {
  const now = Date.now();
  return {
    lease_token: lease,
    state,
    exit_code: 0,
    num_turns: 3,
    usage: { input_tokens: 1200, output_tokens: 340, cache_read_tokens: 50, cache_creation_tokens: 10 },
    cost_usd: 0.0123,
    launches: 1,
    git: { dirty: false, commits: 1, files_changed: 2, insertions: 10, deletions: 2, pushed: false, head: 'abc1234def5678' },
    verification: { level: 1, passed: true },
    cleanup: { outcome: 'removed', reason: 'clean' },
    output_path: '',
    output_bytes: 0,
    started_at: new Date(now - 60_000).toISOString(),
    finished_at: new Date(now).toISOString(),
    ...extra,
  };
}

export async function seed(base, home) {
  const token = readFileSync(path.join(home, 'token'), 'utf8').trim();
  const call = (method, p, body, expect) => api(base, method, p, body, null, expect);
  const worker = (method, p, body, expect) => api(base, method, p, body, token, expect);

  await worker('POST', '/api/v1/worker/register', registerBody(), 200);

  // One attempt lifecycle: claim the single pending target, heartbeat into
  // running (with git context so the task page renders the Git line), then post
  // the terminal report. Tasks are created one at a time so each claim is
  // unambiguous.
  async function runAttempt(reqID, session, branch, events, complete) {
    const lease = `lease-${reqID}`;
    const claim = await worker('POST', '/api/v1/worker/claim', { worker_id: WORKER_ID, claim_request_id: reqID, lease_token: lease }, 200);
    const hb = (body) => worker('POST', `/api/v1/attempts/${claim.attempt_id}/heartbeat`, { lease_token: lease, ...body }, 200);
    await hb({ phase: 'preparing', state: 'preparing' });
    await hb({
      phase: 'running', state: 'running', pid: 4242, pid_start: 7, session_id: session,
      worktree: '/tmp/forge-ui-wt', branch, base_branch: 'main', base_commit: '1111111abcdef22',
    });
    if (events) {
      await worker('POST', `/api/v1/attempts/${claim.attempt_id}/events`, { source: 'worker', events }, 200);
    }
    await worker('POST', `/api/v1/attempts/${claim.attempt_id}/complete`, { ...complete, session_id: session }, 200);
    return claim;
  }

  // 1. A succeeded task with events (facts + timeline), a result, usage, cost.
  const succeeded = await call('POST', '/api/v1/tasks', {
    prompt: 'Inventory the demo repo\nList every file and count them.', repositories: ['demo'],
  }, 201);
  const claimA = await runAttempt('ui-1', 'sess-ui-1', 'forge/ui-1', spanEvents(),
    completeBody('succeeded', 'lease-ui-1', { result_text: 'Inventory complete: 42 files tracked.' }));

  // 2. A failed task, so the dashboard shows a failed chip and stats a failure mix.
  const failed = await call('POST', '/api/v1/tasks', {
    prompt: 'Fail on purpose for the UI fixture', repositories: ['demo'],
  }, 201);
  const claimB = await runAttempt('ui-2', 'sess-ui-2', 'forge/ui-2', null,
    completeBody('failed', 'lease-ui-2', {
      failure_reason: 'exit_nonzero', exit_code: 1, is_error: true, cost_usd: 0.002,
      verification: { level: 1, passed: false, reason: 'agent exited 1' },
      result_text: 'The build is broken as requested.',
    }));

  // 3. A task waiting on a human question (drives /attention and the answer form).
  const waiting = await call('POST', '/api/v1/tasks', {
    prompt: 'Rename the default branch of demo', repositories: ['demo'],
  }, 201);
  const claimC = await runAttempt('ui-3', 'sess-ui-3', 'forge/ui-3', null,
    completeBody('waiting_human', 'lease-ui-3', {
      cleanup: { outcome: 'retained', reason: 'waiting for an answer' },
      question: {
        text: 'Which branch should I target?', options: ['main', 'dev'],
        // Dynamic Human-queue actions: a same-origin link and a registered
        // trigger render as a link and a button on the question card.
        context: { actions: [
          { label: 'Open the doc', url: '/kb/ui-test-brief' },
          { label: 'Send test toast', trigger: 'notify_test' },
        ] },
      },
    }));
  const waitingDetail = await call('GET', `/api/v1/tasks/${waiting.work.id}`, undefined, 200);
  const questionID = waitingDetail.questions[0].id;

  // 4. Open queue: A (pending), B blocked by A, C at lower priority — the same
  // shape as the Go move test, so the drag refusal (B above A) is reachable.
  const qa = await call('POST', '/api/v1/tasks', { prompt: 'Queue task A: refactor the parser', repositories: ['demo'] }, 201);
  const qb = await call('POST', '/api/v1/work', { prompt: 'Queue task B: depends on A', repositories: ['demo'], after: [qa.work.id] }, 201);
  const qc = await call('POST', '/api/v1/tasks', { prompt: 'Queue task C: low priority chore', repositories: ['demo'], priority: 50 }, 201);

  // 5. Proposals: one kept proposed (the dashboard and human queue render it)
  // and one the browser test decides — reject is terminal whatever the apply
  // engine does with a kind, so a retry stays green.
  const propKeep = await call('POST', '/api/v1/proposals', {
    kind: 'process', target: 'routine:ad-hoc', after: { priority: 40 },
    rationale: 'Lower the ad-hoc timeout: p95 sits far below it',
    verification_plan: 'Watch the next five ad-hoc runs for timeouts',
  }, 201);
  const propDecide = await call('POST', '/api/v1/proposals', {
    kind: 'doc', target: 'kb:ui-test-brief',
    rationale: 'Fold the retro findings into one kb note',
    verification_plan: 'The note exists and links the problem attempts',
  }, 201);

  // Seed one kb note as a file (notes are Markdown on disk) and reindex it, so
  // the Knowledge page has real content with headings, a list, code, a table,
  // and a [[wiki link]] to exercise the goldmark renderer.
  mkdirSync(path.join(home, 'kb'), { recursive: true });
  writeFileSync(path.join(home, 'kb', 'ui-test-brief.md'),
    ['---', 'id: ui-test-brief', 'title: "UI Test Brief"', 'type: note',
     'created: 2026-01-01T00:00:00Z', 'tags: [ui-test, brief]', '---', '',
     '## Overview', '', 'A **seeded** note linking to [[ui-test-brief]] with:', '',
     '- first item with `inline code`', '- second item', '', '## Table', '',
     '| a | b |', '| - | - |', '| 1 | 2 |', ''].join('\n'));
  await call('POST', '/api/v1/kb/reindex', {}, 200);

  // Repository controls: give the demo repository a running-app URL so its page
  // and the dashboard strip render an "Open app" link (M12+ repository controls).
  await call('POST', '/api/v1/repositories/demo/app-url', { url: 'http://127.0.0.1:5173' }, 200);

  // Refresh last_seen so the workers card still shows "connected" (90s window)
  // when the browser tests run.
  await worker('POST', '/api/v1/worker/register', registerBody(), 200);

  return {
    worker_id: WORKER_ID,
    succeeded: { work_id: succeeded.work.id, attempt_id: claimA.attempt_id },
    failed: { work_id: failed.work.id, attempt_id: claimB.attempt_id },
    waiting: { work_id: waiting.work.id, attempt_id: claimC.attempt_id, question_id: questionID },
    queue: { a: qa.work.id, b: qb.work.id, c: qc.work.id },
    proposals: { keep: propKeep.id, decide: propDecide.id },
    kb: { id: 'ui-test-brief', title: 'UI Test Brief' },
    repo: { name: 'demo', app_url: 'http://127.0.0.1:5173' },
  };
}

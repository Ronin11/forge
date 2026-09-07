// Browser tests for the workflow graph editor and run view (graph.js). The
// daemon and seed are global-setup's; the wfg-lint/wfg-fix directive files
// are pre-seeded into the daemon's library by global-setup (stored routines
// are target-only, so workflow nodes reference directives), and this file
// creates its workflow over the API. Editor semantics are driven through the
// window.ForgeGraph hooks where precision matters, with one real mouse drag
// as the interaction smoke (the queue-reorder test proves raw drags work in
// this harness).
const { test, expect } = require('@playwright/test');
const fs = require('fs');
const path = require('path');

const HOME = path.join(__dirname, '..', '.tmp-home');
const BASE = 'http://127.0.0.1:7346';
const WORKER_ID = '0123456789abcdef0123456789abcdef';

const token = () => fs.readFileSync(path.join(HOME, 'token'), 'utf8').trim();

async function api(method, p, body, useToken, expectStatus) {
  const headers = { 'Content-Type': 'application/json' };
  if (useToken) headers.Authorization = `Bearer ${token()}`;
  const res = await fetch(BASE + p, { method, headers, body: body === undefined ? undefined : JSON.stringify(body) });
  const text = await res.text();
  if (expectStatus && res.status !== expectStatus) throw new Error(`${method} ${p} = ${res.status} (want ${expectStatus}): ${text}`);
  return text ? JSON.parse(text) : null;
}

const GRAPH = {
  nodes: [
    { id: 'first', type: 'directive', config: { directive: 'wfg-lint', repositories: ['demo'] }, position: { x: 40, y: 40 } },
    { id: 'shape', type: 'script', config: { source: "function main(input) { return {kind: input.steps.first.status} }" }, position: { x: 300, y: 40 } },
    { id: 'route', type: 'switch', config: { expression: 'input.steps.shape.output.kind' }, position: { x: 560, y: 40 } },
    { id: 'good', type: 'directive', config: { directive: 'wfg-fix', repositories: ['demo'] }, position: { x: 820, y: 0 } },
    { id: 'bad', type: 'directive', config: { directive: 'wfg-fix', repositories: ['demo'] }, position: { x: 820, y: 120 } },
  ],
  edges: [
    { from: 'first', to: 'shape', when: 'always' },
    { from: 'shape', to: 'route' },
    { from: 'route', to: 'good', when: 'case', case: 'succeeded' },
    { from: 'route', to: 'bad', default: true },
  ],
};

// ensureWorkflow tolerates reruns: a 409 on create means the earlier run made it.
async function ensureWorkflow() {
  const res = await api('POST', '/api/v1/workflows', { name: 'graphy', graph: GRAPH });
  if (res && res.error && !/exists/.test(res.error)) throw new Error(res.error);
}

test.beforeAll(async () => {
  await ensureWorkflow();
});

test.describe('workflow graph editor', () => {
  test('renders the saved graph with typed nodes, ports, and edge kinds', async ({ page }) => {
    await page.goto('/workflows/graphy/edit');
    await expect(page.locator('.gv-node')).toHaveCount(5);
    await expect(page.locator('.gv-node.gv-t-switch')).toHaveCount(1);
    await expect(page.locator('.gv-edge.gv-e-case')).toHaveCount(2); // case + default
    await expect(page.locator('.gv-edge.gv-e-always')).toHaveCount(1);
    const model = await page.evaluate(() => window.ForgeGraph.getModel());
    expect(model.nodes.map((n) => n.id).sort()).toEqual(['bad', 'first', 'good', 'route', 'shape']);
  });

  test('clicking a node opens its config panel; the workflows list links here', async ({ page }) => {
    await page.goto('/workflows');
    // The estimate cell resolves for every row: a dollar figure once history
    // exists, "no history yet" before, "—" for routine-less graphs.
    const est = page.locator('[data-wf-est]').first();
    await expect(est).not.toHaveText('…');
    await expect(est).toHaveText(/\$|no history yet|—/);
    await page.locator('a[href="/workflows/graphy/edit"]').click();
    await expect(page.locator('.gv-node')).toHaveCount(5);
    await page.evaluate(() => window.ForgeGraph.selectNode('shape'));
    await expect(page.locator('[data-gv-panel] h3')).toHaveText(/JavaScript node/);
    await expect(page.locator('[data-gv-panel] textarea')).toHaveValue(/function main/);
  });

  test('a real mouse drag moves a node and persists its position', async ({ page }) => {
    await page.goto('/workflows/graphy/edit');
    await expect(page.locator('.gv-node')).toHaveCount(5);
    const before = await page.evaluate(() => window.ForgeGraph.getModel().nodes.find((n) => n.id === 'first').position);
    const box = await page.locator('.gv-node[data-node="first"] rect.gv-body').boundingBox();
    await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
    await page.mouse.down();
    await page.mouse.move(box.x + box.width / 2 + 60, box.y + box.height / 2 + 80, { steps: 8 });
    await page.mouse.up();
    const after = await page.evaluate(() => window.ForgeGraph.getModel().nodes.find((n) => n.id === 'first').position);
    expect(Math.abs(after.x - before.x)).toBeGreaterThan(20);
    expect(Math.abs(after.y - before.y)).toBeGreaterThan(20);
    // The positions-only PATCH lands without a generation bump.
    await page.waitForTimeout(900);
    const saved = await api('GET', '/api/v1/workflows/graphy');
    const savedPos = saved.graph.nodes.find((n) => n.id === 'first').position;
    expect(savedPos.x).toBe(after.x);
    expect(saved.generation).toBe(1);
  });

  test('lint blocks saving an uncapped loop edge', async ({ page }) => {
    await page.goto('/workflows/graphy/edit');
    await expect(page.locator('.gv-node')).toHaveCount(5);
    await page.evaluate(() => {
      const g = window.ForgeGraph.getModel();
      g.edges.push({ from: 'good', to: 'first', loop: true }); // no max_iterations
      window.ForgeGraph.setModel(g);
    });
    await expect(page.locator('[data-gv-lint] li.gv-err', { hasText: 'max_iterations' })).toBeVisible();
    await page.locator('[data-gv-save]').click();
    await expect(page.locator('[data-gv-error]')).toBeVisible();
    await expect(page.locator('[data-gv-error]')).toContainText('max_iterations');
  });

  test('the raw JSON escape hatch round-trips the model', async ({ page }) => {
    await page.goto('/workflows/graphy/edit');
    await expect(page.locator('.gv-node')).toHaveCount(5);
    await page.locator('.gv-raw summary').click();
    const raw = await page.locator('[data-gv-raw]').inputValue();
    const parsed = JSON.parse(raw);
    expect(parsed.nodes).toHaveLength(5);
    parsed.nodes.push({ id: 'extra', type: 'join', config: { mode: 'any' }, position: { x: 40, y: 300 } });
    parsed.edges.push({ from: 'good', to: 'extra' }, { from: 'bad', to: 'extra' });
    await page.locator('[data-gv-raw]').fill(JSON.stringify(parsed));
    await page.locator('[data-gv-raw-apply]').click();
    await expect(page.locator('.gv-node')).toHaveCount(6);
  });

  test('a new workflow gets palette nodes via click-then-canvas and saves', async ({ page }) => {
    await page.goto('/workflows/new');
    await expect(page.locator('[data-graph-editor]')).toBeVisible();
    // The palette authors directive nodes: content lives in the library.
    await page.locator('[data-gv-add="directive"]').click();
    const stage = await page.locator('[data-gv-stage]').boundingBox();
    await page.mouse.click(stage.x + 200, stage.y + 120);
    await expect(page.locator('.gv-node')).toHaveCount(1);
    // Name the node's directive and the workflow through the panels, then save.
    await page.locator('[data-gv-panel] input[list="gv-directive-names"]').fill('triage-repo');
    await page.keyboard.press('Escape'); // back to the workflow panel
    await page.locator('[data-gv-panel] input').first().fill('penciled');
    await page.locator('[data-gv-save]').click();
    await page.waitForURL('**/workflows');
    const saved = await api('GET', '/api/v1/workflows/penciled');
    expect(saved.graph.nodes).toHaveLength(1);
    expect(saved.graph.nodes[0].type).toBe('directive');
    await api('DELETE', '/api/v1/workflows/penciled', undefined, false, 204);
  });
});

test.describe('workflow run view', () => {
  test('a run materializes stepwise, routes the switch, and the view shows it', async ({ page }) => {
    const run = await api('POST', '/api/v1/workflows/graphy/run', {}, false, 201);

    // Directive-node works are created at the engine's default priority;
    // outrank the seed's pending queue fixtures so the claim below can only
    // pick this run's root.
    const created = await api('GET', `/api/v1/workflow-runs/${run.run_id}`);
    const rootWork = created.nodes.find((n) => n.node_id === 'first').work_id;
    await api('PATCH', `/api/v1/work/${rootWork}`, { priority: 500 }, false, 200);

    // The repo page test upstream pauses demo and leaves it paused; resume it
    // so the claim is admissible.
    await api('POST', '/api/v1/repositories/demo/resume', {}, false);
    // The root claimed and completed through the worker protocol, as seed.mjs
    // does; the completion advances the run synchronously. Registration is
    // idempotent and refreshes the worker's last-seen.
    await api('POST', '/api/v1/worker/register', {
      worker_id: WORKER_ID, name: 'laptop', version: 'ui-test', max_concurrent: 2, active: 0,
      executors: ['claude-code'], capabilities: { sandbox: 'ready' },
      repositories: [{ name: 'demo', path: '/tmp/forge-ui-demo', origin_identity: 'github.com/x/demo', project: 'default' }],
      retained: [],
    }, true, 200);
    const lease = 'lease-wfg-1';
    const claim = await api('POST', '/api/v1/worker/claim', { worker_id: WORKER_ID, claim_request_id: 'wfg-1', lease_token: lease }, true, 200);
    const hb = (body) => api('POST', `/api/v1/attempts/${claim.attempt_id}/heartbeat`, { lease_token: lease, ...body }, true, 200);
    await hb({ phase: 'preparing', state: 'preparing' });
    await hb({ phase: 'running', state: 'running', pid: 4242, pid_start: 7, session_id: 'sess-wfg', worktree: '/tmp/forge-ui-wt', branch: 'forge/wfg-1', base_branch: 'main', base_commit: '1111111abcdef22' });
    await api('POST', `/api/v1/attempts/${claim.attempt_id}/complete`, {
      lease_token: lease, state: 'succeeded', exit_code: 0, num_turns: 2, launches: 1,
      usage: { input_tokens: 10, output_tokens: 10 },
      git: { dirty: false, commits: 1, files_changed: 1, insertions: 1, deletions: 0, pushed: false, head: 'abc1234def5678' },
      verification: { level: 1, passed: true }, cleanup: { outcome: 'removed', reason: 'clean' },
      started_at: new Date(Date.now() - 30_000).toISOString(), finished_at: new Date().toISOString(), session_id: 'sess-wfg',
    }, true, 200);

    const detail = await api('GET', `/api/v1/workflow-runs/${run.run_id}`);
    const by = {};
    for (const n of detail.nodes) by[n.node_id] = n;
    expect(by.first.status).toBe('succeeded');
    expect(by.shape.status).toBe('succeeded'); // the script ran in the daemon
    expect(by.route.status).toBe('succeeded');
    expect(by.good.status).toBe('running'); // case "succeeded" taken
    expect(by.bad.status).toBe('skipped');

    await page.goto(`/workflows/graphy/runs/${run.run_id}`);
    await expect(page.locator('[data-gv-run-state]')).toHaveText('running');
    await expect(page.locator('.gv-node.gv-st-succeeded')).toHaveCount(3);
    await expect(page.locator('.gv-node.gv-st-running')).toHaveCount(1);
    await expect(page.locator('.gv-node.gv-st-skipped')).toHaveCount(1);
    // Taken edges render at full opacity (class gv-taken).
    expect(await page.locator('.gv-edge.gv-taken').count()).toBeGreaterThanOrEqual(3);
    // The node aside shows the instance and its task link.
    await page.locator('.gv-node[data-node="first"]').click();
    await expect(page.locator('[data-gv-panel] .state-succeeded').first()).toBeVisible();
    await expect(page.locator('[data-gv-panel] a[href^="/tasks/"]')).toBeVisible();

    // The runs listing shows the run with node chips.
    await page.goto('/workflows/graphy/runs');
    await expect(page.locator('table.list tbody tr').first()).toContainText('running');
  });
});

test.describe('workflow metadata', () => {
  test('description and tool flag edit and persist through the settings panel', async ({ page }) => {
    await page.goto('/workflows/graphy/edit');
    await expect(page.locator('.gv-node')).toHaveCount(5);
    const panel = page.locator('[data-gv-panel]');
    await panel.locator('input[placeholder="what this workflow does"]').fill('routes by lint outcome');
    await panel.locator('label.check', { hasText: 'Callable as a tool' }).locator('input').check();
    await page.locator('[data-gv-save]').click();
    await page.waitForURL('**/workflows');
    const saved = await api('GET', '/api/v1/workflows/graphy');
    expect(saved.description).toBe('routes by lint outcome');
    expect(saved.tool).toBe(true);
    // The list shows both — one card per workflow.
    const card = page.locator('.wf-card', { has: page.locator('.wf-title b', { hasText: 'graphy' }) }).first();
    await expect(card.locator('.wf-notes')).toContainText('routes by lint outcome');
    await expect(card.locator('.wf-title .chip', { hasText: 'tool' })).toBeVisible();
  });
});

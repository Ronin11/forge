// Browser tests for the Forge operator UI. The daemon under test was started by
// global-setup.js against a temp FORGE_HOME and seeded over the HTTP API
// (seed.mjs); everything here is offline and spends no budget (STYLE.md §11).
// Single worker, single file: tests run top to bottom, and the two mutating tests
// (answering the question, reordering the queue) are written to stay green on a
// retry after the mutation landed.
const { test, expect } = require('@playwright/test');
const fs = require('fs');
const path = require('path');

const HOME = path.join(__dirname, '..', '.tmp-home');
const seed = () => JSON.parse(fs.readFileSync(path.join(HOME, 'seed.json'), 'utf8'));

test.describe('empty states', () => {
  test('every page rendered 200 with its empty-state copy before seeding', () => {
    // Captured by global-setup before any data existed: a restart per test would
    // be slow, so the pass is a recorded fetch of every page (status + marker
    // string), asserted here.
    const results = JSON.parse(fs.readFileSync(path.join(HOME, 'empty-states.json'), 'utf8'));
    expect(results.length).toBeGreaterThan(0);
    for (const r of results) {
      expect(r.status, `${r.path} status`).toBe(200);
      expect(r.found, `${r.path} should contain ${JSON.stringify(r.want)}`).toBe(true);
    }
  });
});

test.describe('dashboard', () => {
  test('nav, connected worker, recent tasks with state chips', async ({ page }) => {
    const s = seed();
    await page.goto('/');
    const nav = page.locator('nav.top');
    for (const name of ['Dashboard', 'Tasks', 'Queue', 'Human Queue', 'Routines', 'Stats', 'Settings']) {
      await expect(nav.getByText(name, { exact: true })).toBeVisible();
    }
    const workers = page.locator('.card', { hasText: 'Workers' });
    const laptop = workers.locator('li', { hasText: 'laptop' });
    await expect(laptop).toBeVisible();
    await expect(laptop.locator('.dot.ok')).toBeVisible(); // connected
    // The recent-tasks table (scoped: failed/waiting rows also appear under
    // "Needs attention") lists the seeded tasks with the right chip classes.
    const recent = page.locator('h2:has-text("Recent tasks") + table');
    await expect(recent.locator(`tr[data-href="/tasks/${s.succeeded.work_id}"]`)).toContainText('Inventory the demo repo');
    await expect(recent.locator(`tr[data-href="/tasks/${s.succeeded.work_id}"] .state.state-succeeded`).first()).toBeVisible();
    await expect(recent.locator(`tr[data-href="/tasks/${s.failed.work_id}"] .state.state-failed`).first()).toBeVisible();
    for (const id of [s.waiting.work_id, s.queue.a, s.queue.b, s.queue.c]) {
      await expect(recent.locator(`tr[data-href="/tasks/${id}"]`)).toBeVisible();
    }
  });
});

test.describe('dashboard timeline', () => {
  test('renders bars, a hover tooltip, the repo toggle, and a bar click navigates', async ({ page }) => {
    const s = seed();
    await page.goto('/');
    // The timeline fetches /api/v1/timeline and draws a bar per seeded attempt.
    const live = page.locator('[data-timeline-live]');
    await expect(live.locator('.tl-bar').first()).toBeVisible();

    // Hover the succeeded task's bar → the reused tooltip shows the task title.
    const lane = page.locator('.tl-lane', { hasText: 'Inventory the demo repo' });
    await lane.locator('.tl-bar').first().hover();
    const tip = page.locator('.tl-tip');
    await expect(tip).toBeVisible();
    await expect(tip).toContainText('Inventory the demo repo');

    // "By repo" regroups: lane labels become the repository name; bars stay.
    await page.click('[data-tl-view="repo"]');
    await expect(page.locator('.tl-label', { hasText: 'demo' }).first()).toBeVisible();
    await expect(live.locator('.tl-bar').first()).toBeVisible();

    // Back to "By task"; clicking that task's bar opens its detail page.
    await page.click('[data-tl-view="task"]');
    await lane.locator('.tl-bar').first().click();
    await expect(page).toHaveURL(`/tasks/${s.succeeded.work_id}`);
  });
});

test.describe('tasks', () => {
  test('lists every seeded task and a row click navigates to the detail', async ({ page }) => {
    const s = seed();
    await page.goto('/tasks?scope=all');
    const ids = [s.succeeded.work_id, s.failed.work_id, s.waiting.work_id, s.queue.a, s.queue.b, s.queue.c];
    for (const id of ids) {
      await expect(page.locator(`tr[data-href="/tasks/${id}"]`)).toBeVisible();
    }
    // Click a non-link cell: app.js's data-href handler must navigate.
    await page.locator(`tr[data-href="/tasks/${s.succeeded.work_id}"] td`).nth(4).click();
    await expect(page).toHaveURL(`/tasks/${s.succeeded.work_id}`);
  });
});

test.describe('search bar', () => {
  test('defaults to open tasks; scope tabs and DSL reveal closed; New task opens', async ({ page }) => {
    const s = seed();
    await page.goto('/tasks');
    const row = (id) => page.locator(`tr[data-href="/tasks/${id}"]`);

    // Open by default: the waiting/queued tasks show, the succeeded/failed
    // (terminal) ones do not.
    await expect(row(s.waiting.work_id)).toBeVisible();
    await expect(row(s.queue.a)).toBeVisible();
    await expect(row(s.succeeded.work_id)).toHaveCount(0);
    await expect(row(s.failed.work_id)).toHaveCount(0);
    await expect(page.locator('[data-task-scope] a.on')).toHaveText('Open');

    // The All tab loads the closed ones too.
    await page.locator('[data-task-scope] a', { hasText: 'All' }).click();
    await expect(row(s.succeeded.work_id)).toBeVisible();
    await expect(row(s.failed.work_id)).toBeVisible();

    // DSL: a terminal state token typed on the open view widens to all so the
    // closed rows are actually fetched (the page navigates to scope=all).
    await page.goto('/tasks');
    await expect(row(s.succeeded.work_id)).toHaveCount(0);
    const q = page.locator('.searchbar input[name=q]');
    await q.fill('state:succeeded');
    await q.press('Enter');
    await expect(page).toHaveURL(/scope=all/);
    await expect(row(s.succeeded.work_id)).toBeVisible();

    // New task opens the dialog for arbitrary work.
    await page.goto('/tasks');
    await page.locator('[data-task-new]').click();
    await expect(page.locator('[data-task-dialog] textarea[name=prompt]')).toBeVisible();
  });

  test('DSL filters rows, negation, repo qualifier, count, and ?q= sync', async ({ page }) => {
    const s = seed();
    const q = page.locator('.searchbar input[name=q]');
    const row = (id) => page.locator(`tr[data-href="/tasks/${id}"]`);
    await page.goto('/tasks?scope=all');

    // state qualifier: only the succeeded task remains, count reflects it, and
    // the query lands in the URL so the filter is shareable.
    await q.fill('state:succeeded');
    await expect(row(s.succeeded.work_id)).toBeVisible();
    await expect(row(s.failed.work_id)).toBeHidden();
    await expect(page.locator('[data-sb-count]')).toHaveText('2/10');
    await expect(page).toHaveURL(/q=state%3Asucceeded/);

    // negation flips it.
    await q.fill('-state:succeeded');
    await expect(row(s.succeeded.work_id)).toBeHidden();
    await expect(row(s.failed.work_id)).toBeVisible();

    // free text matches the row's own text.
    await q.fill('parser');
    await expect(row(s.queue.a)).toBeVisible();
    await expect(row(s.queue.b)).toBeHidden();

    // repo: matches the target chips; a repo nothing lives in hides everything.
    await q.fill('repo:demo');
    await expect(row(s.succeeded.work_id)).toBeVisible();
    await q.fill('repo:nope');
    await expect(row(s.succeeded.work_id)).toBeHidden();
    await expect(page.locator('[data-sb-count]')).toHaveText('0/10');

    // clearing restores every row and drops ?q=.
    await q.fill('');
    await expect(row(s.succeeded.work_id)).toBeVisible();
    await expect(page.locator('[data-sb-count]')).toBeHidden();
  });

  test('a ?q= deep link applies on load', async ({ page }) => {
    const s = seed();
    await page.goto('/tasks?q=state%3Afailed');
    await expect(page.locator(`tr[data-href="/tasks/${s.failed.work_id}"]`)).toBeVisible();
    await expect(page.locator(`tr[data-href="/tasks/${s.succeeded.work_id}"]`)).toBeHidden();
  });

  test('suggestions offer keys then page-scraped values; "/" focuses the bar', async ({ page }) => {
    await page.goto('/tasks?scope=all');
    // "/" focuses the input from anywhere outside a field.
    await page.locator('body').press('/');
    const q = page.locator('.searchbar input[name=q]');
    await expect(q).toBeFocused();
    // Focus offers the qualifier keys; choosing repo: then offers the repos
    // actually on the page.
    await page.locator('.sb-opt', { hasText: 'repo:' }).first().click();
    await expect(q).toHaveValue('repo:');
    const opt = page.locator('.sb-opt', { hasText: 'repo:demo' });
    await expect(opt).toBeVisible();
    await opt.click();
    // Accepting a value commits it as a chip and clears the input.
    await expect(page.locator('.sb-chip', { hasText: 'repo:demo' })).toBeVisible();
    await expect(q).toHaveValue('');
  });

  test('knowledge search accepts tag: in the server-side query', async ({ page }) => {
    const s = seed();
    await page.goto('/kb');
    const q = page.locator('.searchbar input[name=q]');
    await q.fill('tag:brief');
    await q.press('Enter');
    await expect(page).toHaveURL(/q=tag%3Abrief/);
    await expect(page.locator('tr', { hasText: s.kb.title })).toBeVisible();
  });
});

test.describe('task detail (succeeded)', () => {
  test('attempt line, git line, result, facts, and a rendered timeline', async ({ page }) => {
    const s = seed();
    await page.goto(`/tasks/${s.succeeded.work_id}`);
    await expect(page.locator('h1 .state.state-succeeded')).toBeVisible();
    const kv = page.locator('dl.kv');
    await expect(kv).toContainText(s.succeeded.attempt_id);
    await expect(kv).toContainText('launches 1');
    await expect(kv).toContainText('haiku');
    await expect(kv).toContainText('forge/ui-1'); // git line: branch
    await expect(kv).toContainText('commits 1');
    await expect(kv).toContainText('tokens in 1200 out 340');
    await expect(kv).toContainText('$0.0123');
    await expect(page.locator('pre.result')).toContainText('Inventory complete: 42 files tracked.');
    // Facts: the phase durations posted as span_end events.
    const facts = page.locator('table.facts');
    await expect(facts).toContainText('1ms'); // fetch: 1200µs
    await expect(facts).toContainText('5.0s'); // agent: 5s
    await expect(facts).toContainText('800ms'); // verify
    // Timeline bars: app.js computes each bar's width from data-dur/data-elapsed.
    const bars = page.locator('.timeline .span .bar');
    expect(await bars.count()).toBeGreaterThanOrEqual(3);
    for (const bar of await bars.all()) {
      expect(await bar.evaluate((el) => el.style.width)).not.toBe('');
    }
  });
});

test.describe('task detail (waiting)', () => {
  test('a waiting task exposes the answer form on its own page', async ({ page }) => {
    const s = seed();
    await page.goto('/tasks/' + s.waiting.work_id);
    // The open question renders an inline answer form (same data-answer-form the
    // human queue uses), so a question can be answered from the task page too.
    const form = page.locator('form[data-answer-form]');
    await expect(form).toBeVisible();
    await expect(form.locator('input[name=answer]')).toBeVisible();
    await expect(form.locator('button[type=submit]')).toHaveText(/Answer/);
  });
});

test.describe('provenance', () => {
  test('task strip breadcrumb, back/forward links, and the Work view rollups', async ({ page }) => {
    const s = seed();
    const p = s.provenance;

    // A middle child's task page: the strip carries a breadcrumb up to the
    // root, a forward link to its own follow-up, and a View-work link.
    await page.goto(`/tasks/${p.childA}`);
    const strip = page.locator('nav.prov');
    await expect(strip).toBeVisible();
    // Breadcrumb links to the root.
    await expect(strip.locator(`.prov-crumb a[href="/tasks/${p.root}"]`)).toBeVisible();
    // Forward: the verify follow-up under child A.
    await expect(strip.locator(`.prov-fwd a[href="/tasks/${p.verify}"]`)).toBeVisible();
    // View work → the root's tree.
    const viewWork = strip.locator(`a.prov-view[href="/work/${p.root}"]`);
    await expect(viewWork).toBeVisible();

    // The breadcrumb navigates to the root task.
    await strip.locator(`.prov-crumb a[href="/tasks/${p.root}"]`).click();
    await expect(page).toHaveURL(`/tasks/${p.root}`);

    // From the root, View work opens the tree.
    await page.locator(`a.prov-view[href="/work/${p.root}"]`).click();
    await expect(page).toHaveURL(`/work/${p.root}`);

    // The Work view lists every node in the tree and the root's rollups.
    await expect(page.locator('h1')).toContainText(p.root.slice(0, 8));
    const tree = page.locator('section.wtree');
    for (const id of [p.root, p.childA, p.childB, p.verify]) {
      await expect(tree.locator(`a.wid[href="/tasks/${id}"]`)).toBeVisible();
    }
    // Rollup header: the root's one attempt and its cost.
    const roll = page.locator('.wrollup');
    await expect(roll).toContainText('attempts');
    await expect(roll).toContainText('$0.0123');
    // A node link navigates to its task detail.
    await tree.locator(`a.wid[href="/tasks/${p.childB}"]`).click();
    await expect(page).toHaveURL(`/tasks/${p.childB}`);
  });

  test('non-root tasks carry a "part of" chip in the list', async ({ page }) => {
    const s = seed();
    await page.goto('/tasks?scope=all');
    const row = page.locator(`tr[data-href="/tasks/${s.provenance.childA}"]`);
    await expect(row.locator(`a.chip.part[href="/work/${s.provenance.root}"]`)).toBeVisible();
  });
});

test.describe('human queue', () => {
  test('question is shown, answerable, and the target goes pending', async ({ page }) => {
    const s = seed();
    await page.goto(`/tasks/${s.waiting.work_id}`);
    const question = page.locator('.card.question');
    await expect(question).toContainText('Which branch should I target?');
    await expect(question).toContainText('main, dev');

    await page.goto('/attention');
    const card = page.locator(`[data-question="${s.waiting.question_id}"]`);
    if ((await card.count()) > 0) {
      // First run: the question is open on the human queue; answer it in place.
      await expect(card).toContainText('Which branch should I target?');
      await expect(card).toContainText('options: main, dev');
      // The context actions render: the doc link routes into the UI, and the
      // registered RPC action button fires its method and confirms in place.
      await expect(card.locator('a[href="/kb/ui-test-brief"]')).toBeVisible();
      const toast = card.locator('button[data-rpc="notify.test"]');
      await toast.click();
      await expect(toast).toContainText('✓');
      // A doc proposal's target links straight to the note it covers.
      const docCard = page.locator(`[data-proposal="${s.proposals.decide}"]`);
      if ((await docCard.count()) > 0) {
        await expect(docCard.locator('a[href="/kb/ui-test-brief"]')).toBeVisible();
      }
      await card.locator('input[name=answer]').fill('main');
      await card.locator('button[type=submit]').click();
      // app.js reloads the page after the POST; the answered question leaves
      // it (the seeded proposals stay on the page, so it is not empty).
      await expect(card).toHaveCount(0, { timeout: 10_000 });
    }
    // Answered (idempotent for a retry): the answer shows and the target went
    // back to pending — the state chip changed away from waiting_human.
    await page.goto(`/tasks/${s.waiting.work_id}`);
    await expect(page.locator('.card.question')).toContainText('Answered by human: main');
    await expect(page.locator('.state.state-pending').first()).toBeVisible();
    await expect(page.locator('.state.state-waiting-human')).toHaveCount(0);
  });
});

test.describe('queue', () => {
  test('order, refused drag above a dependency, accepted drag, screenshot', async ({ page }) => {
    const s = seed();
    await page.goto('/queue');
    for (const id of [s.queue.a, s.queue.b, s.queue.c]) {
      await expect(page.locator(`tr[data-id="${id}"]`)).toBeVisible();
    }
    // B is blocked by A and says so.
    const rowB = page.locator(`tr[data-id="${s.queue.b}"]`);
    await expect(rowB.locator('.state.state-blocked')).toBeVisible();
    await expect(rowB).toContainText('waiting_on_dependencies');
    // A sits above B on the first run (same priority, created earlier).
    const order = () => page.$$eval('[data-queue] tbody tr[data-id]', (rows) => rows.map((r) => r.dataset.id));

    // Illegal drag: B above A (B is blocked by A) — the daemon answers 409 and
    // the page shows the refusal, then snaps back by reloading.
    await page.dragAndDrop(`tr[data-id="${s.queue.b}"]`, `tr[data-id="${s.queue.a}"]`, {
      targetPosition: { x: 30, y: 4 },
    });
    const err = page.locator('#queue-error');
    await expect(err).toBeVisible();
    await expect(err).toContainText('Refused');
    await page.waitForTimeout(1500); // app.js reloads 1.2s after a refusal
    await page.goto('/queue');
    let ids = await order();
    expect(ids.indexOf(s.queue.a)).toBeLessThan(ids.indexOf(s.queue.b));

    // Legal drag: C immediately above B (no dependency between them). The PATCH
    // lands move_before=B and the page reloads with the new order.
    await page.dragAndDrop(`tr[data-id="${s.queue.c}"]`, `tr[data-id="${s.queue.b}"]`, {
      targetPosition: { x: 30, y: 4 },
    });
    await expect(page.locator('#queue-error')).toBeHidden();
    await page.waitForTimeout(1000);
    await page.goto('/queue'); // assert the persisted order, not the drag preview
    ids = await order();
    expect(ids.indexOf(s.queue.c)).toBeLessThan(ids.indexOf(s.queue.b));

    // Stored screenshot of the queue page (smoke 22's "L2 screenshots stored").
    await page.screenshot({ path: path.join(__dirname, '..', 'test-results', 'queue-page.png'), fullPage: true });
  });
});

test.describe('stats', () => {
  test('routine table renders verified % from the seeded facts; window links work', async ({ page }) => {
    await page.goto('/stats');
    await expect(page.locator('h1')).toContainText('window 7d');
    // The terminal ad-hoc seeds: the two originals (1 succeeded, 1 failed) plus
    // the provenance root's succeeded attempt → 3 runs, 2 verified → 67%.
    const section = page.locator('section.card', { hasText: 'ad-hoc' });
    await expect(section).toBeVisible();
    await expect(section.locator('h2')).toContainText('3 run(s)');
    const verifiedCell = section.locator('tbody tr').first().locator('td').nth(2);
    await expect(verifiedCell).toHaveText(/^\d+%$/);
    await expect(verifiedCell).toHaveText('67%');
    await expect(section).toContainText('$'); // cost columns rendered
    // Window links.
    await page.click('a[href="/stats?since=1d"]');
    await expect(page).toHaveURL(/since=1d/);
    await expect(page.locator('h1')).toContainText('window 1d');
    await expect(page.locator('section.card', { hasText: 'ad-hoc' })).toBeVisible(); // still inside 1d
  });

  test('capability matrix renders per-model verified success from the seeded facts (M10)', async ({ page }) => {
    await page.goto('/stats');
    // The seeded ad-hoc attempts run on model haiku (class small, runner claude
    // from the embedded model table); the capability matrix groups by model.
    const matrix = page.locator('#capability-matrix');
    await expect(matrix).toBeVisible();
    const row = matrix.locator('tbody tr', { hasText: 'haiku' });
    await expect(row).toBeVisible();
    // Columns: Model, Class, Runner, Runs, Verified %, Samples, ...
    await expect(row.locator('td').nth(1)).toHaveText('small');
    await expect(row.locator('td').nth(2)).toHaveText('claude');
    await expect(row.locator('td').nth(4)).toHaveText(/^\d+%$/);
    // Per-runner utilization table lists the claude runner.
    const util = page.locator('#runner-utilization');
    await expect(util).toBeVisible();
    await expect(util.locator('tbody tr', { hasText: 'claude' })).toBeVisible();
  });
});

test.describe('system', () => {
  test('worker and repository rows', async ({ page }) => {
    await page.goto('/system');
    const workerRow = page.locator('tr', { hasText: 'laptop' });
    await expect(workerRow).toBeVisible();
    await expect(workerRow).toContainText('claude-code');
    await expect(workerRow).toContainText('sandbox=ready');
    const repoRow = page.locator('tr', { hasText: 'github.com/x/demo' });
    await expect(repoRow).toBeVisible();
    await expect(repoRow).toContainText('demo');
    await expect(repoRow).toContainText('default');
  });
});

test.describe('repositories', () => {
  test('system page shows a repo state chip and a link to the repo page', async ({ page }) => {
    await page.goto('/system');
    const row = page.locator('tr', { hasText: 'github.com/x/demo' });
    await expect(row).toBeVisible();
    await expect(row.locator('a[href="/repos/demo"]')).toBeVisible();
    await expect(row.locator('.state').first()).toBeVisible();
  });

  test('repo page shows the app lifecycle card', async ({ page }) => {
    seed();
    await page.goto('/repos/demo');
    const card = page.locator('[data-app-card]');
    await expect(card).toBeVisible();
    await expect(card.locator('h2')).toContainText('App');
    // The demo repo declares no [run] section, so the card shows the hint and
    // the controls stay hidden (the status poll resolves configured=false).
    await expect(card.locator('[data-app-unconfigured]')).toBeVisible({ timeout: 5000 });
  });

  test('repo page renders name, state, app link, and pause toggles the state', async ({ page }) => {
    await page.goto('/repos/demo');
    await expect(page.locator('h1')).toContainText('demo');
    await expect(page.locator('h1 .state').first()).toBeVisible();
    // The seeded app_url renders an "Open app" link.
    await expect(page.locator('a', { hasText: 'Open app' })).toBeVisible();

    // Pause (idempotent on a retry: if already paused, the button is Resume).
    const pauseBtn = page.locator('button[data-repo-pause="demo"]');
    if ((await pauseBtn.count()) > 0) {
      await pauseBtn.click(); // app.js POSTs and reloads on success
    }
    await expect(page.locator('h1 .state.state-paused')).toBeVisible({ timeout: 10_000 });
    await expect(page.locator('button[data-repo-resume="demo"]')).toBeVisible();
  });
});

test.describe('repos page', () => {
  test('lists repos, opens the add dialog, and shows archive controls', async ({ page }) => {
    seed();
    await page.goto('/repos');
    await expect(page.locator('h1')).toHaveText('Repos');
    // The seeded demo repo is listed with an Archive control.
    const row = page.locator('tr', { hasText: 'demo' }).first();
    await expect(row).toBeVisible();
    await expect(row.locator('[data-repo-archive="demo"]')).toBeVisible();
    // The + button opens the add dialog with url and path fields.
    await page.locator('[data-repo-add]').click();
    await expect(page.locator('[data-repo-dialog] input[name=url]')).toBeVisible();
    await expect(page.locator('[data-repo-dialog] input[name=path]')).toBeVisible();
  });
});

test.describe('settings', () => {
  test('General fills the health panel and applies a log level; Plugins page has the install form', async ({ page }) => {
    await page.goto('/settings');
    // Sub-nav tabs.
    for (const t of ['General', 'System', 'Plugins']) {
      await expect(page.locator('[data-settings-nav]').getByText(t, { exact: true })).toBeVisible();
    }
    // The daemon health panel fills from /api/v1/health (version stops being the … placeholder).
    const version = page.locator('[data-h="version"]');
    await expect(version).not.toHaveText('…', { timeout: 10_000 });
    // Apply a log level and see it round-trip into the input.
    const input = page.locator('[data-loglevel-input]');
    await input.fill('debug');
    await page.locator('[data-loglevel-form] button[type=submit]').click();
    await expect(input).toHaveValue('debug', { timeout: 10_000 });

    // Plugins tab: the install form is present.
    await page.locator('[data-settings-nav]').getByText('Plugins', { exact: true }).click();
    await expect(page.locator('[data-plugin-install-form] input[name=name]')).toBeVisible();
  });
});

test.describe('proposals', () => {
  test('rows render and a decision moves the proposal to a terminal state', async ({ page }) => {
    const s = seed();
    await page.goto('/proposals');
    // The kept proposal renders with its status chip, kind, and rationale.
    const keep = page.locator(`tr[data-proposal="${s.proposals.keep}"]`);
    await expect(keep).toContainText('process');
    await expect(keep).toContainText('routine:ad-hoc');
    await expect(keep).toContainText('Lower the ad-hoc timeout');
    await expect(keep.locator('.state.state-proposed')).toBeVisible();
    // Decide the second one. Reject is terminal regardless of the apply
    // engine, so a retry after the mutation landed stays green: the button is
    // gone and the chip already reads rejected.
    const decide = page.locator(`tr[data-proposal="${s.proposals.decide}"]`);
    const rejectBtn = decide.locator('button[data-proposal-reject]');
    if ((await rejectBtn.count()) > 0) {
      await rejectBtn.click(); // app.js POSTs and reloads on success
    }
    await expect(decide.locator('.state.state-rejected')).toBeVisible({ timeout: 10_000 });
    await expect(decide.locator('button')).toHaveCount(0);
    // The kept proposal reaches the human queue with the CLI hint.
    await page.goto('/attention');
    const card = page.locator(`[data-proposal="${s.proposals.keep}"]`);
    await expect(card).toContainText('Lower the ad-hoc timeout');
    await expect(card).toContainText('forge proposal approve');
  });
});

test.describe('proposal detail', () => {
  test('the id links to a detail page with rationale, decision history, and actions', async ({ page }) => {
    const s = seed();
    await page.goto('/proposals');
    // Click the proposal id link to drill in.
    await page.locator(`tr[data-proposal="${s.proposals.keep}"] a`).first().click();
    await expect(page).toHaveURL(new RegExp('/proposals/' + s.proposals.keep));
    await expect(page.locator('h1')).toContainText('Proposal');
    await expect(page.getByText('Verification plan')).toBeVisible();
    await expect(page.getByRole('heading', { name: 'Decision history' })).toBeVisible();
    // A still-proposed proposal shows the approve action and its created event.
    await expect(page.locator('[data-proposal-approve]')).toBeVisible();
    await expect(page.getByText('proposal.created')).toBeVisible();
  });
});

test.describe('knowledge base', () => {
  test('lists a seeded note, filters by tag, and renders its markdown', async ({ page }) => {
    const s = seed();
    await page.goto('/kb');
    await expect(page.locator('h1')).toContainText('Knowledge');

    // The seeded note is listed with its tag chips.
    const row = page.locator('tr', { hasText: s.kb.title });
    await expect(row).toBeVisible();
    await expect(row.locator('.chip', { hasText: 'ui-test' })).toBeVisible();

    // The tag rail filters to it.
    await page.click('.kb-tags a.chip:has-text("brief")');
    await expect(page).toHaveURL(/tag=brief/);
    await expect(page.locator('tr', { hasText: s.kb.title })).toBeVisible();

    // Open the note; goldmark rendered real structure, not escaped text.
    await page.click(`a[href="/kb/${s.kb.id}"]`);
    await expect(page).toHaveURL(new RegExp(`/kb/${s.kb.id}$`));
    const body = page.locator('.kb-body');
    await expect(body.locator('h2', { hasText: 'Overview' })).toBeVisible();
    await expect(body.locator('ul li').first()).toContainText('first item');
    await expect(body.locator('ul li code').first()).toHaveText('inline code');
    await expect(body.locator('strong', { hasText: 'seeded' })).toBeVisible();
    await expect(body.locator('table td', { hasText: '1' })).toBeVisible();
    // The [[wiki link]] became an internal /kb link.
    await expect(body.locator(`a[href="/kb/${s.kb.id}"]`)).toBeVisible();
  });
});

test.describe('click-to-copy', () => {
  test('a forge command in the human queue copies on click', async ({ page, context }) => {
    await context.grantPermissions(['clipboard-write', 'clipboard-read']);
    await page.goto('/attention');
    // The human queue shows `forge proposal approve <id>` for the still-proposed
    // "keep" proposal — stable regardless of which earlier tests mutated state.
    const cmd = page.locator('code.cmd-copy', { hasText: 'forge proposal approve' }).first();
    await expect(cmd).toBeVisible();
    await expect(cmd).toHaveAttribute('role', 'button');
    const text = (await cmd.textContent()).trim();
    await cmd.click();
    await expect(cmd).toHaveClass(/copied/); // visual feedback
    const clip = await page.evaluate(() => navigator.clipboard.readText());
    expect(clip).toBe(text);
  });
});

test.describe('chat popout', () => {
  // Runs last: sending files a real task, which would shift earlier tests'
  // row counts. The unique prompt suffix dodges the 24h intake dedupe on retry.
  test('routes a question to explore, a change to intake, and files the task', async ({ page }) => {
    await page.goto('/');
    await page.click('[data-chat-toggle]');
    const pop = page.locator('[data-chat]');
    await expect(pop).toBeVisible();
    // The repo picker filled itself from the API.
    await expect(pop.locator('option[value="demo"]')).toHaveCount(1);

    // A question auto-routes to Ask (explore); an imperative flips to Change (intake).
    const text = pop.locator('[data-chat-text]');
    await text.fill('How does queue ordering decide priority?');
    await expect(pop.locator('[data-chat-route="explore"]')).toHaveClass(/on/);
    await text.fill('Add a retry button to the queue page');
    await expect(pop.locator('[data-chat-route="intake"]')).toHaveClass(/on/);
    // A manual override sticks while typing continues.
    await pop.locator('[data-chat-route="explore"]').click();
    await text.fill('Rename the demo readme please');
    await expect(pop.locator('[data-chat-route="explore"]')).toHaveClass(/on/);
    await pop.locator('[data-chat-route="intake"]').click();

    const prompt = `Chat fixture: add a retry button (${Date.now()})`;
    await text.fill(prompt);
    await pop.locator('[data-chat-send]').click();
    const link = pop.locator('[data-chat-status] a');
    await expect(link).toBeVisible();
    await expect(pop.locator('[data-chat-status]')).toContainText('Change filed as task');
    // The link lands on the created task, titled from the prompt.
    await link.click();
    await expect(page).toHaveURL(/\/tasks\/[0-9a-hjkmnp-tv-z]+/i);
    await expect(page.locator('h1')).toContainText('Task');
    await expect(page.locator('p.meta').first()).toContainText('Chat fixture: add a retry button');
  });

  test('escape closes the popout and the toggle reopens it', async ({ page }) => {
    await page.goto('/system'); // present even on the page without a search bar
    await page.click('[data-chat-toggle]');
    await expect(page.locator('[data-chat]')).toBeVisible();
    await page.keyboard.press('Escape');
    await expect(page.locator('[data-chat]')).toBeHidden();
  });
});

test.describe('search chips', () => {
  test('a committed filter becomes a deletable chip and persists across pages', async ({ page }) => {
    const s = seed();
    const q = page.locator('.searchbar input[name=q]');
    await page.goto('/tasks?scope=all');
    // Type a filter and commit it with Enter → it becomes a chip, input clears.
    await q.fill('state:succeeded');
    await q.press('Enter');
    const chip = page.locator('.sb-chip', { hasText: 'state:succeeded' });
    await expect(chip).toBeVisible();
    await expect(q).toHaveValue('');
    await expect(page.locator(`tr[data-href="/tasks/${s.succeeded.work_id}"]`)).toBeVisible();
    await expect(page.locator(`tr[data-href="/tasks/${s.failed.work_id}"]`)).toBeHidden();

    // The filter persists onto another client-mode page.
    await page.goto('/queue');
    await expect(page.locator('.sb-chip', { hasText: 'state:succeeded' })).toBeVisible();

    // Deleting the chip clears the filter.
    await page.goto('/tasks?scope=all');
    await page.locator('.sb-chip', { hasText: 'state:succeeded' }).locator('.sb-chip-x').click();
    await expect(page.locator('.sb-chip')).toHaveCount(0);
    await expect(page.locator(`tr[data-href="/tasks/${s.failed.work_id}"]`)).toBeVisible();
  });
});

test.describe('icons', () => {
  test('favicon, nav icons, and state-pill glyphs render', async ({ page }) => {
    seed();
    await page.goto('/');
    // Favicon linked in the head.
    await expect(page.locator('link[rel="icon"]')).toHaveAttribute('href', '/static/favicon.svg');
    // Every nav link carries an icon.
    const navIcons = page.locator('nav.top a svg.i');
    expect(await navIcons.count()).toBeGreaterThanOrEqual(10);
    // Brand anvil.
    await expect(page.locator('nav.top .brand svg use[href="#i-anvil"]')).toHaveCount(1);
    // A state pill shows its glyph (a closed task has a terminal-state glyph).
    await page.goto('/tasks?scope=all');
    await expect(page.locator('.state svg.i-pill').first()).toBeVisible();
  });
});

test.describe('routine templates', () => {
  test('a role template fills the New-routine form', async ({ page }) => {
    seed();
    await page.goto('/routines');
    await page.locator('[data-routine-new]').click();
    const dialog = page.locator('[data-routine-dialog]');
    await expect(dialog.locator('[data-routine-template]')).toBeVisible();
    // Pick the Programmer template; the form fills from it.
    await dialog.locator('[data-routine-template]').selectOption('programmer');
    await expect(dialog.locator('[name=mode]')).toHaveValue('implement');
    await expect(dialog.locator('[name=model]')).toHaveValue('sonnet');
    await expect(dialog.locator('[name=prompt]')).toHaveValue(/careful programmer/);
    await expect(dialog.locator('[name=integrate]')).toBeChecked();
  });
});

test.describe('responsive', () => {
  const sizes = [
    { name: 'phone', w: 390, h: 844 },
    { name: 'tablet', w: 768, h: 1024 },
  ];
  const pages = ['/', '/tasks?scope=all', '/queue', '/attention', '/proposals', '/kb', '/routines', '/workflows', '/stats', '/system', '/settings', '/settings/plugins'];
  for (const sz of sizes) {
    test(`no horizontal page overflow at ${sz.name} (${sz.w}px)`, async ({ page }) => {
      seed();
      await page.setViewportSize({ width: sz.w, height: sz.h });
      for (const path of pages) {
        await page.goto(path);
        // The nav is present and the document does not scroll sideways: any wide
        // content (tables) scrolls inside its own container, not the page body.
        await expect(page.locator('nav.top')).toBeVisible();
        const overflow = await page.evaluate(() => {
          const el = document.scrollingElement || document.documentElement;
          return el.scrollWidth - el.clientWidth;
        });
        expect(overflow, `${path} overflows by ${overflow}px at ${sz.w}px`).toBeLessThanOrEqual(1);
      }
    });
  }
});

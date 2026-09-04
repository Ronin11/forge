// The Directives page: the file-backed library rendered as its folder tree, with
// composition previews and routine testing. The test daemon's fresh
// FORGE_HOME bootstraps the starter persona library, so real personas are on
// the page.
const { test, expect } = require('@playwright/test');

test.describe('prompts page', () => {
  test('tree shows the library folders and the starter personas', async ({ page }) => {
    await page.goto('/directives');
    await expect(page.locator('h1')).toHaveText('Directives');
    const tree = page.locator('.pr-tree');
    await expect(tree).toContainText('personas/');
    await expect(tree).toContainText('fragments/');
    await expect(tree.locator('[data-sel="prompt:senior-reviewer"]')).toBeVisible();
    await expect(tree.locator('[data-sel="prompt:engineering-standards"]')).toBeVisible();
  });

  test('selecting a persona shows its source and the composed text per mode', async ({ page }) => {
    await page.goto('/directives');
    await page.locator('[data-sel="prompt:senior-reviewer"]').click();
    const detail = page.locator('[data-prompt-detail]');
    await expect(detail).toContainText('persona');
    await expect(detail).toContainText('model: sonnet');
    // Source shows the raw include, the composed pane its expansion.
    await expect(detail.locator('pre').first()).toContainText('{{> engineering-standards}}');
    await expect(detail).toContainText('Composed from');
    await expect(detail.locator('pre').nth(1)).toContainText('Be honest, not flattering.');
    // Switching to the review mode section composes its extra teaching.
    await detail.locator('select').first().selectOption('review');
    await expect(detail.locator('pre').nth(1)).toContainText('Rank findings by severity');
  });

  test('the routines page lists triggers and its dialog edits them', async ({ page }) => {
    const res = await page.request.post('/api/v1/routines', {
      data: { name: 'pr-page-test', target: 'directive:pr-page-test', repositories: ['demo'], timeout_seconds: 300, objective: 'audit the gauges' },
    });
    if (res.status() !== 201) {
      expect((await res.text())).toContain('exists'); // rerun tolerance
    }
    await page.goto('/routines');
    const row = page.locator('tr', { hasText: 'pr-page-test' }).first();
    await expect(row).toContainText('directive:pr-page-test');
    await expect(row).toContainText('audit the gauges');
    // The target chip links into the library.
    await row.locator('a', { hasText: 'directive:pr-page-test' }).click();
    await expect(page).toHaveURL(/directives\?sel=prompt%3Apr-page-test|directives\?sel=prompt:pr-page-test/);
    await expect(page.locator('[data-prompt-detail]')).toContainText('directive');
  });

  test('the routine dialog opens from the row and carries the trigger fields', async ({ page }) => {
    await page.goto('/routines');
    await page.locator('[data-routine-edit="pr-page-test"]').click();
    const dialog = page.locator('[data-routine-dialog]');
    await expect(dialog.locator('[name=name]')).toHaveValue('pr-page-test');
    await expect(dialog.locator('[name=target]')).toHaveValue('directive:pr-page-test');
    await expect(dialog.locator('[name=prompt]')).toHaveCount(0);
    await expect(dialog.locator('[name=mode]')).toHaveCount(0);
    await dialog.locator('[data-editor-cancel]').click();
  });
});

test.describe('prompt editing and testing', () => {
  test('a fragment edits in place: save validates, commits, and recomposes', async ({ page }) => {
    await page.goto('/directives');
    await page.locator('[data-sel="prompt:engineering-standards"]').click();
    const detail = page.locator('[data-prompt-detail]');
    await expect(detail.locator('pre').first()).toContainText('Be honest, not flattering.');
    await detail.locator('button', { hasText: 'Edit' }).first().click();
    const ta = detail.locator('textarea.pr-source');
    const original = await ta.inputValue();
    await ta.fill(original + '\nUI EDIT MARKER.');
    await detail.locator('button', { hasText: 'Save' }).click();
    await expect(detail.locator('pre').first()).toContainText('UI EDIT MARKER.');
    // The edit composes immediately: a persona including this fragment shows it.
    await page.locator('[data-sel="prompt:senior-reviewer"]').click();
    await expect(detail.locator('pre').nth(1)).toContainText('UI EDIT MARKER.');
  });

  test('a breaking edit is refused and the file stays intact', async ({ page }) => {
    await page.goto('/directives');
    await page.locator('[data-sel="prompt:escalation"]').click();
    const detail = page.locator('[data-prompt-detail]');
    await detail.locator('button', { hasText: 'Edit' }).first().click();
    await detail.locator('textarea.pr-source').fill('{{> does-not-exist}}');
    await detail.locator('button', { hasText: 'Save' }).click();
    await expect(page.locator('#editor-error')).toBeVisible();
    await expect(page.locator('#editor-error')).toContainText('not found');
    // Re-selecting shows the untouched file.
    await page.locator('[data-sel="prompt:engineering-standards"]').click();
    await page.locator('[data-sel="prompt:escalation"]').click();
    await expect(detail.locator('pre').first()).toContainText('When to decide and when to ask');
  });

  test('the persona tester renders the full assembly with task and objective', async ({ page }) => {
    await page.goto('/directives');
    await page.locator('[data-sel="prompt:qa-engineer"]').click();
    const detail = page.locator('[data-prompt-detail]');
    const tester = detail.locator('.pr-test');
    await tester.locator('textarea').fill('Break {{repo}} on purpose: {{objective}}');
    await tester.locator('input[placeholder^="objective"]').fill('the checkout flow');
    await tester.locator('input[placeholder^="repository"]').fill('demo');
    await tester.locator('button', { hasText: 'Preview' }).click();
    const preview = detail.locator('pre').last();
    await expect(preview).toContainText('professionally distrustful');
    await expect(preview).toContainText('Break demo on purpose: the checkout flow');
    await expect(preview).toContainText('YOUR TASK');
    // The run panel is present with a model picker; clicking would spend a
    // real completion, so the suite only asserts the controls.
    await expect(detail.locator('button', { hasText: 'Run test' })).toBeVisible();
    await expect(detail.locator('option', { hasText: 'sonnet (default)' })).toHaveCount(1);
  });

  test('the optimize panel offers goal, models, and variant count on personas', async ({ page }) => {
    // Presence only: starting an experiment spends many real completions.
    await page.goto('/directives?sel=prompt:senior-reviewer');
    const detail = page.locator('[data-prompt-detail]');
    const opt = detail.locator('.pr-optimize');
    await expect(opt.locator('textarea[placeholder^="Goal"]')).toBeVisible();
    await expect(opt.locator('option', { hasText: 'run on: haiku' })).toHaveCount(1);
    await expect(opt.locator('.pr-variants')).toHaveValue('8');
    await expect(opt.locator('button', { hasText: 'Start experiment' })).toBeVisible();
    // The target defaults to the persona's model, the optimizer to the
    // biggest alias.
    await expect(opt.locator('select').first()).toHaveValue('sonnet');
    await expect(opt.locator('select').nth(1)).toHaveValue('fable');
    // The expected-cost line prices the run from the composed prompt and the
    // models' list prices, and follows the picker.
    await expect(opt.locator('.pr-cost')).toContainText(/expected cost ≈ \$\d/);
    await expect(opt.locator('.pr-cost')).toContainText('9 runs on sonnet + 2 fable calls');
    await opt.locator('select').first().selectOption('haiku');
    await opt.locator('.pr-variants').fill('4');
    await expect(opt.locator('.pr-cost')).toContainText('5 runs on haiku + 2 fable calls');

  });
});

test.describe('deep links', () => {
  test('?sel selects on load, with the composer mode from the URL', async ({ page }) => {
    await page.goto('/directives?sel=prompt:senior-reviewer&mode=review');
    const detail = page.locator('[data-prompt-detail]');
    await expect(detail).toContainText('senior-reviewer');
    await expect(page.locator('[data-sel="prompt:senior-reviewer"]')).toHaveClass(/on/);
    await expect(detail.locator('select').first()).toHaveValue('review');
    await expect(detail.locator('pre').nth(1)).toContainText('Rank findings by severity');
  });

  test('tree clicks push the selection into the URL; back returns', async ({ page }) => {
    await page.goto('/directives');
    await page.locator('[data-sel="prompt:triager"]').click();
    await expect(page).toHaveURL(/sel=prompt%3Atriager/);
    await page.locator('[data-sel="prompt:escalation"]').click();
    await expect(page).toHaveURL(/sel=prompt%3Aescalation/);
    await page.goBack();
    await expect(page.locator('[data-prompt-detail]')).toContainText('triager');
    // Tree items are real links: the href is shareable.
    await expect(page.locator('[data-sel="prompt:triager"]')).toHaveAttribute('href', '/directives?sel=prompt:triager');
  });

  test('the composition manifest links between prompts', async ({ page }) => {
    await page.goto('/directives?sel=prompt:senior-reviewer');
    const detail = page.locator('[data-prompt-detail]');
    await detail.locator('a[data-nav="prompt:engineering-standards"]').first().click();
    await expect(detail).toContainText('engineering-standards');
    await expect(page).toHaveURL(/sel=prompt%3Aengineering-standards/);
  });
});

test.describe('directives', () => {
  test('the tree has a directives section and the detail composes and previews', async ({ page }) => {
    await page.goto('/directives');
    const tree = page.locator('.pr-tree');
    await expect(tree).toContainText('directives/');
    await tree.locator('[data-sel="prompt:triage-repo"]').click();
    const detail = page.locator('[data-prompt-detail]');
    await expect(detail).toContainText('directive');
    await expect(detail).toContainText('mode: run');
    // The composed body expands with its manifest.
    await expect(detail).toContainText('Composed from');
    await expect(detail.locator('pre').nth(1)).toContainText('Survey the current state of {{repo}}');
    // The preview renders the full assembly without a model call.
    const tester = detail.locator('.pr-test');
    await tester.locator('textarea').fill('find the gaps');
    await tester.locator('button', { hasText: 'Preview' }).click();
    const preview = detail.locator('pre').last();
    await expect(preview).toContainText('find the gaps');
    await expect(preview).toContainText('YOUR TASK');
    await expect(detail.locator('button', { hasText: 'Run test' })).toBeVisible();
  });

  test('tree sections collapse, remember, and reopen while filtering', async ({ page }) => {
    await page.goto('/directives');
    const sec = page.locator('.pr-sec[data-sec="fragments"]');
    await expect(sec.locator('.pr-item').first()).toBeVisible();
    await sec.locator('summary').click();
    await expect(sec.locator('.pr-item').first()).toBeHidden();
    // The toggle event (and its localStorage write) is queued async — wait
    // for it before reloading.
    await expect.poll(() => page.evaluate(() => localStorage.getItem('forge.tree.fragments'))).toBe('closed');
    // The collapse survives a reload (localStorage).
    await page.reload();
    await expect(page.locator('.pr-sec[data-sec="fragments"] .pr-item').first()).toBeHidden();
    // Filtering reopens sections so matches are visible.
    await page.locator('[data-tree-filter]').fill('engineering');
    await expect(page.locator('[data-sel="prompt:engineering-standards"]')).toBeVisible();
    await page.locator('[data-tree-filter]').fill('');
    await page.locator('.pr-sec[data-sec="fragments"] summary').click(); // restore open for later specs
  });
});

test.describe('scripts and search', () => {
  test('the tree filter narrows, and a seeded script runs in the sandbox', async ({ page }) => {
    // Seed a script straight into the daemon's library; the 30s reload is too
    // slow for a test, so use the same pre-seeded file global-setup wrote —
    // check it exists, else write + wait via the API-side reload on PUT.
    await page.goto('/directives');
    const tree = page.locator('.pr-tree');
    await expect(tree).toContainText('scripts/');
    await expect(tree.locator('[data-sel="prompt:wfg-shape"]')).toBeVisible();
    // Filter narrows to matching names.
    await tree.locator('[data-tree-filter]').fill('wfg-shape');
    await expect(tree.locator('[data-sel="prompt:triage-repo"]')).toBeHidden();
    await expect(tree.locator('[data-sel="prompt:wfg-shape"]')).toBeVisible();
    await tree.locator('[data-tree-filter]').fill('');
    // The script pane: chips + sandbox run.
    await tree.locator('[data-sel="prompt:wfg-shape"]').click();
    const detail = page.locator('[data-prompt-detail]');
    await expect(detail).toContainText('script');
    await detail.locator('.pr-test textarea').fill('{"n": 21}');
    await detail.locator('button', { hasText: 'Run script' }).click();
    await expect(detail.locator('pre').last()).toContainText('42');
  });
});

// The Prompts page: the file-backed library rendered as its folder tree, with
// composition previews and routine testing. The test daemon's fresh
// FORGE_HOME bootstraps the starter persona library, so real personas are on
// the page.
const { test, expect } = require('@playwright/test');

test.describe('prompts page', () => {
  test('tree shows the library folders and the starter personas', async ({ page }) => {
    await page.goto('/routines');
    await expect(page.locator('h1')).toHaveText('Prompts');
    const tree = page.locator('.pr-tree');
    await expect(tree).toContainText('personas/');
    await expect(tree).toContainText('fragments/');
    await expect(tree).toContainText('routines/');
    await expect(tree.locator('[data-sel="prompt:senior-reviewer"]')).toBeVisible();
    await expect(tree.locator('[data-sel="prompt:engineering-standards"]')).toBeVisible();
  });

  test('selecting a persona shows its source and the composed text per mode', async ({ page }) => {
    await page.goto('/routines');
    await page.locator('[data-sel="prompt:senior-reviewer"]').click();
    const detail = page.locator('[data-prompt-detail]');
    await expect(detail).toContainText('persona');
    await expect(detail).toContainText('model: sonnet');
    // Source shows the raw include, the composed pane its expansion.
    await expect(detail.locator('pre').first()).toContainText('{{> engineering-standards}}');
    await expect(detail).toContainText('Composed from');
    await expect(detail.locator('pre').nth(1)).toContainText('Be honest, not flattering.');
    // Switching to the review mode section composes its extra teaching.
    await detail.locator('select').selectOption('review');
    await expect(detail.locator('pre').nth(1)).toContainText('Rank findings by severity');
  });

  test('a routine detail previews the exact prompt with objective and repo substituted', async ({ page }) => {
    // A routine bound to a starter persona, created through the API.
    const res = await page.request.post('/api/v1/routines', {
      data: {
        name: 'pr-page-test', mode: 'run', prompt: 'Task on {{repo}}: {{objective}}',
        persona: 'senior-reviewer', repositories: ['demo'], timeout_seconds: 300,
      },
    });
    if (res.status() !== 201) {
      expect((await res.text())).toContain('exists'); // rerun tolerance
    }
    await page.goto('/routines');
    await page.locator('[data-sel="routine:pr-page-test"]').click();
    const detail = page.locator('[data-prompt-detail]');
    await expect(detail).toContainText('persona: senior-reviewer');
    await detail.locator('.pr-test textarea').fill('audit the gauges');
    await detail.locator('.pr-test button').click();
    await expect(detail).toContainText('composed from');
    const preview = detail.locator('pre').last();
    await expect(preview).toContainText('You are the senior reviewer');
    await expect(preview).toContainText('Task on demo: audit the gauges');
    await expect(preview).toContainText('YOUR TASK');
  });

  test('the routine dialog still opens from the detail pane', async ({ page }) => {
    await page.goto('/routines');
    await page.locator('[data-sel="routine:pr-page-test"]').click();
    await page.locator('[data-prompt-detail] button', { hasText: 'Edit' }).click();
    const dialog = page.locator('[data-routine-dialog]');
    await expect(dialog.locator('[name=name]')).toHaveValue('pr-page-test');
    await expect(dialog.locator('[name=persona]')).toHaveValue('senior-reviewer');
  });
});

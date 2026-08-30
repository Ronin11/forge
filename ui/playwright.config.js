// Playwright config for the Forge UI tests (docs/VERIFICATION.md "Forge's own UI").
// global-setup.js builds the forge binary, starts a daemon on 127.0.0.1:7346 with a
// throwaway FORGE_HOME (ui/.tmp-home), and seeds fixture data over the HTTP API —
// no network and no real claude binary are involved (STYLE.md §11).
const { defineConfig, devices } = require('@playwright/test');

module.exports = defineConfig({
  testDir: './tests',
  timeout: 30_000,
  workers: 1,
  retries: 1,
  fullyParallel: false,
  globalSetup: require.resolve('./global-setup'),
  outputDir: 'test-results',
  reporter: [['list'], ['html', { outputFolder: 'playwright-report', open: 'never' }]],
  use: {
    baseURL: 'http://127.0.0.1:7346',
    screenshot: 'only-on-failure',
    trace: 'off',
  },
  projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'] } }],
});

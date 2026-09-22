// Run by web/tests/stats_render.rs under node: pure rendering, no DOM —
// a fixture StatsDoc renders every tab (including a sortable table per
// tab) and the chart's 30 points; the factors tab hides itself when
// `factors` is absent from the document.
const assert = require('node:assert/strict');
const { TABS, visibleTabs, renderTab, renderTable, sortRows, renderDailyChart } = require('../src/stats.js');

function daily30() {
  const out = [];
  for (let i = 0; i < 30; i++) {
    const day = String(i + 1).padStart(2, '0');
    out.push({ date: `2026-08-${day}`, landed: i % 4, cost_usd: (i % 4) * 1.5 });
  }
  return out;
}

const FIXTURE = {
  workflows: [
    {
      workflow: 'direct', hash: 'h1', pieces: 20, succeeded: 16, failed: 2, blocked: 1, unverified: 1,
      attempts: 22, mean_cost_usd: 12.5, cost_per_success_usd: 0.78, landed: 15, cost_per_landed_usd: 0.83,
      broke_base: 1, broke_base_share: 0.0667, repaired: 1, repaired_share: 1.0, repair_cost_usd: 0.4,
      true_cost_per_landed_usd: 0.86, churn_share: 0.1,
      rate: 0.8, rate_lo: 0.6, rate_hi: 0.92, regressed: false,
    },
    {
      workflow: 'direct', hash: 'h0', pieces: 10, succeeded: 9, failed: 1, blocked: 0, unverified: 0,
      attempts: 10, mean_cost_usd: 5.0, cost_per_success_usd: 0.56, landed: 9, cost_per_landed_usd: 0.56,
      broke_base: 0, broke_base_share: 0, repaired: 0, repaired_share: 0, repair_cost_usd: 0,
      true_cost_per_landed_usd: 0.56, churn_share: null,
      rate: 0.9, rate_lo: 0.7, rate_hi: 0.98, regressed: false,
    },
  ],
  assessment_correlation: [
    { measure: 'churn', rho: -0.4, n: 12 },
    { measure: 'repair_cost', rho: null, n: 1 },
  ],
  by_role: [
    { role: 'code', provider: 'anthropic', model: 'claude-sonnet-5', kind: 'attempt', attempts: 20, succeeded: 16, succeeded_share: 0.8, mean_turns: 12.5, mean_cost_usd: 0.6, mean_secs: 300, landed: 15, broke_base: 1, broke_base_share: 0.0667 },
    { role: 'notify', provider: '', model: '', kind: 'job_step', attempts: 4, succeeded: 0, succeeded_share: null, mean_turns: 0, mean_cost_usd: 0.01, mean_secs: 2, landed: null, broke_base: null, broke_base_share: null },
  ],
  human_attention: [
    { workflow: 'direct', hash: 'h1', landed: 15, operator_answers: 2, hand_landed: 1, withdrawals: 0, hand_commits: 3, events: 6, events_per_landed: 0.4 },
  ],
  human_attention_projects: [
    { project: 'demo', landed: 15, operator_answers: 2, hand_landed: 1, withdrawals: 0, hand_commits: 3, events: 6, events_per_landed: 0.4 },
  ],
  time_to_live: [
    { workflow: 'direct', hash: 'h1', n: 15, median_secs: 3600, p90_secs: 9000 },
  ],
  time_to_live_projects: [
    { project: 'demo', n: 15, median_secs: 3600, p90_secs: 9000 },
  ],
  factors: [
    { factor: 'workflow', level: 'direct', tasks: 30, landed: 24, rate: 0.8, rate_lo: 0.63, rate_hi: 0.9, mean_true_cost_usd: 0.7, is_reference: true, effect: null, effect_se: null },
    { factor: 'workflow', level: 'other', tasks: 4, landed: 1, rate: 0.25, rate_lo: 0.05, rate_hi: 0.7, mean_true_cost_usd: 1.2, is_reference: false, effect: 0.3, effect_se: 0.1 },
  ],
  daily: daily30(),
};

// Every tab in TABS is present when `factors` is an array; each tab's
// `renderTab` produces at least one `<table`.
assert.equal(TABS.length, 6, TABS.map(t => t.id));
const tabs = visibleTabs(FIXTURE);
assert.equal(tabs.length, 6, tabs.map(t => t.id));
for (const tab of tabs) {
  const html = renderTab(tab.id, FIXTURE, null);
  assert.match(html, /<table/, `tab ${tab.id} renders no table: ${html}`);
}

// The workflows tab draws the verified-rate interval as a bar (a
// `.bar-track`/`.bar-range`/`.bar-point`), not just a number.
const workflowsHtml = renderTab('workflows', FIXTURE, null);
assert.match(workflowsHtml, /class="bar-track"/, workflowsHtml);
assert.match(workflowsHtml, /class="bar-range"/, workflowsHtml);
assert.match(workflowsHtml, /class="bar-point"/, workflowsHtml);

// The quality tab shows every defect-escape/delayed-cost column plus the
// assessment-correlation line.
const qualityHtml = renderTab('quality', FIXTURE, null);
assert.match(qualityHtml, /repair cost/);
assert.match(qualityHtml, /true cost/);
assert.match(qualityHtml, /churn%/);
assert.match(qualityHtml, /rho -0\.40/);
assert.match(qualityHtml, /rho - \(n=1\)/);

// The by-role tab carries the kind column, distinguishing an attempt row
// from a job-step row.
const byRoleHtml = renderTab('by-role', FIXTURE, null);
assert.match(byRoleHtml, />kind</);
assert.match(byRoleHtml, />attempt</);
assert.match(byRoleHtml, />job_step</);

// Human attention and time to live each render two tables: by workflow
// and by project.
const humanHtml = renderTab('human-attention', FIXTURE, null);
assert.equal((humanHtml.match(/<table/g) || []).length, 2, humanHtml);
assert.match(humanHtml, />By workflow</);
assert.match(humanHtml, />By project</);
const ttlHtml = renderTab('time-to-live', FIXTURE, null);
assert.equal((ttlHtml.match(/<table/g) || []).length, 2, ttlHtml);

// A document with no `factors` key at all (an older `forge` with no
// `--factors` verb) hides the factors tab, rather than show it empty.
const noFactors = { ...FIXTURE };
delete noFactors.factors;
const tabsNoFactors = visibleTabs(noFactors);
assert.equal(tabsNoFactors.length, 5, tabsNoFactors.map(t => t.id));
assert.ok(!tabsNoFactors.some(t => t.id === 'factors'));
// An empty array (the verb exists, nothing to show yet) still shows the tab.
const emptyFactors = { ...FIXTURE, factors: [] };
assert.ok(visibleTabs(emptyFactors).some(t => t.id === 'factors'));

// Every table is sortable by column: sorting the workflows table by
// `pieces` ascending puts the smaller one first.
const byPieces = sortRows('workflows', FIXTURE.workflows, 'pieces', 'asc');
assert.deepEqual(byPieces.map(r => r.hash), ['h0', 'h1']);
const byPiecesDesc = sortRows('workflows', FIXTURE.workflows, 'pieces', 'desc');
assert.deepEqual(byPiecesDesc.map(r => r.hash), ['h1', 'h0']);
// A row with a null value for the sorted column sorts last either way.
const byChurn = sortRows('quality', FIXTURE.workflows, 'churn_share', 'desc');
assert.equal(byChurn[byChurn.length - 1].hash, 'h0', 'the null churn_share row sorts last');

// A table's `<th>` carries the click target (`data-table`/`data-key`)
// and shows the active sort's arrow.
const sortedHtml = renderTable('workflows', FIXTURE, { table: 'workflows', key: 'pieces', dir: 'asc' });
assert.match(sortedHtml, /data-table="workflows" data-key="pieces"[^>]*>tasks ▲/);

// The 30-day chart always draws exactly 30 points: one bar and one dot
// per day, oldest first.
const chart = renderDailyChart(FIXTURE.daily);
assert.equal(chart.points, 30, chart);
assert.equal((chart.svgHtml.match(/class="chart-bar"/g) || []).length, 30, chart.svgHtml);
assert.equal((chart.svgHtml.match(/class="chart-dot"/g) || []).length, 30, chart.svgHtml);
assert.match(chart.svgHtml, /2026-08-01/);
assert.match(chart.svgHtml, /2026-08-30/);

// A quiet scope (every day zeroed) still draws 30 points, not a crash on
// dividing by a zero max.
const quiet = renderDailyChart(Array.from({ length: 30 }, (_, i) => ({ date: `2026-01-${String(i + 1).padStart(2, '0')}`, landed: 0, cost_usd: 0 })));
assert.equal(quiet.points, 30);
assert.equal((quiet.svgHtml.match(/class="chart-bar"/g) || []).length, 30);

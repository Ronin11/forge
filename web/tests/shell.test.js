// Run by web/tests/shell_render.rs under node: pure rendering, no DOM —
// a fixture snapshot (a worker, some tasks, a `forge doctor --json` read)
// renders the full nav (every page on the client contract, the current
// one bold) and a populated header strip (worker state, both rate
// windows as gauges with their reset times, queued/running counts,
// today's spend). `fmtTime` is a deterministic stub here — time.js's own
// zone handling is covered by web/tests/time.rs — so this test is only
// about shell.js's own assembly.
const assert = require('node:assert/strict');
const { NAV_PAGES, SHORTCUT_TARGETS, renderNav, renderHeaderStrip } = require('../src/shell.js');
const fmtTime = secs => `T${secs}`;

// Every page the client contract names, exactly once, each with a
// distinct g-then-letter shortcut.
const EXPECTED_PAGES = [
  'tasks', 'requests', 'projects', 'initiatives', 'workflows', 'jobs',
  'deploys', 'stats', 'graph', 'plugins', 'activity', 'messages', 'doctor',
];
assert.deepEqual(NAV_PAGES.map(p => p.key), EXPECTED_PAGES);
assert.equal(new Set(Object.values(SHORTCUT_TARGETS)).size, Object.keys(SHORTCUT_TARGETS).length);
for (const p of NAV_PAGES) {
  assert.ok(Object.values(SHORTCUT_TARGETS).includes(p.href), `no shortcut jumps to ${p.href}`);
}

// The nav names every page, and only the current one is bold.
{
  const html = renderNav('stats', ' <a href="/x" class="extra-sub-nav">sub</a>');
  for (const p of NAV_PAGES) assert.match(html, new RegExp(`href="${p.href}"`), p.href);
  assert.match(html, /<a href="\/stats" style="font-weight:600">stats<\/a>/);
  assert.doesNotMatch(html, /<a href="\/tasks" style="font-weight:600">/);
  assert.ok(html.includes('extra-sub-nav'), 'extraHtml is appended');
}
{
  const html = renderNav('tasks', '');
  assert.match(html, /<a href="\/tasks" style="font-weight:600">tasks<\/a>/);
}

// The header strip: a worker running, two rate windows across two
// providers (the worse one of each wins the gauge), a doctor-reported
// queue count that differs from the raw task list (proving it, not the
// task list, drives the number when present), and today's spend.
const FIXTURE = {
  worker: { running: true, pid: 42, stale_binary: false },
  tasks: [
    { state: 'queued' }, { state: 'queued' }, { state: 'queued' },
    { state: 'running' }, { state: 'running' },
    { state: 'succeeded' },
  ],
  doctor: [
    { name: 'queue', status: 'ok', detail: '2 queued, 1 running', hint: '', queued: 2, running: 1 },
    { name: 'spend', status: 'ok', detail: '$3.50 of $10.00', hint: '', spend_usd: 3.5, spend_cap_usd: 10 },
    {
      name: 'rate_limit', status: 'ok', detail: 'anthropic', hint: '', provider: 'anthropic',
      five_hour_pct: 0.82, five_hour_resets_at: 1700000000,
      seven_day_pct: 0.4, seven_day_resets_at: 1700600000,
    },
    {
      name: 'rate_limit', status: 'ok', detail: 'openai', hint: '', provider: 'openai',
      five_hour_pct: 0.5, five_hour_resets_at: 111,
      seven_day_pct: 0.1, seven_day_resets_at: 222,
    },
  ],
  now: 1700000500,
};
const strip = renderHeaderStrip(FIXTURE, fmtTime);
assert.match(strip, /worker pid 42/);
assert.doesNotMatch(strip, /worker not running/);
// The worse of the two providers' 5h/7d windows wins the gauge — here
// always anthropic, on both windows.
assert.match(strip, /anthropic 5h 82%.*resets T1700000000/);
assert.match(strip, /anthropic 7d 40%.*resets T1700600000/);
// The doctor's own queue check (2, 1), not the six-task fixture list
// (3 queued, 2 running), is what the strip shows.
assert.match(strip, /2 queued · 1 running/);
assert.match(strip, /spend \$3\.50 of \$10\.00/);
assert.match(strip, /updated T1700000500/);

// No doctor read yet (a fresh page load before /api/doctor answers):
// gauges and spend fall back to a plain placeholder, never a crash, and
// the queue count still comes from the task list.
const bare = renderHeaderStrip({ worker: { running: false }, tasks: FIXTURE.tasks, doctor: [], now: null }, fmtTime);
assert.match(bare, /worker not running/);
assert.match(bare, /5h —/);
assert.match(bare, /7d —/);
assert.match(bare, /spend —/);
assert.match(bare, /3 queued · 2 running/);

console.log('shell.js renders the nav and header strip from a fixture snapshot');

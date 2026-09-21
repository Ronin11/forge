// Run by web/tests/time.rs once per zone, with TZ set in the environment:
// the helper must render a known instant in the zone the process is in, the
// way the browser's zone decides for a viewer.
const assert = require('node:assert/strict');
const { fmtTime, fmtSpan, fmtAgo } = require('../src/time.js');

// 2026-09-21 14:13:20 UTC, and 2027-01-15 08:00:00 UTC (outside daylight time).
const SUMMER = 1790000000;
const WINTER = 1800000000;
const EXPECTED = {
  UTC: ['2026-09-21 14:13', '2027-01-15 08:00'],
  'America/New_York': ['2026-09-21 10:13', '2027-01-15 03:00'],
  'Asia/Kolkata': ['2026-09-21 19:43', '2027-01-15 13:30'],
  'Pacific/Auckland': ['2026-09-22 02:13', '2027-01-15 21:00'],
};

const tz = process.env.TZ;
assert.ok(EXPECTED[tz], `TZ must be one of ${Object.keys(EXPECTED)}, got ${tz}`);
assert.equal(fmtTime(SUMMER), EXPECTED[tz][0]);
assert.equal(fmtTime(WINTER), EXPECTED[tz][1]);

// A time the server has not recorded renders as nothing, never "Invalid Date".
for (const missing of [null, undefined, NaN, '2026-09-21']) assert.equal(fmtTime(missing), '');

assert.equal(fmtSpan(0), '0s');
assert.equal(fmtSpan(45), '45s');
assert.equal(fmtSpan(300), '5m');
assert.equal(fmtSpan(7380), '2h 3m');
assert.equal(fmtSpan(7200), '2h');
assert.equal(fmtSpan(3 * 86400 + 4 * 3600), '3d 4h');

assert.equal(fmtAgo(SUMMER - 300, SUMMER), '5m ago');
assert.equal(fmtAgo(SUMMER + 7380, SUMMER), 'in 2h 3m');
assert.equal(fmtAgo(SUMMER - 3, SUMMER), 'just now');
assert.equal(fmtAgo(null, SUMMER), '');

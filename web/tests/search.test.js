// Run by web/tests/search_render.rs under node: pure functions, no DOM —
// the /tasks list's filters (web UI task 9, "search") round-trip through
// a URL query string the same way a browser's `location.search` would
// carry them.
const assert = require('node:assert/strict');
const { FILTER_KEYS, emptyFilters, filtersFromSearch, paramsFromFilters } = require('../src/search.js');

// Every filter `forge log --json` itself takes, mapped one to one:
// text (`--grep`, as `q`), state, repository (as `repo`), workflow,
// project, initiative.
assert.deepEqual(FILTER_KEYS, ['q', 'state', 'repo', 'workflow', 'project', 'initiative']);
assert.deepEqual(emptyFilters(), { q: '', state: '', repo: '', workflow: '', project: '', initiative: '' });

// A URL with every filter set, plus the `before` paging cursor, parses
// back into exactly what built it — the round trip the task asks for.
{
  const filters = {
    q: 'doctor json',
    state: 'failed',
    repo: '/repos/demo',
    workflow: 'direct',
    project: 'demo',
    initiative: '7',
  };
  const before = 120;
  const params = paramsFromFilters(filters, before);
  assert.equal(
    params.toString(),
    'q=doctor+json&state=failed&repo=%2Frepos%2Fdemo&workflow=direct&project=demo&initiative=7&before=120',
  );
  const round = filtersFromSearch(`?${params}`);
  assert.deepEqual(round.filters, filters);
  assert.equal(round.before, before);
}

// Empty filters and no cursor produce an empty query string, and parsing
// an empty (or absent) search string back gives the empty filter set with
// no cursor.
{
  const params = paramsFromFilters(emptyFilters(), null);
  assert.equal(params.toString(), '');
  const round = filtersFromSearch('');
  assert.deepEqual(round.filters, emptyFilters());
  assert.equal(round.before, null);
}

// A leading '?' is optional, and an unset filter or a non-numeric
// `before` reads back as empty/absent rather than throwing.
{
  const round = filtersFromSearch('state=queued&before=not-a-number');
  assert.equal(round.filters.state, 'queued');
  assert.equal(round.filters.q, '');
  assert.equal(round.before, null);
}

// Only the filters that are set land in the query string — an unset one
// is omitted, not written as an empty pair.
{
  const params = paramsFromFilters({ ...emptyFilters(), project: 'demo' }, null);
  assert.equal(params.toString(), 'project=demo');
}

console.log('ok');

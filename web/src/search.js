// The /tasks list's filters (web UI task 9, "search"): pure functions, no
// DOM, so web/tests/search.test.js can exercise the URL round-trip under
// node exactly like time.js and shell.js. Mapped one to one onto `forge
// log --json`'s own flags (docs/CLIENT.md): `q` (--grep: task text or an
// exact id, title, plan or last result summary; each row's `matched` says
// which), `state`, `repo` (--repo, a repository path), `workflow`,
// `project`, `initiative`. `before` is the paging cursor `forge log
// --before` itself takes, carried alongside the filters so a scrolled-to
// page round-trips through the URL too.
(function (root, factory) {
  const api = factory();
  if (typeof module === 'object' && module.exports) module.exports = api;
  else root.ForgeSearch = api;
})(globalThis, () => {
  const FILTER_KEYS = ['q', 'state', 'repo', 'workflow', 'project', 'initiative'];

  function emptyFilters() {
    const f = {};
    for (const k of FILTER_KEYS) f[k] = '';
    return f;
  }

  // Parses a `location.search`-shaped string (leading '?' optional, also
  // takes a bare query string) into `{filters, before}` — the inverse of
  // `paramsFromFilters`.
  function filtersFromSearch(search) {
    const p = new URLSearchParams(search || '');
    const filters = emptyFilters();
    for (const k of FILTER_KEYS) filters[k] = p.get(k) || '';
    const rawBefore = p.get('before');
    const before = rawBefore && /^\d+$/.test(rawBefore) ? Number(rawBefore) : null;
    return { filters, before };
  }

  // Builds a `URLSearchParams` carrying only the filters that are set, in
  // a fixed order, plus `before` when given. Used both for the address
  // bar (so a filtered, paged view can be linked) and, with `limit` added
  // by the caller, for the `/api/tasks` fetch itself — the same filters,
  // the same query string, one code path for both.
  function paramsFromFilters(filters, before) {
    const p = new URLSearchParams();
    for (const k of FILTER_KEYS) {
      const v = filters && filters[k];
      if (v) p.set(k, v);
    }
    if (before) p.set('before', String(before));
    return p;
  }

  return { FILTER_KEYS, emptyFilters, filtersFromSearch, paramsFromFilters };
});

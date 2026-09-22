// Pure rendering for the /stats page (web UI task 5): no DOM, no fetch, so
// web/tests/stats.test.js can exercise it under node exactly like
// workflows.js — the tabbed tables `forge stats --json`'s StatsDoc backs,
// each one sortable by column, and the 30-day chart of daily landings and
// spend drawn as SVG from StatsDoc.daily.
(function (root, factory) {
  const api = factory();
  if (typeof module === 'object' && module.exports) module.exports = api;
  else root.ForgeStats = api;
})(globalThis, () => {
  const esc = s => String(s ?? '').replace(/[&<>"]/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));
  const pct = v => v == null ? '-' : (v * 100).toFixed(0) + '%';
  const usd = v => v == null ? '-' : '$' + Number(v).toFixed(2);
  const num = v => v == null ? '-' : v;
  const num1 = v => v == null ? '-' : Number(v).toFixed(1);
  const fsecs = v => v == null ? '-' : Number(v).toFixed(0) + 's';
  const num4 = v => v == null ? '-' : Number(v).toFixed(4);

  // The verified-rate interval as a bar: the range track is
  // `rate_lo`-`rate_hi`, the point mark is `rate`, and a workflow's
  // current version whose interval sits entirely below its previous
  // version's carries `regressed` — the mark this draws as "REGRESSION".
  function renderRateBar(rate, lo, hi, regressed) {
    if (rate == null || lo == null || hi == null) return '<span class="mute">-</span>';
    const p = v => Math.max(0, Math.min(100, v * 100));
    const left = p(lo), width = Math.max(0, p(hi) - p(lo));
    return `<div class="rate-bar">
      <div class="bar-track"><div class="bar-range" style="left:${left.toFixed(1)}%;width:${width.toFixed(1)}%"></div><div class="bar-point" style="left:${p(rate).toFixed(1)}%"></div></div>
      <span class="rate-label">${pct(rate)} <span class="mute">(${pct(lo)}–${pct(hi)})</span></span>${regressed ? ' <span class="failed">REGRESSION</span>' : ''}
    </div>`;
  }

  // One column of a sortable table: `key` names it for sorting and for
  // the clicked `<th>`'s `data-key`; `value(row)` is the raw value a sort
  // compares; `render(row)` is the cell's HTML, defaulting to the escaped
  // value.
  function col(key, label, value, render, numeric) {
    return { key, label, value, render: render || (r => esc(String(value(r) ?? '-'))), numeric: numeric !== false };
  }
  function textCol(key, label, value) {
    return col(key, label, value, null, false);
  }

  // Every sortable table `forge stats --json` backs, keyed by id: its
  // columns and where its rows come from in the `StatsDoc`.
  const TABLES = {
    workflows: {
      rows: d => d.workflows || [],
      columns: [
        textCol('workflow', 'workflow', r => r.workflow),
        textCol('hash', 'hash', r => r.hash),
        col('pieces', 'tasks', r => r.pieces),
        col('succeeded', 'ok', r => r.succeeded),
        col('failed', 'fail', r => r.failed),
        col('blocked', 'blk', r => r.blocked),
        col('unverified', 'unv', r => r.unverified),
        col('attempts', 'att', r => r.attempts),
        col('mean_cost_usd', 'cost', r => r.mean_cost_usd, r => usd(r.mean_cost_usd)),
        col('cost_per_success_usd', '$/ok', r => r.cost_per_success_usd, r => usd(r.cost_per_success_usd)),
        col('landed', 'landed', r => r.landed),
        col('rate', 'verified rate', r => r.rate, r => renderRateBar(r.rate, r.rate_lo, r.rate_hi, r.regressed)),
      ],
    },
    quality: {
      rows: d => d.workflows || [],
      columns: [
        textCol('workflow', 'workflow', r => r.workflow),
        textCol('hash', 'hash', r => r.hash),
        col('landed', 'landed', r => r.landed),
        col('broke_base', 'broke base', r => r.broke_base),
        col('broke_base_share', 'broke%', r => r.broke_base_share, r => pct(r.broke_base_share)),
        col('repaired', 'repaired', r => r.repaired),
        col('repaired_share', 'repair%', r => r.repaired_share, r => pct(r.repaired_share)),
        col('repair_cost_usd', 'repair cost', r => r.repair_cost_usd, r => usd(r.repair_cost_usd)),
        col('true_cost_per_landed_usd', 'true cost', r => r.true_cost_per_landed_usd, r => usd(r.true_cost_per_landed_usd)),
        col('churn_share', 'churn%', r => r.churn_share, r => pct(r.churn_share)),
      ],
    },
    'by-role': {
      rows: d => d.by_role || [],
      columns: [
        textCol('role', 'role', r => r.role),
        textCol('provider', 'provider', r => r.provider),
        textCol('model', 'model', r => r.model),
        textCol('kind', 'kind', r => r.kind),
        col('attempts', 'att', r => r.attempts),
        col('succeeded_share', 'succeed%', r => r.succeeded_share, r => pct(r.succeeded_share)),
        col('mean_turns', 'turns', r => r.mean_turns, r => num1(r.mean_turns)),
        col('mean_cost_usd', 'cost', r => r.mean_cost_usd, r => usd(r.mean_cost_usd)),
        col('mean_secs', 'secs', r => r.mean_secs, r => fsecs(r.mean_secs)),
        col('landed', 'landed', r => r.landed, r => num(r.landed)),
        col('broke_base', 'broke base', r => r.broke_base, r => num(r.broke_base)),
        col('broke_base_share', 'broke%', r => r.broke_base_share, r => pct(r.broke_base_share)),
      ],
    },
    'human-attention': {
      rows: d => d.human_attention || [],
      columns: [
        textCol('workflow', 'workflow', r => r.workflow),
        textCol('hash', 'hash', r => r.hash),
        col('landed', 'landed', r => r.landed),
        col('operator_answers', 'answers', r => r.operator_answers),
        col('hand_landed', 'hand', r => r.hand_landed),
        col('withdrawals', 'wdrawn', r => r.withdrawals),
        col('hand_commits', 'handc', r => r.hand_commits),
        col('events', 'events', r => r.events),
        col('events_per_landed', 'evt/land', r => r.events_per_landed, r => (r.events_per_landed == null ? '-' : r.events_per_landed.toFixed(2))),
      ],
    },
    'human-attention-projects': {
      rows: d => d.human_attention_projects || [],
      columns: [
        textCol('project', 'project', r => r.project),
        col('landed', 'landed', r => r.landed),
        col('operator_answers', 'answers', r => r.operator_answers),
        col('hand_landed', 'hand', r => r.hand_landed),
        col('withdrawals', 'wdrawn', r => r.withdrawals),
        col('hand_commits', 'handc', r => r.hand_commits),
        col('events', 'events', r => r.events),
        col('events_per_landed', 'evt/land', r => r.events_per_landed, r => (r.events_per_landed == null ? '-' : r.events_per_landed.toFixed(2))),
      ],
    },
    'time-to-live': {
      rows: d => d.time_to_live || [],
      columns: [
        textCol('workflow', 'workflow', r => r.workflow),
        textCol('hash', 'hash', r => r.hash),
        col('n', 'n', r => r.n),
        col('median_secs', 'median', r => r.median_secs, r => fsecs(r.median_secs)),
        col('p90_secs', 'p90', r => r.p90_secs, r => fsecs(r.p90_secs)),
      ],
    },
    'time-to-live-projects': {
      rows: d => d.time_to_live_projects || [],
      columns: [
        textCol('project', 'project', r => r.project),
        col('n', 'n', r => r.n),
        col('median_secs', 'median', r => r.median_secs, r => fsecs(r.median_secs)),
        col('p90_secs', 'p90', r => r.p90_secs, r => fsecs(r.p90_secs)),
      ],
    },
    factors: {
      rows: d => d.factors || [],
      columns: [
        textCol('factor', 'factor', r => r.factor),
        textCol('level', 'level', r => r.level),
        col('tasks', 'tasks', r => r.tasks),
        col('landed', 'landed', r => r.landed),
        col('rate', 'rate (95% CI)', r => r.rate, r => renderRateBar(r.rate, r.rate_lo, r.rate_hi, false)),
        col('mean_true_cost_usd', 'true cost', r => r.mean_true_cost_usd, r => usd(r.mean_true_cost_usd)),
        textCol('is_reference', 'ref', r => (r.is_reference ? 'ref' : '')),
        col('effect', 'effect(log$)', r => r.effect, r => num4(r.effect)),
        col('effect_se', 'se', r => r.effect_se, r => num4(r.effect_se)),
      ],
    },
  };

  // The tabs, in order, each naming the tables it shows (a tab with two
  // tables — human attention, time to live — shows both, workflow-scoped
  // first, project-scoped second). `factors` hides itself when
  // `doc.factors` is not an array: an older `forge` with no `--factors`
  // verb leaves the key out of `forge stats --json` entirely, rather than
  // send an empty one.
  const TABS = [
    { id: 'workflows', label: 'Workflows', tables: [{ id: 'workflows' }] },
    { id: 'quality', label: 'Quality', tables: [{ id: 'quality' }] },
    { id: 'by-role', label: 'By role', tables: [{ id: 'by-role' }] },
    {
      id: 'human-attention', label: 'Human attention',
      tables: [{ id: 'human-attention', title: 'By workflow' }, { id: 'human-attention-projects', title: 'By project' }],
    },
    {
      id: 'time-to-live', label: 'Time to live',
      tables: [{ id: 'time-to-live', title: 'By workflow' }, { id: 'time-to-live-projects', title: 'By project' }],
    },
    {
      id: 'factors', label: 'Factors', tables: [{ id: 'factors' }],
      hidden: doc => !Array.isArray(doc && doc.factors),
    },
  ];

  // The tabs a given `StatsDoc` actually shows, `factors` dropped when
  // its verb's data is absent.
  function visibleTabs(doc) {
    return TABS.filter(t => !t.hidden || !t.hidden(doc));
  }

  // `rows` sorted by column `key` (as `TABLES[tableId].columns` defines
  // it), ascending or descending; a row missing the value sorts last
  // regardless of direction, same as an unset field in a CLI table reads
  // as "least interesting", not "smallest".
  function sortRows(tableId, rows, key, dir) {
    const column = TABLES[tableId].columns.find(c => c.key === key);
    if (!column) return rows;
    const sign = dir === 'asc' ? 1 : -1;
    return [...rows].sort((a, b) => {
      const av = column.value(a), bv = column.value(b);
      if (av == null && bv == null) return 0;
      if (av == null) return 1;
      if (bv == null) return -1;
      if (av < bv) return -sign;
      if (av > bv) return sign;
      return 0;
    });
  }

  // One table's HTML: a `<table data-table-id>` with clickable, sortable
  // `<th data-table data-key>` headers (an arrow marks the active sort)
  // and a body of `<td>` cells from each column's `render`. `sort` is
  // `{table, key, dir}` or `null`; only applies to `tableId`'s own rows.
  function renderTable(tableId, doc, sort) {
    const def = TABLES[tableId];
    const rows = def.rows(doc) || [];
    const active = sort && sort.table === tableId ? sort : null;
    const thead = def.columns.map(c => {
      const isActive = active && active.key === c.key;
      const arrow = isActive ? (active.dir === 'asc' ? ' ▲' : ' ▼') : '';
      return `<th data-table="${tableId}" data-key="${c.key}" class="sortable${c.numeric ? ' num' : ''}">${esc(c.label)}${arrow}</th>`;
    }).join('');
    const sorted = active ? sortRows(tableId, rows, active.key, active.dir) : rows;
    const body = sorted.map(r => `<tr>${def.columns.map(c => `<td${c.numeric ? ' class="num"' : ''}>${c.render(r)}</td>`).join('')}</tr>`).join('')
      || `<tr><td colspan="${def.columns.length}" class="mute">no data</td></tr>`;
    return `<table data-table-id="${tableId}"><thead><tr>${thead}</tr></thead><tbody>${body}</tbody></table>`;
  }

  // The assessment-correlation line `forge stats --quality` prints under
  // its defect-escape table: how well the assess directive's score
  // tracks each delayed-cost measure. Not a sortable table (`quality`'s
  // own two rows), just the same line the CLI shows.
  function renderCorrelation(rows) {
    if (!rows || !rows.length) return '';
    const rho = v => v == null ? '-' : v.toFixed(2);
    return `<div class="card">${rows.map(r => `score vs ${esc(r.measure)}: rho ${rho(r.rho)} (n=${r.n})`).join(' &middot; ')}</div>`;
  }

  // One tab's full HTML: every table it names, each with its own heading
  // when it names more than one (human attention, time to live).
  function renderTab(tabId, doc, sort) {
    const tab = TABS.find(t => t.id === tabId);
    if (!tab) return '';
    const correlation = tabId === 'quality' ? renderCorrelation(doc.assessment_correlation) : '';
    const tables = tab.tables.map(t => (t.title ? `<h2>${esc(t.title)}</h2>` : '') + renderTable(t.id, doc, sort)).join('');
    return correlation + tables;
  }

  // The 30-day chart above the tabs: daily landings as bars, daily spend
  // as a line over its own scale — `StatsDoc.daily`, always exactly
  // `points` entries (`Store::DAILY_WINDOW_DAYS`), oldest first. Pure SVG,
  // no DOM, drawn the same way `graph.js` draws the module graph.
  function renderDailyChart(daily) {
    const days = daily || [];
    const n = days.length;
    const w = Math.max(n * 14, 40);
    const h = 140;
    const padTop = 8, padBottom = 18;
    const plotH = h - padTop - padBottom;
    const step = w / Math.max(n, 1);
    const barW = Math.max(3, step - 3);
    const maxLanded = Math.max(1, ...days.map(d => d.landed || 0));
    const maxCost = Math.max(0.01, ...days.map(d => d.cost_usd || 0));
    const bars = days.map((d, i) => {
      const x = i * step + 1;
      const bh = ((d.landed || 0) / maxLanded) * plotH;
      const y = padTop + (plotH - bh);
      return `<rect class="chart-bar" x="${x.toFixed(1)}" y="${y.toFixed(1)}" width="${barW.toFixed(1)}" height="${bh.toFixed(1)}"><title>${esc(d.date)}: ${d.landed} landed</title></rect>`;
    }).join('');
    const at = i => {
      const x = i * step + step / 2;
      const y = padTop + plotH - ((days[i].cost_usd || 0) / maxCost) * plotH;
      return [x, y];
    };
    const points = days.map((_, i) => at(i).map(v => v.toFixed(1)).join(',')).join(' ');
    const dots = days.map((d, i) => {
      const [x, y] = at(i);
      return `<circle class="chart-dot" cx="${x.toFixed(1)}" cy="${y.toFixed(1)}" r="2.2"><title>${esc(d.date)}: ${usd(d.cost_usd)}</title></circle>`;
    }).join('');
    const svgHtml = bars + `<polyline class="chart-line" points="${points}" fill="none"></polyline>` + dots;
    return { svgHtml, width: Math.max(w, 1), height: h, points: n };
  }

  return {
    TABS, TABLES,
    visibleTabs, renderTab, renderTable, sortRows,
    renderDailyChart, renderRateBar, renderCorrelation,
  };
});

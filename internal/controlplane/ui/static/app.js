// Row links and the phase timeline. No framework; behaviour attaches by data- attributes.
document.querySelectorAll('tr[data-href]').forEach(function (row) {
  row.addEventListener('click', function (e) {
    if (e.target.closest('a')) return;
    window.location = row.dataset.href;
  });
});
document.querySelectorAll('[data-timeline]').forEach(function (tl) {
  var spans = Array.prototype.slice.call(tl.querySelectorAll('.span'));
  var end = 0;
  spans.forEach(function (s) { end = Math.max(end, Number(s.dataset.elapsed)); });
  if (!end) return;
  spans.forEach(function (s) {
    var dur = Number(s.dataset.dur), start = Number(s.dataset.elapsed) - dur;
    var bar = s.querySelector('.bar');
    bar.style.marginLeft = (100 * Math.max(0, start) / end) + '%';
    bar.style.width = Math.max(0.5, 100 * dur / end) + '%';
    if (s.dataset.parent) s.style.paddingLeft = '1rem';
  });
});

// Queue drag-and-drop: PATCH {move_before}; the daemon refuses an order that
// puts a task above one it is blocked by, and the row snaps back.
(function () {
  var table = document.querySelector('[data-queue] tbody');
  if (!table) return;
  var dragging = null;
  var errBox = document.getElementById('queue-error');
  table.addEventListener('dragstart', function (e) {
    var row = e.target.closest('tr[data-id]');
    if (!row) return;
    dragging = row;
    e.dataTransfer.effectAllowed = 'move';
  });
  table.addEventListener('dragover', function (e) {
    e.preventDefault();
    var over = e.target.closest('tr[data-id]');
    if (!over || over === dragging || !dragging) return;
    var rect = over.getBoundingClientRect();
    var after = e.clientY > rect.top + rect.height / 2;
    over.parentNode.insertBefore(dragging, after ? over.nextSibling : over);
  });
  table.addEventListener('drop', function (e) { e.preventDefault(); commit(); });
  table.addEventListener('dragend', function () { commit(); });
  function commit() {
    if (!dragging) return;
    var moved = dragging; dragging = null;
    var next = moved.nextElementSibling;
    var body = next ? { move_before: next.dataset.id } : { move_before: '' };
    fetch('/api/v1/work/' + moved.dataset.id, {
      method: 'PATCH',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body),
    }).then(function (resp) {
      if (!resp.ok) return resp.json().then(function (e) { throw new Error(e.error || resp.status); });
      window.location.reload();
    }).catch(function (err) {
      errBox.hidden = false;
      errBox.textContent = 'Refused: ' + err.message;
      window.setTimeout(function () { window.location.reload(); }, 1200);
    });
  }
})();

// Human queue: answer a question in place.
document.querySelectorAll('[data-answer-form]').forEach(function (form) {
  form.addEventListener('submit', function (e) {
    e.preventDefault();
    var id = form.dataset.answerForm;
    var answer = form.querySelector('[name=answer]').value;
    fetch('/api/v1/questions/' + id + '/answer', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ answer: answer, by: 'human' }),
    }).then(function (resp) {
      if (!resp.ok) return resp.json().then(function (e) { throw new Error(e.error || resp.status); });
      window.location.reload();
    }).catch(function (err) { window.alert('Answer failed: ' + err.message); });
  });
});

// Proposals: approve/reject post to the API, which owns the rule — approve
// applies inside the decision's transaction, and a refused apply answers 409
// with the proposal left proposed.
document.querySelectorAll('[data-proposal-approve], [data-proposal-reject]').forEach(function (btn) {
  btn.addEventListener('click', function () {
    var id = btn.dataset.proposalApprove || btn.dataset.proposalReject;
    var action = btn.dataset.proposalApprove ? 'approve' : 'reject';
    fetch('/api/v1/proposals/' + id + '/' + action, { method: 'POST' }).then(function (resp) {
      if (!resp.ok) return resp.json().then(function (e) { throw new Error(e.error || resp.status); });
      window.location.reload();
    }).catch(function (err) {
      var box = document.getElementById('proposal-error');
      if (box) {
        box.hidden = false;
        box.textContent = 'Refused: ' + err.message;
      } else {
        window.alert('Refused: ' + err.message);
      }
    });
  });
});

// Repository controls: pause/resume, cancel-running, and the app-url form POST
// to the API and reload — the same pattern as the proposal decision buttons.
(function () {
  function fail(err) {
    var box = document.getElementById('repo-error');
    if (box) {
      box.hidden = false;
      box.textContent = 'Refused: ' + err.message;
    } else {
      window.alert('Refused: ' + err.message);
    }
  }
  function post(path, body) {
    return fetch(path, {
      method: 'POST',
      headers: body ? { 'Content-Type': 'application/json' } : undefined,
      body: body ? JSON.stringify(body) : undefined,
    }).then(function (resp) {
      if (!resp.ok) return resp.json().then(function (e) { throw new Error(e.error || resp.status); });
      window.location.reload();
    }).catch(fail);
  }
  function bind(attr, action) {
    document.querySelectorAll('[' + attr + ']').forEach(function (btn) {
      var name = btn.getAttribute(attr);
      btn.addEventListener('click', function () {
        post('/api/v1/repositories/' + encodeURIComponent(name) + '/' + action);
      });
    });
  }
  bind('data-repo-pause', 'pause');
  bind('data-repo-resume', 'resume');
  bind('data-repo-cancel', 'cancel-running');
  document.querySelectorAll('[data-repo-app-url]').forEach(function (form) {
    form.addEventListener('submit', function (e) {
      e.preventDefault();
      var url = form.querySelector('[name=url]').value;
      post('/api/v1/repositories/' + encodeURIComponent(form.dataset.repoAppUrl) + '/app-url', { url: url });
    });
  });
})();

// Dashboard usage gauges: one per window, target line, filled from the API so
// the page and `forge usage` can never disagree.
(function () {
  var box = document.querySelector('[data-usage]');
  if (!box) return;
  fetch('/api/v1/usage').then(function (resp) {
    if (!resp.ok) return; // budget policy inactive: leave the section hidden
    return resp.json();
  }).then(function (data) {
    if (!data) return;
    box.hidden = false;
    ['five_hour', 'seven_day'].forEach(function (w) {
      var g = box.querySelector('[data-window=' + w + ']');
      var u = data.usage[w], cfg = data.config;
      var target = cfg[w + '_target'], hard = cfg[w + '_hard_stop'];
      if (!u || u.utilization < 0) { g.querySelector('.meta').textContent = 'no samples yet'; return; }
      var fill = g.querySelector('.fill');
      fill.style.width = Math.min(100, 100 * u.utilization) + '%';
      fill.className = 'fill ' + (u.utilization >= hard ? 'bad' : u.utilization >= target ? 'warn' : 'ok');
      g.querySelector('.target').style.left = 100 * target + '%';
      var resets = Math.max(0, Math.round((new Date(u.resets_at) - Date.now()) / 60000));
      g.querySelector('.meta').textContent = (100 * u.utilization).toFixed(1) + '% · target ' + (100 * target).toFixed(0) +
        '% · resets in ' + (resets >= 60 ? Math.floor(resets / 60) + 'h' + (resets % 60) + 'm' : resets + 'm') +
        ' · 1h rate ' + (100 * u.rate_1h).toFixed(1) + '%/h · Δ ' + (u.delta >= 0 ? '+' : '') + (100 * u.delta).toFixed(1) + '%/h';
    });
  }).catch(function () {});
})();

// Click-to-copy: any inline `forge …` command shown anywhere in the UI becomes
// a one-click copy. A single pass upgrades every page, so a command in the
// human queue, an empty state, a hint, or a kb note is copied with one click
// (loopback is a secure context, so the clipboard API is available).
(function () {
  document.querySelectorAll('code').forEach(function (el) {
    var text = el.textContent.trim();
    if (text.indexOf('forge ') !== 0) return;   // only forge commands
    if (el.closest('pre')) return;               // leave fenced blocks alone
    el.classList.add('cmd-copy');
    el.setAttribute('role', 'button');
    el.setAttribute('tabindex', '0');
    el.setAttribute('aria-label', 'Copy command: ' + text);
    el.setAttribute('title', 'Click to copy');
    function copy(e) {
      if (e) { e.stopPropagation(); e.preventDefault(); }
      var done = function () {
        el.classList.add('copied');
        window.setTimeout(function () { el.classList.remove('copied'); }, 1200);
      };
      if (navigator.clipboard && navigator.clipboard.writeText) {
        navigator.clipboard.writeText(text).then(done).catch(select);
      } else {
        select();
      }
    }
    function select() {
      var r = document.createRange(); r.selectNodeContents(el);
      var s = window.getSelection(); s.removeAllRanges(); s.addRange(r);
      el.classList.add('copied');
      window.setTimeout(function () { el.classList.remove('copied'); }, 1200);
    }
    el.addEventListener('click', copy);
    el.addEventListener('keydown', function (e) {
      if (e.key === 'Enter' || e.key === ' ') copy(e);
    });
  });
})();

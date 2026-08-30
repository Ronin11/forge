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

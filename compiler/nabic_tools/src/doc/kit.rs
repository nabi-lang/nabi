//! The files every doc output carries: the web kit's stylesheet, theme
//! and search-shortcut scripts and fonts, and the docs site's API page
//! styles, from the checked-in copies in `kit/` (`just check-kit` keeps
//! them equal to the web kit and nabi-lang.org), plus the mark and the
//! search script, which reads `search-index.js` so pages work from
//! `file://`, and `doc.css`, the header that only `nabi doc` draws.

/// (path under the doc root, contents).
pub(crate) const FILES: &[(&str, &[u8])] = &[
    ("nabi.css", include_bytes!("../../kit/nabi.css")),
    ("style.css", include_bytes!("../../kit/style.css")),
    ("theme.js", include_bytes!("../../kit/theme.js")),
    ("keys.js", include_bytes!("../../kit/keys.js")),
    (
        "fonts/Geist-Variable.woff2",
        include_bytes!("../../kit/fonts/Geist-Variable.woff2"),
    ),
    (
        "fonts/GeistMono-Variable.woff2",
        include_bytes!("../../kit/fonts/GeistMono-Variable.woff2"),
    ),
    (
        "fonts/LICENSE-Geist.txt",
        include_bytes!("../../kit/fonts/LICENSE-Geist.txt"),
    ),
    (
        "favicon.svg",
        include_bytes!("../../../../assets/icons/web/favicon.svg"),
    ),
    ("search.js", SEARCH_JS.as_bytes()),
    ("doc.css", DOC_CSS.as_bytes()),
];

const WORDMARK: &str = include_str!("../../../../assets/wordmark/nabi-horizontal-light.svg");

/// The wordmark inline, its letters in `--wm` as the sites draw it.
pub(crate) fn wordmark() -> String {
    let svg = WORDMARK.trim();
    let at = svg.find('>').unwrap_or(0);
    let head = svg[..at]
        .replace(" role=\"img\"", "")
        .replace(" aria-label=\"Nabi\"", "");
    let body = svg[at..]
        .replace("<title>Nabi</title>", "")
        .replace("fill=\"#1F1B1E\"", "class=\"wm\"");
    format!("{head} role=\"img\" aria-label=\"Nabi\" height=\"28\"{body}")
}

/// The header's title: the mark, the package name and version. The
/// sites keep their own headers in style.css.
const DOC_CSS: &str = r"/* nabi doc's header, on top of nabi.css and style.css */
.doc .hdr .in { gap: 16px; }
.doc .hdr .tools { flex: none; }
.pkg-t { display: flex; align-items: center; gap: 9px; min-width: 0; }
.pkg-t .brand { flex: none; }
.pkg-t .mk { display: block; flex: none; }
/* centred alone, Geist Mono's line box puts the baseline .5px above
   the brand's '/': drop the name and version onto that baseline, their
   caps then on the header's centre line (the mark's viewBox is tight) */
.pkg-id { position: relative; top: .5px; display: flex; align-items: baseline; gap: 8px; min-width: 0; }
.pkg-t .pkg-n { min-width: 0; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; color: var(--ink); font-family: var(--mono); font-size: 16px; font-weight: 600; letter-spacing: -0.02em; line-height: 23px; }
.pkg-t a.pkg-n:hover { color: var(--ink); text-decoration: underline; text-decoration-color: var(--line-2); text-underline-offset: 4px; }
.pkg-t .pkg-v { flex: none; color: var(--ink-3); font-family: var(--mono); font-size: 12px; line-height: 16px; }
.hdr .pkg-t .brand .sl { display: inline; }
@media (max-width: 600px) {
  .doc .hdr:not(.nav-compact) .search { width: 160px; }
}
/* phones: the butterfly alone, no version; search.js folds the search
   into its icon once the name is cut short */
@media (max-width: 480px) {
  .doc .hdr .in { gap: 12px; }
  .pkg-t .mk { width: 26px; overflow: hidden; }
  .pkg-t .pkg-v { display: none; }
}
";

const SEARCH_JS: &str = r#"(function () {
  var root = document.body.getAttribute('data-root') || '';
  var q = document.getElementById('q');
  var box = document.getElementById('results');
  var sbox = q && q.closest('.search');
  var status = sbox && sbox.querySelector('[role=status]');
  var hdr = document.querySelector('.hdr.top');
  var sel = -1;
  function esc(s) {
    return s.replace(/[&<>"]/g, function (c) {
      return {'&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;'}[c];
    });
  }
  function mark(s, w) {
    var i = s.toLowerCase().indexOf(w);
    if (!w || i < 0) return esc(s);
    return esc(s.slice(0, i)) + '<mark>' + esc(s.slice(i, i + w.length)) + '</mark>' + esc(s.slice(i + w.length));
  }
  function show(on) {
    box.hidden = !on;
    q.setAttribute('aria-expanded', on ? 'true' : 'false');
    if (!on) { sel = -1; q.removeAttribute('aria-activedescendant'); }
  }
  function pick(k) {
    var opts = box.querySelectorAll('[role=option]');
    if (!opts.length) return;
    sel = (k + opts.length) % opts.length;
    for (var i = 0; i < opts.length; i++) opts[i].setAttribute('aria-selected', i === sel ? 'true' : 'false');
    q.setAttribute('aria-activedescendant', opts[sel].id);
    opts[sel].scrollIntoView({block: 'nearest'});
  }
  function search() {
    var w = q.value.trim().toLowerCase();
    if (!w) { box.innerHTML = ''; show(false); return; }
    var hits = (window.SEARCH_INDEX || []).filter(function (e) { return e.p.toLowerCase().indexOf(w) >= 0; });
    hits.sort(function (a, b) {
      var an = a.p.split('::').pop().toLowerCase(), bn = b.p.split('::').pop().toLowerCase();
      return (an !== w) - (bn !== w) || (an.indexOf(w) !== 0) - (bn.indexOf(w) !== 0) || a.p.length - b.p.length;
    });
    var shown = hits.slice(0, 50);
    box.innerHTML = shown.map(function (e, k) {
      return '<li role="none"><a id="hit-' + k + '" role="option" tabindex="-1" aria-selected="false" href="' + esc(root + e.u) + '">' +
        '<span class="src">' + esc(e.k) + '</span><span class="hit-t">' + mark(e.p, w) + '</span>' +
        (e.d ? '<small class="hit-x">' + esc(e.d) + '</small>' : '') + '</a></li>';
    }).join('') + (shown.length ? '' : '<li role="none" class="empty">No results</li>');
    if (status) status.textContent = hits.length === 1 ? '1 result' : hits.length + ' results';
    sel = -1;
    show(true);
  }
  if (q && box) {
    q.addEventListener('input', search);
    q.addEventListener('focus', function () { if (q.value.trim()) search(); });
    q.addEventListener('keydown', function (ev) {
      if (ev.key === 'ArrowDown') { ev.preventDefault(); pick(sel + 1); }
      else if (ev.key === 'ArrowUp') { ev.preventDefault(); pick(sel - 1); }
      else if (ev.key === 'Enter') {
        var opts = box.querySelectorAll('[role=option]');
        var a = opts[sel < 0 ? 0 : sel];
        if (a) { ev.preventDefault(); location.href = a.href; }
      } else if (ev.key === 'Escape') {
        show(false);
        if (sbox && sbox.classList.contains('open') && hdr && hdr.classList.contains('nav-compact')) {
          sbox.classList.remove('open');
          sbox.querySelector('.s-open').focus();
        } else q.blur();
      }
    });
    document.addEventListener('click', function (ev) {
      if (!ev.target.closest('.search')) { show(false); if (sbox) sbox.classList.remove('open'); }
    });
    var so = sbox && sbox.querySelector('.s-open');
    if (so) so.addEventListener('click', function () { sbox.classList.add('open'); q.focus(); });
    if (sbox) sbox.addEventListener('click', function () { sbox.classList.add('open'); });
  }
  // the package name gives way first; once it is cut below a few
  // letters the search folds into its icon (style.css .nav-compact)
  var name = document.querySelector('.pkg-n');
  if (hdr && name && so) {
    var fold = function () {
      hdr.classList.remove('nav-compact');
      var tight = name.clientWidth < Math.min(name.scrollWidth, 64);
      hdr.classList.toggle('nav-compact', tight);
      so.tabIndex = tight ? 0 : -1;
      if (tight) so.removeAttribute('aria-hidden');
      else { so.setAttribute('aria-hidden', 'true'); sbox.classList.remove('open'); }
    };
    fold();
    addEventListener('resize', fold);
  }
  var side = document.querySelector('.side');
  var toggle = document.querySelector('.side-toggle');
  if (side && toggle) {
    var scrim = document.createElement('div');
    scrim.className = 'scrim appear';
    scrim.hidden = true;
    document.body.appendChild(scrim);
    var here = side.querySelector('[aria-current=page]');
    var set = function (open) {
      document.body.classList.toggle('side-open', open);
      document.documentElement.classList.toggle('lock', open);
      toggle.setAttribute('aria-expanded', open ? 'true' : 'false');
      scrim.hidden = !open;
      if (open) (here || side.querySelector('a, button')).focus({preventScroll: true});
      else toggle.focus({preventScroll: true});
    };
    toggle.addEventListener('click', function () { set(true); });
    scrim.addEventListener('click', function () { set(false); });
    var close = side.querySelector('.side-close');
    if (close) close.addEventListener('click', function () { set(false); });
    side.addEventListener('keydown', function (ev) {
      if (ev.key === 'Escape' && document.body.classList.contains('side-open')) set(false);
    });
    side.addEventListener('click', function (ev) {
      if (ev.target.closest('a') && document.body.classList.contains('side-open')) set(false);
    });
    if (here && side.scrollHeight > side.clientHeight) side.scrollTop = here.offsetTop - side.clientHeight / 2;
  }
  var tocM = document.querySelector('.toc-m');
  if (tocM) tocM.addEventListener('click', function (ev) { if (ev.target.closest('a')) tocM.open = false; });
})();
"#;

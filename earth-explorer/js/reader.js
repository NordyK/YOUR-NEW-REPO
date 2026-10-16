// Article reader: a full Wikipedia article set as an encyclopedia entry, with a margin
// "apparatus" that ties the text to place and time (locator, key figures, dates, related, sources).
import { icon } from './icons.js';

const DROP_SECTIONS = /^(references|notes|citations|footnotes|bibliography|sources|further reading|external links|see also|works cited|general and cited sources|notes and references|explanatory notes)$/i;
const DROP_SELECTORS = [
  'style', 'script', 'link', 'meta', '.mw-editsection', '.navbox', '.navbox-styles', '.vertical-navbox', '.sidebar', '.metadata', '.ambox',
  '.mbox-small', '.box-More_citations_needed', '.infobox', '.mw-empty-elt', '.shortdescription', '.noprint', '.portalbox', '.sistersitebox',
  '.mw-references-wrap', 'sup.reference', 'sup.noprint', '.reference', 'table.sidebar', '.hatnote.navigation-not-searchable', '.mw-kartographer-maplink',
];

export function createReader(ctx) {
  const { esc, fmtYear, getJSON, flyTo, setYear, openByKey } = ctx;
  const root = document.getElementById('reader');
  const cache = new Map();
  const history = [];
  let current = null;

  // Read aloud with the browser's speech synthesis, paragraph by paragraph (keeps long articles responsive).
  function stopSpeech() { if ('speechSynthesis' in window) speechSynthesis.cancel(); root.querySelector('[data-r="listen"]')?.setAttribute('aria-pressed', 'false'); const l = root.querySelector('[data-r="listen"] span'); if (l) l.textContent = 'Listen'; }
  function toggleSpeech(btn) {
    if (speechSynthesis.speaking) return stopSpeech();
    const parts = [...root.querySelectorAll('.rd-col h1, .rd-col .rd-standfirst, .rd-body h2, .rd-body h3, .rd-body p, .rd-body li')].map((n) => n.textContent.trim()).filter((t) => t.length > 1);
    if (!parts.length) return;
    btn.setAttribute('aria-pressed', 'true');
    btn.querySelector('span').textContent = 'Stop';
    parts.forEach((t, i) => {
      const u = new SpeechSynthesisUtterance(t);
      u.lang = 'en-GB'; u.rate = 1;
      if (i === parts.length - 1) u.onend = stopSpeech;
      speechSynthesis.speak(u);
    });
  }
  function close() {
    stopSpeech();
    root.classList.add('hidden');
    document.body.classList.remove('reading');
    history.length = 0;
    ctx.onReaderClose?.();
  }
  root.addEventListener('click', (e) => {
    const a = e.target.closest('a[data-wiki]');
    if (a) { e.preventDefault(); history.push(current); open({ title: a.dataset.wiki }); return; }
    const b = e.target.closest('[data-r]');
    if (!b) return;
    const act = b.dataset.r;
    if (act === 'close') close();
    else if (act === 'back') { const prev = history.pop(); if (prev) open(prev, true); }
    else if (act === 'globe') { const [lat, lng] = b.dataset.ll.split(',').map(Number); close(); flyTo(lat, lng, 0.6); }
    else if (act === 'year') { setYear(+b.dataset.year, true); toastYear(+b.dataset.year); }
    else if (act === 'item') { close(); openByKey(b.dataset.key); }
    else if (act === 'retry') open(current, true);
    else if (act === 'listen') toggleSpeech(b);
    else if (act === 'key') { e.preventDefault(); const t = root.querySelector('#ap-' + b.dataset.k)?.closest('.ap'); t?.scrollIntoView({ block: 'nearest', behavior: 'smooth' }); t?.classList.add('flash'); setTimeout(() => t?.classList.remove('flash'), 1200); }
    else if (act === 'toc') { const t = root.querySelector('#' + CSS.escape(b.dataset.id)); t?.scrollIntoView({ behavior: matchMedia('(prefers-reduced-motion: reduce)').matches ? 'auto' : 'smooth', block: 'start' }); }
  });
  const toastYear = (y) => ctx.toast?.(`Time machine set to ${fmtYear(y)}. Close the reader to see the globe in that year.`);
  document.addEventListener('keydown', (e) => { if (e.key === 'Escape' && !root.classList.contains('hidden') && document.getElementById('lightbox').classList.contains('hidden')) close(); });

  async function fetchArticle(title) {
    if (!cache.has(title)) {
      cache.set(title, (async () => {
        const enc = encodeURIComponent(title.replace(/ /g, '_'));
        const [html, sum] = await Promise.all([
          (async () => {
            for (let i = 0; i < 3; i++) {
              try {
                const r = await fetch(`https://en.wikipedia.org/api/rest_v1/page/html/${enc}?redirect=true`);
                if (r.ok) return r.text();
                if (r.status !== 429 && r.status !== 503) return null;
                await new Promise((res) => setTimeout(res, Math.max(1000 * 2 ** i, Math.min(10, +r.headers.get('retry-after') || 0) * 1000)));
              } catch { await new Promise((res) => setTimeout(res, 1000 * 2 ** i)); }
            }
            return null;
          })(),
          getJSON(`https://en.wikipedia.org/api/rest_v1/page/summary/${enc}`),
        ]);
        if (!html) { cache.delete(title); return null; }
        return { html, sum };
      })());
    }
    return cache.get(title);
  }

  /** Clean Parsoid HTML into a readable article body and a table of contents. */
  function clean(html) {
    const doc = new DOMParser().parseFromString(html, 'text/html');
    const body = doc.body;
    DROP_SELECTORS.forEach((s) => body.querySelectorAll(s).forEach((n) => n.remove()));
    body.querySelectorAll('section').forEach((sec) => {
      const h = sec.querySelector(':scope > h2, :scope > h3, :scope > h4');
      if (h && DROP_SECTIONS.test(h.textContent.trim())) sec.remove();
    });
    // links: internal → reader, external → new tab
    body.querySelectorAll('a').forEach((a) => {
      const href = a.getAttribute('href') || '';
      const rel = a.getAttribute('rel') || '';
      if (rel.includes('mw:WikiLink') && href.startsWith('./') && !/^\.\/(File|Special|Help|Wikipedia|Template|Category|Portal|Talk):/.test(href)) {
        const t = decodeURIComponent(href.slice(2).split('#')[0]).replace(/_/g, ' ');
        a.setAttribute('href', `https://en.wikipedia.org/wiki/${encodeURIComponent(t.replace(/ /g, '_'))}`);
        a.dataset.wiki = t;
      } else if (href.startsWith('./File:')) {
        a.replaceWith(...a.childNodes);
      } else {
        a.setAttribute('target', '_blank');
        a.setAttribute('rel', 'noopener');
      }
    });
    body.querySelectorAll('img').forEach((img) => {
      ['src', 'srcset'].forEach((k) => { const v = img.getAttribute(k); if (v) img.setAttribute(k, v.replace(/(^|\s|,)\/\//g, '$1https://')); });
      img.setAttribute('loading', 'lazy');
      img.removeAttribute('width'); img.removeAttribute('height'); img.removeAttribute('style');
      const res = img.getAttribute('resource');
      if (res) img.dataset.file = decodeURIComponent(res.replace(/^\.\//, ''));
      if (!img.getAttribute('alt')) img.setAttribute('alt', '');
    });
    body.querySelectorAll('[style]').forEach((n) => n.removeAttribute('style'));
    body.querySelectorAll('table').forEach((t) => { const w = doc.createElement('div'); w.className = 'table-wrap'; t.replaceWith(w); w.appendChild(t); });
    // headings get ids for the contents list
    const toc = [];
    body.querySelectorAll('h2, h3').forEach((h, i) => {
      const id = 'sec-' + i;
      h.id = id;
      toc.push({ id, level: h.tagName === 'H2' ? 2 : 3, text: h.textContent.trim() });
    });
    // lead section first paragraph becomes the standfirst
    const lead = body.querySelector('section p:not(:empty)');
    const standfirst = lead ? lead.innerHTML : '';
    lead?.remove();
    return { body: body.innerHTML, toc, standfirst };
  }

  // Index letters key a margin note to the place in the text it annotates; notes with no anchor get none.
  function margin({ item, sum, title, keys = {} }) {
    const parts = [];
    const key = (name) => (keys[name] ? `<span class="key" id="ap-${keys[name]}">${keys[name]}</span>` : '');
    const coords = item?.lat != null ? [item.lat, item.lng] : sum?.coordinates ? [sum.coordinates.lat, sum.coordinates.lon] : null;
    if (coords) {
      parts.push(`<section class="ap"><h3>${key('where')}Where</h3>
        <p class="num">${Math.abs(coords[0]).toFixed(3)}° ${coords[0] >= 0 ? 'N' : 'S'}, ${Math.abs(coords[1]).toFixed(3)}° ${coords[1] >= 0 ? 'E' : 'W'}</p>
        ${item?.place ? `<p>${esc(item.place)}</p>` : ''}
        <button class="btn" data-r="globe" data-ll="${coords[0]},${coords[1]}">${icon('crosshair')}Show on globe</button></section>`);
    }
    if (item?.figures && Object.keys(item.figures).length) {
      parts.push(`<section class="ap"><h3>${key('figures')}Key figures</h3><dl>${Object.entries(item.figures).filter(([, v]) => v != null && v !== '').map(([a, b]) => `<dt>${esc(a)}</dt><dd>${esc(b)}</dd>`).join('')}</dl></section>`);
    }
    if (item?.dates?.length) {
      parts.push(`<section class="ap"><h3>${key('time')}In time</h3>${item.dates.map((d) => `<button class="ap-row" data-r="year" data-year="${d.year}"><span class="num">${esc(fmtYear(d.year))}</span><span>${esc(d.label)}</span></button>`).join('')}<p class="ap-note">Choose a date to set the time machine.</p></section>`);
    }
    if (item?.related?.length) {
      parts.push(`<section class="ap"><h3>Nearby on the globe</h3>${item.related.map((r) => `<button class="ap-row" data-r="item" data-key="${esc(r.key)}"><span>${esc(r.name)}</span><span class="num">${r.km < 1 ? '<1' : Math.round(r.km)} km</span></button>`).join('')}</section>`);
    }
    const src = `https://en.wikipedia.org/wiki/${encodeURIComponent(title.replace(/ /g, '_'))}`;
    parts.push(`<section class="ap"><h3>Sources</h3>
      <p>Text: <a href="${src}" target="_blank" rel="noopener">“${esc(sum?.title || title)}”</a>, Wikipedia contributors, <a href="https://creativecommons.org/licenses/by-sa/4.0/" target="_blank" rel="noopener">CC BY-SA 4.0</a>. <a href="${src}?action=history" target="_blank" rel="noopener">Authors</a>.</p>
      <p>Images: Wikimedia Commons; select an image for its author and licence.</p>
      ${item?.source ? `<p>${esc(item.source)}</p>` : ''}</section>`);
    return parts.join('');
  }

  const commonsFile = (url) => decodeURIComponent(url.match(/\/commons\/(?:thumb\/)?[0-9a-f]\/[0-9a-f]{2}\/([^/]+)/)?.[1] || '');

  /** On narrow screens the margin falls below the article, so place and time also sit under the lead. */
  function inlineApparatus(item, sum) {
    const c = item?.lat != null ? [item.lat, item.lng] : sum?.coordinates ? [sum.coordinates.lat, sum.coordinates.lon] : null;
    if (!c && !item?.dates?.length) return '';
    return `<aside class="rd-inline-ap" aria-label="Place and time">
      ${c ? `<div class="row"><span class="num">${Math.abs(c[0]).toFixed(2)}° ${c[0] >= 0 ? 'N' : 'S'}, ${Math.abs(c[1]).toFixed(2)}° ${c[1] >= 0 ? 'E' : 'W'}</span><button class="btn" data-r="globe" data-ll="${c[0]},${c[1]}">${icon('crosshair')}Show on globe</button></div>` : ''}
      ${(item?.dates || []).slice(0, 3).map((d) => `<button class="ap-row" data-r="year" data-year="${d.year}"><span class="num">${esc(fmtYear(d.year))}</span><span>${esc(d.label)}</span></button>`).join('')}
    </aside>`;
  }

  /** Put a superscript key at the first mention in the text of each annotated thing; return the letters used. */
  function anchorKeys(col, item) {
    if (!item) return {};
    const scope = [col.querySelector('.rd-standfirst'), col.querySelector('.rd-body')].filter(Boolean);
    const wants = [];
    if (item.place) wants.push(['where', item.place.split(',').map((x) => x.trim()).filter((x) => x.length > 2)]);
    const fig = Object.values(item.figures || {}).map((v) => String(v ?? '').match(/\d[\d,.]*\d/)?.[0]).filter(Boolean);
    if (fig.length) wants.push(['figures', fig]);
    if (item.dates?.length) wants.push(['time', item.dates.map((d) => String(Math.abs(d.year)))]);
    const keys = {};
    let n = 0;
    for (const [name, terms] of wants) {
      const hit = findText(scope, terms);
      if (!hit) continue;
      const letter = String.fromCharCode(97 + n++);
      keys[name] = letter;
      const { node, end } = hit;
      const after = node.splitText(end);
      const sup = document.createElement('sup');
      sup.className = 'key-ref';
      sup.innerHTML = `<a href="#ap-${letter}" data-r="key" data-k="${letter}" aria-label="Note ${letter}">${letter}</a>`;
      after.parentNode.insertBefore(sup, after);
    }
    return keys;
  }
  function findText(scope, terms) {
    for (const el of scope) {
      const walk = document.createTreeWalker(el, NodeFilter.SHOW_TEXT, { acceptNode: (t) => (t.parentElement.closest('h2,h3,h4,figcaption,table,sup') ? NodeFilter.FILTER_REJECT : NodeFilter.FILTER_ACCEPT) });
      for (let t = walk.nextNode(); t; t = walk.nextNode()) {
        for (const term of terms) {
          const m = new RegExp(`(^|[^\\w])${term.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}(?![\\w])`).exec(t.data);
          if (m) return { node: t, end: m.index + m[0].length };
        }
      }
    }
    return null;
  }

  async function open(opts, isBack = false) {
    stopSpeech();
    current = opts;
    const { title, item } = opts;
    root.classList.remove('hidden');
    document.body.classList.add('reading');
    const yr = item?.dates?.length ? fmtYear(item.dates[0].year) : '';
    const wiki = `https://en.wikipedia.org/wiki/${encodeURIComponent(title.replace(/ /g, '_'))}`;
    root.innerHTML = `<header class="rd-head">
        <div class="rd-crumb">${history.length ? `<button class="btn" data-r="back" aria-label="Back">${icon('chevron-left')}<span class="lbl">Back</span></button>` : ''}<span>${esc(item?.kind || 'Article')}</span>${item?.place ? `<span class="sep">·</span><span class="place">${esc(item.place)}</span>` : ''}${yr ? `<span class="sep">·</span><span class="num">${esc(yr)}</span>` : ''}</div>
        <div class="rd-tools"><span class="btn rd-toc">${icon('list')}<span class="lbl">Contents</span><select id="rd-toc" aria-label="Contents: jump to section"><option value="">Contents</option></select></span>
        ${'speechSynthesis' in window ? `<button class="btn" data-r="listen" aria-pressed="false" aria-label="Listen">${icon('radio')}<span class="lbl">Listen</span></button>` : ''}
        <a class="btn" target="_blank" rel="noopener" href="${wiki}" aria-label="Open on Wikipedia">${icon('external-link')}<span class="lbl">Wikipedia</span></a>
        <button class="btn primary" data-r="close" aria-label="Back to globe">${icon('globe')}<span class="lbl">Back to globe</span><span class="lbl-short" aria-hidden="true">Globe</span></button></div>
      </header>
      <div class="rd-scroll"><article class="rd-page"><div class="rd-col"><p class="rd-loading">Loading the article…</p></div><aside class="rd-margin" aria-label="Notes">${margin({ item, sum: null, title })}</aside></article></div>`;
    root.querySelector('.rd-scroll').focus?.();
    const data = await fetchArticle(title);
    if (current !== opts) return;
    const col = root.querySelector('.rd-col');
    if (!data) {
      col.innerHTML = `<h1>${esc(title)}</h1><p class="rd-error">This article could not be loaded. Wikipedia may be busy or you may be offline. <a href="https://en.wikipedia.org/wiki/${encodeURIComponent(title.replace(/ /g, '_'))}" target="_blank" rel="noopener">Open it on Wikipedia</a> or <button class="btn" data-r="retry">${icon('refresh-cw')}try again</button></p>`;
      return;
    }
    const { body, toc, standfirst } = clean(data.html);
    const sum = data.sum;
    const lead = sum?.originalimage?.source || sum?.thumbnail?.source;
    col.innerHTML = `<h1>${esc(sum?.title || title)}</h1>
      ${sum?.description ? `<p class="rd-desc">${esc(sum.description)}</p>` : ''}
      ${standfirst ? `<p class="rd-standfirst">${standfirst}</p>` : ''}
      ${lead ? `<figure class="rd-lead"><img src="${esc(lead.replace(/\/(\d+)px-/, '/960px-'))}" alt="${esc(sum.title)}" data-fallback="${esc(sum?.thumbnail?.source || '')}">${commonsFile(lead) ? `<figcaption>Wikimedia Commons · <a href="https://commons.wikimedia.org/wiki/File:${encodeURIComponent(commonsFile(lead))}" target="_blank" rel="noopener">Author and licence</a></figcaption>` : ''}</figure>` : ''}
      ${inlineApparatus(item, sum)}
      <div class="rd-body">${body}</div>
      <footer class="rd-foot">Text from Wikipedia, licensed <a href="https://creativecommons.org/licenses/by-sa/4.0/" target="_blank" rel="noopener">CC BY-SA 4.0</a>. Read the <a href="https://en.wikipedia.org/wiki/${encodeURIComponent((sum?.title || title).replace(/ /g, '_'))}" target="_blank" rel="noopener">full article with references on Wikipedia</a>.</footer>`;
    const leadImg = col.querySelector('.rd-lead img');
    if (leadImg) leadImg.onerror = () => { if (leadImg.dataset.fallback && leadImg.src !== leadImg.dataset.fallback) leadImg.src = leadImg.dataset.fallback; };
    root.querySelector('.rd-margin').innerHTML = margin({ item, sum, title, keys: anchorKeys(col, item) });
    const sel = root.querySelector('#rd-toc');
    sel.innerHTML = `<option value="">Contents</option>` + toc.map((t) => `<option value="${t.id}">${t.level === 3 ? '— ' : ''}${esc(t.text)}</option>`).join('');
    sel.onchange = () => { if (sel.value) root.querySelector('#' + CSS.escape(sel.value))?.scrollIntoView({ block: 'start' }); sel.value = ''; };
    // images inside the article open in the shared lightbox
    const imgs = [...col.querySelectorAll('.rd-body figure img, .rd-body img')].filter((im) => (im.naturalWidth || 200) >= 80 && !/\.svg/i.test(im.src));
    const list = imgs.map((im) => ({ full: (im.getAttribute('src') || '').replace(/\/(\d+)px-/, '/960px-'), fallback: im.getAttribute('src'), caption: im.closest('figure')?.querySelector('figcaption')?.textContent?.trim() || '', file: im.dataset.file || null }));
    imgs.forEach((im, i) => { im.style.cursor = 'zoom-in'; im.addEventListener('click', () => ctx.openLightbox(list, i, sum?.title || title)); });
    root.querySelector('.rd-scroll').scrollTop = 0;
  }

  return { open, close, isOpen: () => !root.classList.contains('hidden') };
}

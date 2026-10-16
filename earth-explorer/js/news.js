// News and chronicles: real reporting tied to place and time.
//  • Day by day: Wikipedia "Portal:Current events" daily pages: news summaries citing their outlets (2002 → today).
//  • Year chronicle: the Events section of Wikipedia's year article ("1912", "490 BC", or decade/century fallbacks).
//  • Country chronicle: "1912 in France"-style articles.
//  • Newspapers of the period: Library of Congress digitised newspapers (1770–1963), when available.
// Every item is geolocated through the coordinates of the articles it links to, so it can be pinned on the globe.
import { icon } from './icons.js';

const MONTHS = ['January', 'February', 'March', 'April', 'May', 'June', 'July', 'August', 'September', 'October', 'November', 'December'];
const DATE_RX = new RegExp(`^((?:${MONTHS.join('|')})\\s+\\d{1,2}(?:\\s*[–-]\\s*(?:(?:${MONTHS.join('|')})\\s+)?\\d{1,2})?|(?:${MONTHS.join('|')}))\\s*[–—:-]\\s*`);
// the same date prefix as it appears in our rich-text output (the date is usually a link)
const DATE_HTML_RX = new RegExp(`^(?:<a(?: [^>]*)?>)?\\s*(?:${MONTHS.join('|')})(?:\\s+\\d{1,2})?\\s*(?:<\\/a>)?(?:\\s*[–-]\\s*(?:<a(?: [^>]*)?>)?\\s*(?:(?:${MONTHS.join('|')})\\s+)?\\d{1,2}\\s*(?:<\\/a>)?)?\\s*[–—:-]\\s*`);
const SKIP_LINK = /^(\d{1,4}( BC)?|\d{1,4}s( BC)?|(January|February|March|April|May|June|July|August|September|October|November|December)( \d{1,2})?|\d+(st|nd|rd|th) (century|millennium)( BC)?|List of .*|Portal:.*|Category:.*|Template:.*|Help:.*|File:.*)$/;

export function createNews(ctx) {
  const { esc, getJSON, fmtYear } = ctx;
  const pageCache = new Map(), coordCache = new Map();
  const st = { mode: 'day', country: '', items: [], status: '', source: null, loadedFor: '', date: null, paperDate: null, paperQ: '' };

  const enc = (t) => encodeURIComponent(t.replace(/ /g, '_'));
  async function page(title) {
    if (!pageCache.has(title)) {
      pageCache.set(title, (async () => {
        for (let i = 0; i < 3; i++) {
          try {
            const r = await fetch(`https://en.wikipedia.org/api/rest_v1/page/html/${enc(title)}?redirect=true`);
            if (r.ok) return { title, html: await r.text() };
            if (r.status === 404) return null;
            if (r.status !== 429 && r.status !== 503) break;
          } catch { /* retry */ }
          await new Promise((res) => setTimeout(res, 900 * 2 ** i));
        }
        pageCache.delete(title);
        return { title, error: true };
      })());
    }
    return pageCache.get(title);
  }

  /** Batch-resolve article titles to coordinates (Wikipedia GeoData). */
  async function coordinates(titles) {
    const need = [...new Set(titles)].filter((t) => !coordCache.has(t));
    for (let i = 0; i < need.length; i += 50) {
      const batch = need.slice(i, i + 50);
      const j = await getJSON(`https://en.wikipedia.org/w/api.php?action=query&prop=coordinates&coprimary=primary&colimit=max&redirects=1&format=json&origin=*&titles=${batch.map((t) => encodeURIComponent(t)).join('|')}`);
      const redir = {};
      (j?.query?.normalized || []).forEach((n) => (redir[n.to] = n.from));
      (j?.query?.redirects || []).forEach((n) => (redir[n.to] = redir[n.from] || n.from));
      batch.forEach((t) => coordCache.set(t, null));
      Object.values(j?.query?.pages || {}).forEach((p) => {
        const c = p.coordinates?.[0];
        if (!c) return;
        const orig = redir[p.title] || p.title;
        coordCache.set(orig, { lat: c.lat, lng: c.lon });
        coordCache.set(p.title, { lat: c.lat, lng: c.lon });
      });
    }
    return (t) => coordCache.get(t) || null;
  }

  const linkTitles = (el) => [...el.querySelectorAll('a[rel~="mw:WikiLink"]')]
    .map((a) => decodeURIComponent((a.getAttribute('href') || '').replace(/^\.\//, '').split('#')[0]).replace(/_/g, ' '))
    .filter((t) => t && !SKIP_LINK.test(t));
  const stripSubs = (el) => { const c = el.cloneNode(true); c.querySelectorAll('ul, ol').forEach((u) => u.remove()); return c; };
  const plain = (el) => el.textContent.replace(/\s+/g, ' ').replace(/\[\d+\]/g, '').trim();
  function richText(el) {
    // keep internal links (open in the reader) and external source links; drop everything else
    const clone = el.cloneNode(true);
    clone.querySelectorAll('ul, ol, sup, style, .mw-ref').forEach((n) => n.remove());
    let out = '';
    clone.childNodes.forEach(function walk(n) {
      if (n.nodeType === 3) { out += esc(n.textContent); return; }
      if (n.nodeType !== 1) return;
      if (n.tagName === 'A') {
        const href = n.getAttribute('href') || '';
        const rel = n.getAttribute('rel') || '';
        if (rel.includes('mw:WikiLink') && href.startsWith('./')) {
          const t = decodeURIComponent(href.slice(2).split('#')[0]).replace(/_/g, ' ');
          out += `<a href="https://en.wikipedia.org/wiki/${enc(t)}" data-wiki="${esc(t)}">${esc(n.textContent)}</a>`;
        } else if (/^https?:/.test(href)) out += `<a href="${esc(href)}" target="_blank" rel="noopener" class="src">${esc(n.textContent)}</a>`;
        else out += esc(n.textContent);
        return;
      }
      n.childNodes.forEach(walk);
    });
    return out.replace(/\s+/g, ' ').trim();
  }

  /** Parse an "Events" section (year and "YEAR in COUNTRY" articles). */
  function parseEvents(html, year) {
    const doc = new DOMParser().parseFromString(html, 'text/html');
    const sections = [...doc.querySelectorAll('section')].filter((s) => { const h = s.querySelector(':scope > h2, :scope > div > h2'); return h && /^(events|events and trends|ongoing|chronology)/i.test(h.textContent.trim()); });
    const root = sections[0] || doc.body;
    const out = [];
    let month = '';
    root.querySelectorAll('h3, h4, li').forEach((n) => {
      if (n.tagName !== 'LI') { const t = n.textContent.trim(); if (MONTHS.includes(t.split(' ')[0])) month = t; return; }
      if (n.closest('.navbox, .reflist, .references, table')) return;
      const parentLi = n.parentElement?.closest('li');
      const own = stripSubs(n);
      const ownText = plain(own);
      if (!ownText || ownText.length < 12) return;
      const parentDate = parentLi ? plain(stripSubs(parentLi)).match(DATE_RX)?.[1] : null;
      const m = ownText.match(DATE_RX);
      // a parent li that only carries a date and has children: its children are the events
      if (m && n.querySelector(':scope > ul') && ownText.replace(DATE_RX, '').trim().length < 3) return;
      const date = m?.[1] || parentDate || month || '';
      const text = richText(n).replace(DATE_HTML_RX, '').replace(DATE_RX, '');
      const titles = linkTitles(own);
      out.push({ date, text, titles, year });
    });
    return out.filter((e) => e.text.length > 10).slice(0, 160);
  }

  /** Parse a Portal:Current events daily page: category headings and cited news items. */
  function parseCurrentEvents(html, dateLabel) {
    const doc = new DOMParser().parseFromString(html, 'text/html');
    const content = doc.querySelector('.current-events-content') || doc.body;
    const out = [];
    let cat = '';
    const walk = (node) => {
      node.childNodes.forEach((n) => {
        if (n.nodeType !== 1) return;
        if ((n.tagName === 'P' || n.tagName === 'DIV') && n.querySelector('b') && plain(n).length < 60 && !n.querySelector('li')) { cat = plain(n); return; }
        if (n.tagName === 'UL') {
          n.querySelectorAll(':scope > li').forEach(function li(item, _, __, topic = '') {
            const sub = item.querySelector(':scope > ul');
            const own = stripSubs(item);
            if (sub) {
              const t = plain(own);
              sub.querySelectorAll(':scope > li').forEach((s) => li(s, 0, 0, [topic, t].filter(Boolean).join(' · ')));
              return;
            }
            const text = richText(item);
            if (plain(own).length < 15) return;
            const sources = [...own.querySelectorAll('a')].filter((a) => /^https?:/.test(a.getAttribute('href') || '') && !(a.getAttribute('rel') || '').includes('WikiLink')).map((a) => ({ url: a.getAttribute('href'), name: a.textContent.replace(/[()]/g, '').trim() }));
            out.push({ date: dateLabel, category: cat, topic, text, titles: linkTitles(own), sources });
          });
          return;
        }
        walk(n);
      });
    };
    walk(content);
    return out;
  }

  // Candidate articles for a year, finest first (year → decade → century → millennium).
  function yearPages(y) {
    if (y >= 1) return [String(y)];
    const b = -y, list = [`${b} BC`];
    list.push(`${Math.floor(b / 10) * 10}s BC`);
    const c = Math.ceil(b / 100), sfx = (n) => (n % 10 === 1 && n % 100 !== 11 ? 'st' : n % 10 === 2 && n % 100 !== 12 ? 'nd' : n % 10 === 3 && n % 100 !== 13 ? 'rd' : 'th');
    list.push(`${c}${sfx(c)} century BC`);
    const mil = Math.ceil(b / 1000);
    list.push(`${mil}${sfx(mil)} millennium BC`);
    return list;
  }
  const isoDay = (d) => `${d.getUTCFullYear()}_${MONTHS[d.getUTCMonth()]}_${d.getUTCDate()}`;

  async function geocode(items) {
    const get = await coordinates(items.flatMap((e) => e.titles.slice(0, 6)));
    items.forEach((e) => {
      for (const t of e.titles.slice(0, 6)) { const c = get(t); if (c) { e.lat = c.lat; e.lng = c.lng; e.place = t; break; } }
      e.article = e.titles[0] || null;
    });
    return items;
  }

  async function load() {
    const year = ctx.state.year;
    const key = `${st.mode}|${st.mode === 'day' ? st.date : st.mode === 'papers' ? st.paperDate + '|' + st.paperQ : year}|${st.country}`;
    if (st.loadedFor === key) return;
    st.loadedFor = key;
    st.items = []; st.status = 'loading'; st.source = null;
    ctx.refresh();
    try {
      if (st.mode === 'papers') {
        // Library of Congress, Chronicling America: digitised US newspaper pages, 1770–1963
        const d = st.paperDate;
        const j = await getJSON(`https://www.loc.gov/collections/chronicling-america/?fo=json&dates=${d}/${d}&c=80&at=results${st.paperQ ? '&q=' + encodeURIComponent(st.paperQ) : ''}`);
        if (st.loadedFor !== key) return;
        if (!j) throw new Error('busy');
        const title = (t) => t.replace(/\b\w/g, (c) => c.toUpperCase());
        st.items = (j.results || []).filter((x) => x.date === d && x.image_url?.length).map((x) => ({
          kind: 'paper', date: d, paper: x.title.replace(/,\s*[A-Z][a-z]+ \d{1,2}, \d{4}$/, ''),
          city: title(x.location_city?.[0] || ''), state: title(x.location_state?.[0] || ''),
          thumb: x.image_url[0].split('#')[0], big: x.image_url[0].split('#')[0].replace(/pct:[\d.]+/, 'pct:25'), url: x.url,
          text: esc(x.title), titles: x.location_city?.[0] && x.location_state?.[0] ? [`${title(x.location_city[0])}, ${title(x.location_state[0])}`] : [],
        }));
        st.source = { title: 'Chronicling America', url: `https://www.loc.gov/collections/chronicling-america/?dates=${d}/${d}`, label: 'Library of Congress, Chronicling America' };
      } else if (st.mode === 'day') {
        const d = new Date(st.date + 'T12:00:00Z');
        const title = `Portal:Current events/${isoDay(d).replace(/_/g, ' ')}`;
        const p = await page(title);
        if (st.loadedFor !== key) return;
        if (p?.error) throw new Error('busy');
        if (!p) { st.status = 'empty'; st.source = { title, url: `https://en.wikipedia.org/wiki/${enc(title)}` }; ctx.refresh(); return; }
        const label = `${MONTHS[d.getUTCMonth()]} ${d.getUTCDate()}, ${d.getUTCFullYear()}`;
        st.items = parseCurrentEvents(p.html, label);
        st.source = { title, url: `https://en.wikipedia.org/wiki/${enc(title)}`, label: 'Wikipedia Current events portal' };
      } else {
        const THE = /^(United States|United Kingdom|Netherlands|Philippines|Bahamas|Gambia|Czech Republic|Dominican Republic|Central African Republic|Democratic Republic of the Congo|Republic of the Congo|Maldives|Comoros|United Arab Emirates|Solomon Islands|Marshall Islands)$/;
        const cname = st.country && THE.test(st.country) ? 'the ' + st.country : st.country;
        const candidates = st.country ? [`${year} in ${cname}`] : yearPages(year);
        let p = null;
        for (const t of candidates) { p = await page(t); if (p) break; }
        if (st.loadedFor !== key) return;
        if (p?.error) throw new Error('busy');
        if (!p) { st.status = 'empty'; st.source = { title: candidates[0], url: `https://en.wikipedia.org/wiki/${enc(candidates[0])}` }; ctx.refresh(); return; }
        st.items = parseEvents(p.html, year);
        st.source = { title: p.title, url: `https://en.wikipedia.org/wiki/${enc(p.title)}`, label: `Wikipedia, “${p.title}”` };
      }
      if (st.loadedFor !== key) return;
      st.status = st.items.length ? 'ok' : 'empty';
      ctx.refresh(); // show the stories now; pins follow once locations resolve
      st.locating = true;
      await geocode(st.items);
      st.locating = false;
      if (st.loadedFor !== key) return;
    } catch (e) {
      console.warn('news failed', e);
      st.status = 'error';
      st.loadedFor = '';
    }
    ctx.refresh();
  }

  function defaultDate() {
    const y = ctx.state.year;
    if (y >= 2026) { const d = new Date(Date.now() - 864e5 * 0); return d.toISOString().slice(0, 10); }
    if (y >= 2002) return `${y}-06-15`;
    return null;
  }

  function renderPanel(el) {
    const y = ctx.state.year;
    if (!st.date || (st.mode === 'day' && !st.date.startsWith(String(Math.max(2002, Math.min(2026, y)))))) st.date = defaultDate();
    if (st.mode === 'day' && y < 2002) st.mode = 'year';
    if (st.mode === 'papers' && (y < 1770 || y > 1963)) st.mode = 'year';
    if (!st.paperDate || +st.paperDate.slice(0, 4) !== y) st.paperDate = y >= 1770 && y <= 1963 ? `${y}-04-16` : null;
    const countries = ctx.countryNames();
    el.innerHTML = `<div class="side-head"><h2>News</h2></div>
      <p class="side-desc">Real reporting for the year on the timeline: daily news citing its outlets (2002 onwards), the chronicle of any year back to the Ice Age, and chronicles of single countries.</p>
      <div class="seg" role="tablist" aria-label="News source">
        <button role="tab" aria-selected="${st.mode === 'day'}" data-nm="day" ${y < 2002 ? 'disabled title="Daily news starts in 2002. Move the timeline later."' : ''}>${icon('newspaper')}Day by day</button>
        <button role="tab" aria-selected="${st.mode === 'year'}" data-nm="year">${icon('calendar-days')}Chronicle</button>
        <button role="tab" aria-selected="${st.mode === 'papers'}" data-nm="papers" ${y < 1770 || y > 1963 ? 'disabled title="Digitised newspapers cover 1770–1963. Move the timeline into that range."' : ''}>${icon('scroll')}Newspapers</button>
      </div>
      ${st.mode === 'papers' ? `<label class="field">Date <input type="date" data-pd value="${esc(st.paperDate || '')}" min="1770-01-01" max="1963-12-31"></label><label class="field">Words on the page (optional) <input type="search" data-pq value="${esc(st.paperQ)}" placeholder="e.g. Titanic, election, earthquake"></label>`
        : st.mode === 'day' ? `<label class="field">Date <input type="date" data-nd value="${esc(st.date || '')}" min="2002-01-01" max="${new Date().toISOString().slice(0, 10)}"></label>`
        : `<label class="field">Place <select data-nc><option value="">The whole world</option>${countries.map((c) => `<option ${c === st.country ? 'selected' : ''}>${esc(c)}</option>`).join('')}</select></label>`}
      <div class="news-head"><b>${st.mode === 'papers' ? `US newspapers of ${esc(st.paperDate ? new Date(st.paperDate + 'T12:00:00Z').toLocaleDateString('en-GB', { day: 'numeric', month: 'long', year: 'numeric', timeZone: 'UTC' }) : '')}` : st.mode === 'day' ? esc(st.date ? new Date(st.date + 'T12:00:00Z').toLocaleDateString('en-GB', { day: 'numeric', month: 'long', year: 'numeric', timeZone: 'UTC' }) : '') : esc(st.country ? `${fmtYear(y)} in ${st.country}` : `The world in ${fmtYear(y)}`)}</b>
        ${st.items.length ? `<span class="muted">${st.items.length} items · ${st.locating ? 'placing them on the globe…' : `${st.items.filter((e) => e.lat != null).length} on the globe`}</span>` : ''}</div>
      <div class="news-list">${body()}</div>
      ${st.source ? `<p class="muted small news-src">Source: <a href="${esc(st.source.url)}" target="_blank" rel="noopener">${esc(st.source.label || st.source.title)}</a>${st.mode === 'papers' ? '.' : ', CC BY-SA 4.0.'} ${st.mode === 'papers' ? 'Page scans and text from the Library of Congress; no known copyright restrictions.' : st.mode === 'day' ? 'Each item links to the news outlets it cites.' : 'Items summarise events documented on Wikipedia.'}</p>` : ''}`;
    el.querySelectorAll('[data-nm]').forEach((b) => (b.onclick = () => { st.mode = b.dataset.nm; st.loadedFor = ''; renderPanel(el); load(); }));
    el.querySelector('[data-nd]')?.addEventListener('change', (e) => { st.date = e.target.value; const yy = +st.date.slice(0, 4); if (yy !== ctx.state.year) ctx.setYear(yy, true); load(); });
    el.querySelector('[data-nc]')?.addEventListener('change', (e) => { st.country = e.target.value; load(); });
    el.querySelector('[data-pd]')?.addEventListener('change', (e) => { st.paperDate = e.target.value; load(); });
    el.querySelector('[data-pq]')?.addEventListener('change', (e) => { st.paperQ = e.target.value.trim(); load(); });
    el.querySelectorAll('[data-paper]').forEach((b) => (b.onclick = () => ctx.showPaper(st.items, +b.dataset.paper)));
    el.querySelector('[data-nretry]')?.addEventListener('click', () => { st.loadedFor = ''; load(); });
    el.querySelectorAll('[data-ni]').forEach((b) => (b.onclick = (ev) => { if (ev.target.closest('a')) return; ctx.showNewsItem(st.items[+b.dataset.ni]); }));
    el.querySelectorAll('.news-list a[data-wiki]').forEach((a) => (a.onclick = (ev) => { ev.preventDefault(); ctx.openArticle(a.dataset.wiki); }));
  }
  function body() {
    if (st.status === 'loading') return `<p class="muted">Gathering the news…</p>`;
    if (st.status === 'error') return `<p class="muted">Wikipedia is busy or unreachable right now. <button class="btn" data-nretry>${icon('refresh-cw')}Try again</button></p>`;
    if (st.status === 'empty') return `<p class="muted">${st.mode === 'papers' ? 'No digitised newspaper pages were found for this date. Try a nearby day or remove the search words.' : `No chronicle was found for this ${st.mode === 'day' ? 'day' : st.country ? 'country and year' : 'year'}.`}${st.source ? ` <a href="${esc(st.source.url)}" target="_blank" rel="noopener">Look on Wikipedia</a>.` : ''}</p>`;
    if (st.mode === 'papers') return `<div class="papers">${st.items.map((e, i) => `<button class="paper" data-paper="${i}"><img src="${esc(e.thumb)}" alt="Front page of ${esc(e.paper)}" loading="lazy"><span><b>${esc(e.paper)}</b><small>${esc([e.city, e.state].filter(Boolean).join(', '))}</small></span></button>`).join('')}</div>`;
    let lastCat = null;
    return st.items.map((e, i) => {
      const head = st.mode === 'day' && e.category !== lastCat ? `<h3 class="news-cat">${esc(e.category || 'News')}</h3>` : '';
      lastCat = e.category;
      return `${head}<article class="news-item ${e.lat != null ? 'geo' : ''}" data-ni="${i}" tabindex="0">
        <div class="news-meta">${e.date ? `<span class="num">${esc(e.date)}</span>` : ''}${e.topic ? `<span>${esc(e.topic)}</span>` : ''}${e.lat != null ? `<span class="on-map">${icon('map-pin')}${esc(e.place)}</span>` : ''}</div>
        <p>${e.text}</p></article>`;
    }).join('');
  }

  return {
    state: st,
    load,
    renderPanel,
    pins: () => st.items.filter((e) => e.lat != null),
    invalidate: () => { st.loadedFor = ''; },
  };
}

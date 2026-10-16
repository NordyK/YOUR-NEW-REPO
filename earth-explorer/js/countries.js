// Countries: atlas index, rich profiles (facts + World Bank time series) and side-by-side comparison.
import { icon } from './icons.js';

// Chart series colours: dataviz reference palette, dark steps (validated on #0c1220: CVD ΔE 26.8, contrast ≥ 3:1).
const SERIES = ['#3987e5', '#d95926'];
export const INDICATORS = [
  { id: 'SP.POP.TOTL', label: 'Population', fmt: (v) => fmtBig(v) },
  { id: 'NY.GDP.PCAP.CD', label: 'GDP per person (US$)', fmt: (v) => '$' + fmtBig(v) },
  { id: 'SP.DYN.LE00.IN', label: 'Life expectancy (years)', fmt: (v) => v.toFixed(1) },
  { id: 'SP.URB.TOTL.IN.ZS', label: 'Urban population (%)', fmt: (v) => v.toFixed(1) + '%' },
  { id: 'EN.GHG.CO2.PC.CE.AR5', label: 'CO₂ per person (t)', fmt: (v) => v.toFixed(2) + ' t' },
  { id: 'AG.LND.FRST.ZS', label: 'Forest cover (% of land)', fmt: (v) => v.toFixed(1) + '%' },
  { id: 'IT.NET.USER.ZS', label: 'Internet users (%)', fmt: (v) => v.toFixed(1) + '%' },
];
function fmtBig(v) {
  const a = Math.abs(v);
  if (a >= 1e12) return (v / 1e12).toFixed(2) + ' T';
  if (a >= 1e9) return (v / 1e9).toFixed(2) + ' B';
  if (a >= 1e6) return (v / 1e6).toFixed(1) + ' M';
  if (a >= 1e4) return Math.round(v / 1e3) + ' K';
  return Math.round(v).toLocaleString('en-US');
}

export function createCountries(ctx) {
  const { esc, getJSON, COUNTRIES, COUNTRY_BY_ISO3 } = ctx;
  let INFO = {};
  const ready = fetch(new URL('../assets/data/countries-info.json', import.meta.url)).then((r) => r.json()).then((d) => (INFO = d)).catch(() => (INFO = {}));
  const wbCache = new Map();
  const state = { filter: '', compare: [] };

  function wb(iso3, ind) {
    const k = iso3 + '|' + ind;
    if (!wbCache.has(k)) {
      wbCache.set(k, getJSON(`https://api.worldbank.org/v2/country/${iso3}/indicator/${ind}?format=json&per_page=100&date=1960:2026`).then((d) =>
        Array.isArray(d) && Array.isArray(d[1]) ? d[1].filter((r) => r.value != null).map((r) => [+r.date, r.value]).sort((a, b) => a[0] - b[0]) : []));
    }
    return wbCache.get(k);
  }

  /** Accessible single/multi-series line chart: crosshair + tooltip, legend for 2 series, table view. */
  function lineChart({ title, fmt, series, year }) {
    const W = 360, H = 150, L = 46, R = 12, T = 12, B = 24;
    const all = series.flatMap((s) => s.points);
    const wrap = document.createElement('figure');
    wrap.className = 'chart';
    if (!all.length) { wrap.innerHTML = `<figcaption>${esc(title)}</figcaption><p class="chart-empty">No World Bank data for this measure.</p>`; return wrap; }
    const x0 = Math.min(...all.map((p) => p[0])), x1 = Math.max(...all.map((p) => p[0]));
    let y0 = Math.min(0, ...all.map((p) => p[1])), y1 = Math.max(...all.map((p) => p[1]));
    if (y1 === y0) y1 = y0 + 1;
    const X = (x) => L + ((x - x0) / Math.max(1, x1 - x0)) * (W - L - R);
    const Y = (y) => H - B - ((y - y0) / (y1 - y0)) * (H - T - B);
    const ticks = [y0, (y0 + y1) / 2, y1];
    const path = (pts) => pts.map((p, i) => `${i ? 'L' : 'M'}${X(p[0]).toFixed(1)},${Y(p[1]).toFixed(1)}`).join('');
    const yearIn = year >= x0 && year <= x1;
    wrap.innerHTML = `<figcaption>${esc(title)}</figcaption>
      ${series.length > 1 ? `<div class="chart-legend">${series.map((s, i) => `<span><i style="background:${SERIES[i]}"></i>${esc(s.name)}</span>`).join('')}</div>` : ''}
      <div class="chart-plot"><svg viewBox="0 0 ${W} ${H}" role="img" aria-label="${esc(title)} from ${x0} to ${x1}">
        ${ticks.map((t) => `<line x1="${L}" x2="${W - R}" y1="${Y(t)}" y2="${Y(t)}" class="grid"/><text x="${L - 6}" y="${Y(t) + 4}" class="ax" text-anchor="end">${esc(fmt(t))}</text>`).join('')}
        <text x="${L}" y="${H - 6}" class="ax">${x0}</text><text x="${W - R}" y="${H - 6}" class="ax" text-anchor="end">${x1}</text>
        ${yearIn ? `<line x1="${X(year)}" x2="${X(year)}" y1="${T}" y2="${H - B}" class="yr"/><text x="${X(year) + 4}" y="${T + 10}" class="ax">${year}</text>` : ''}
        ${series.map((s, i) => `<path d="${path(s.points)}" fill="none" stroke="${SERIES[i]}" stroke-width="2" stroke-linejoin="round" stroke-linecap="round"/>`).join('')}
        ${series.length <= 2 ? series.map((s, i) => { const p = s.points[s.points.length - 1]; return p ? `<circle cx="${X(p[0])}" cy="${Y(p[1])}" r="3.5" fill="${SERIES[i]}" stroke="#0c1220" stroke-width="2"/>` : ''; }).join('') : ''}
        <line class="xh" y1="${T}" y2="${H - B}" x1="0" x2="0" visibility="hidden"/>
        <rect x="${L}" y="${T}" width="${W - L - R}" height="${H - T - B}" fill="transparent" class="hit"/>
      </svg><div class="chart-tip" hidden></div></div>
      <details class="chart-table"><summary>Show data</summary><div class="table-wrap"><table><thead><tr><th>Year</th>${series.map((s) => `<th>${esc(s.name)}</th>`).join('')}</tr></thead><tbody>${[...new Set(all.map((p) => p[0]))].sort((a, b) => b - a).map((yr) => `<tr><td>${yr}</td>${series.map((s) => { const p = s.points.find((q) => q[0] === yr); return `<td>${p ? esc(fmt(p[1])) : '—'}</td>`; }).join('')}</tr>`).join('')}</tbody></table></div></details>`;
    const svg = wrap.querySelector('svg'), xh = wrap.querySelector('.xh'), tip = wrap.querySelector('.chart-tip'), hit = wrap.querySelector('.hit');
    const show = (clientX) => {
      const r = svg.getBoundingClientRect();
      const sx = ((clientX - r.left) / r.width) * W;
      const yr = Math.round(x0 + ((sx - L) / (W - L - R)) * (x1 - x0));
      const rows = series.map((s, i) => {
        const p = s.points.reduce((best, q) => (Math.abs(q[0] - yr) < Math.abs(best[0] - yr) ? q : best), s.points[0]);
        return p ? { s, i, p } : null;
      }).filter(Boolean);
      if (!rows.length) return;
      const xs = X(rows[0].p[0]);
      xh.setAttribute('x1', xs); xh.setAttribute('x2', xs); xh.setAttribute('visibility', 'visible');
      tip.replaceChildren();
      const head = document.createElement('div'); head.className = 'tt-year'; head.textContent = rows[0].p[0]; tip.append(head);
      rows.forEach(({ s, i, p }) => {
        const row = document.createElement('div'); row.className = 'tt-row';
        const dot = document.createElement('i'); dot.style.background = SERIES[i];
        const v = document.createElement('strong'); v.textContent = fmt(p[1]);
        const n = document.createElement('span'); n.textContent = series.length > 1 ? s.name : '';
        row.append(dot, v, n); tip.append(row);
      });
      tip.hidden = false;
      const px = (xs / W) * r.width;
      tip.style.left = Math.min(r.width - 120, Math.max(0, px + 10)) + 'px';
    };
    hit.addEventListener('pointermove', (e) => show(e.clientX));
    hit.addEventListener('pointerleave', () => { tip.hidden = true; xh.setAttribute('visibility', 'hidden'); });
    hit.addEventListener('click', (e) => { const r = svg.getBoundingClientRect(); const yr = Math.round(x0 + ((((e.clientX - r.left) / r.width) * W - L) / (W - L - R)) * (x1 - x0)); ctx.setYear(yr, true); });
    return wrap;
  }

  async function mountIndicators(el, iso3s, names) {
    el.innerHTML = `<p class="muted small">Loading World Bank data…</p>`;
    const res = await Promise.all(INDICATORS.map(async (ind) => ({ ind, series: await Promise.all(iso3s.map(async (c, i) => ({ name: names[i], points: await wb(c, ind.id) }))) })));
    el.innerHTML = '';
    res.forEach(({ ind, series }) => el.appendChild(lineChart({ title: ind.label, fmt: ind.fmt, series: series.filter((s) => s.points.length || iso3s.length > 1), year: ctx.state.year })));
    const note = document.createElement('p');
    note.className = 'muted small';
    note.innerHTML = 'Source: <a href="https://data.worldbank.org" target="_blank" rel="noopener">World Bank Open Data</a> (CC BY 4.0). Select a year on a chart to move the time machine there.';
    el.appendChild(note);
  }

  /** Facts block for a country (from the bundled mledoze/countries dataset, ODbL). */
  function factsHtml(iso3) {
    const f = INFO[iso3];
    if (!f) return '';
    const borders = (f.borders || []).map((b) => (COUNTRY_BY_ISO3[b] ? `<button class="chip" data-country="${b}">${esc(INFO[b]?.name || COUNTRY_BY_ISO3[b].name)}</button>` : '')).join('');
    return `<dl class="figures">
      ${f.capital?.length ? `<dt>Capital</dt><dd>${esc(f.capital.join(', '))}</dd>` : ''}
      ${f.official && f.official !== f.name ? `<dt>Official name</dt><dd>${esc(f.official)}</dd>` : ''}
      ${f.area ? `<dt>Area</dt><dd>${Math.round(f.area).toLocaleString('en-US')} km²</dd>` : ''}
      ${f.languages?.length ? `<dt>Languages</dt><dd>${esc(f.languages.join(', '))}</dd>` : ''}
      ${f.currencies?.length ? `<dt>Currency</dt><dd>${esc(f.currencies.join(', '))}</dd>` : ''}
      ${f.demonym ? `<dt>People</dt><dd>${esc(f.demonym)}</dd>` : ''}
      ${f.callingCode ? `<dt>Calling code</dt><dd>${esc(f.callingCode)}</dd>` : ''}
      ${f.landlocked ? `<dt>Coast</dt><dd>Landlocked</dd>` : ''}
    </dl>${borders ? `<h3>Neighbours</h3><div class="chips-row">${borders}</div>` : ''}`;
  }

  // ---------------------------------------------------------------- panel: atlas index + compare
  function renderPanel(el) {
    const q = state.filter.toLowerCase();
    const groups = {};
    COUNTRIES.forEach((c) => {
      const f = INFO[c.iso3];
      const region = f?.region || c.props.CONTINENT || 'Other';
      if (q && !(`${c.longName} ${f?.capital?.join(' ') || ''} ${region}`.toLowerCase().includes(q))) return;
      (groups[region] ||= []).push(c);
    });
    const cmp = state.compare.map((i) => COUNTRY_BY_ISO3[i]).filter(Boolean);
    el.innerHTML = `<div class="side-head"><h2>Countries</h2></div>
      <p class="side-desc">An atlas of ${COUNTRIES.length} countries with facts, neighbours and 60 years of World Bank data. Tick two to compare them.</p>
      ${cmp.length ? `<div class="notice" style="margin-bottom:10px">${cmp.map((c) => esc(c.longName)).join(' and ')}${cmp.length === 2 ? `<button class="btn primary" data-cmp-go>${icon('scale')}Compare</button>` : '<span class="muted">Tick one more to compare.</span>'}<button class="btn" data-cmp-clear>Clear</button></div>` : ''}
      <input class="list-filter" type="search" data-cf placeholder="Filter by country, capital or region" aria-label="Filter countries" value="${esc(state.filter)}">
      ${Object.keys(groups).sort().map((g) => `<div class="group-title">${esc(g)} <span class="muted">${groups[g].length}</span></div>` + groups[g].sort((a, b) => a.longName.localeCompare(b.longName)).map((c) => `<div class="item">
          <input type="checkbox" data-cmp="${c.iso3}" ${state.compare.includes(c.iso3) ? 'checked' : ''} aria-label="Compare ${esc(c.longName)}">
          ${c.iso2 ? `<img class="flag" src="https://flagcdn.com/w40/${c.iso2.toLowerCase()}.png" alt="" loading="lazy">` : '<span class="flag"></span>'}
          <button class="nm" data-open="c:${c.iso3}">${esc(c.longName)}<small>${esc(INFO[c.iso3]?.capital?.[0] || c.props.SUBREGION || '')}</small></button></div>`).join('')).join('')}`;
    const inp = el.querySelector('[data-cf]');
    inp.oninput = () => { const pos = inp.selectionStart; state.filter = inp.value; renderPanel(el); const n = el.querySelector('[data-cf]'); n.focus(); n.setSelectionRange(pos, pos); };
    el.querySelectorAll('[data-cmp]').forEach((cb) => (cb.onchange = () => {
      const id = cb.dataset.cmp;
      state.compare = cb.checked ? [...state.compare.filter((x) => x !== id), id].slice(-2) : state.compare.filter((x) => x !== id);
      renderPanel(el);
    }));
    el.querySelector('[data-cmp-go]')?.addEventListener('click', () => showCompare(state.compare));
    el.querySelector('[data-cmp-clear]')?.addEventListener('click', () => { state.compare = []; renderPanel(el); });
  }

  function showCompare([a, b]) {
    const A = COUNTRY_BY_ISO3[a], B = COUNTRY_BY_ISO3[b];
    if (!A || !B) return;
    const fa = INFO[a] || {}, fb = INFO[b] || {};
    const row = (k, va, vb) => `<tr><th scope="row">${esc(k)}</th><td>${esc(va ?? '—')}</td><td>${esc(vb ?? '—')}</td></tr>`;
    ctx.openInfo(`<div class="info-main">
      <h2>${esc(A.longName)} and ${esc(B.longName)}</h2>
      <div class="meta">${icon('scale')}<span>Side-by-side comparison</span></div>
      <div class="table-wrap"><table class="cmp"><thead><tr><th></th><th><span class="swatch" style="background:${SERIES[0]}"></span> ${esc(A.longName)}</th><th><span class="swatch" style="background:${SERIES[1]}"></span> ${esc(B.longName)}</th></tr></thead><tbody>
        ${row('Capital', fa.capital?.join(', '), fb.capital?.join(', '))}
        ${row('Region', fa.subregion, fb.subregion)}
        ${row('Area', fa.area ? Math.round(fa.area).toLocaleString('en-US') + ' km²' : null, fb.area ? Math.round(fb.area).toLocaleString('en-US') + ' km²' : null)}
        ${row('Population', A.props.POP_EST ? fmtBig(A.props.POP_EST) : null, B.props.POP_EST ? fmtBig(B.props.POP_EST) : null)}
        ${row('Languages', fa.languages?.join(', '), fb.languages?.join(', '))}
        ${row('Currency', fa.currencies?.join(', '), fb.currencies?.join(', '))}
        ${row('Landlocked', fa.landlocked ? 'Yes' : 'No', fb.landlocked ? 'Yes' : 'No')}
      </tbody></table></div>
      <h3>Over time</h3><div class="charts" id="cmp-charts"></div></div>`, null);
    mountIndicators(document.getElementById('cmp-charts'), [a, b], [A.longName, B.longName]);
  }

  return { ready, factsHtml, mountIndicators, renderPanel, showCompare, info: (iso3) => INFO[iso3] };
}

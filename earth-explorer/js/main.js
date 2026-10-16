import { Globe } from '../vendor/earth-libs.js';
import { THREE, createAnimal, AnimalViewer, MARINE, FLYING } from './animals3d.js';

// ============================================================ helpers
const $ = (s, r = document) => r.querySelector(s);
const $$ = (s, r = document) => [...r.querySelectorAll(s)];
const esc = (s) => String(s ?? '').replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
const clamp = (v, a, b) => Math.max(a, Math.min(b, v));
const TEX = 'https://cdn.jsdelivr.net/npm/three-globe@2.45.1/example/img/';
const HIST_URL = (y) => `https://cdn.jsdelivr.net/gh/aourednik/historical-basemaps@master/geojson/world_${y < 0 ? 'bc' + -y : y}.geojson`;
const NOW = 2026;
const debounce = (fn, ms) => { let t; return (...a) => { clearTimeout(t); t = setTimeout(() => fn(...a), ms); }; };
function hexA(hex, a) {
  const c = new THREE.Color(hex);
  return `rgba(${Math.round(c.r * 255)},${Math.round(c.g * 255)},${Math.round(c.b * 255)},${a})`;
}
function hashColor(str, s = 55, l = 55) {
  let h = 0;
  for (const ch of String(str)) h = (h * 31 + ch.charCodeAt(0)) >>> 0;
  return `hsl(${h % 360}, ${s}%, ${l}%)`;
}
function fmtYear(y, long = false) {
  y = Math.round(y);
  if (y < 0) return `${(-y).toLocaleString('en-US')} BC`;
  if (y === 0) return '1 BC';
  if (y < 1000) return `AD ${y}`;
  return long && y === NOW ? `${y}` : `${y}`;
}
function fmtNum(v, unit = '') {
  if (v == null || isNaN(v)) return '—';
  const a = Math.abs(v);
  if (a >= 1e9) return (v / 1e9).toFixed(a >= 1e10 ? 1 : 2) + ' B' + unit;
  if (a >= 1e6) return (v / 1e6).toFixed(a >= 1e8 ? 0 : 1) + ' M' + unit;
  if (a >= 1e4) return Math.round(v / 1e3) + ' K' + unit;
  if (a >= 100) return Math.round(v).toLocaleString('en-US') + unit;
  return (Math.round(v * 100) / 100).toString() + unit;
}
function interp(points, x) {
  if (!points?.length) return null;
  if (x < points[0][0] || x > points[points.length - 1][0]) return null;
  for (let i = 1; i < points.length; i++) {
    if (x <= points[i][0]) {
      const [x0, y0] = points[i - 1], [x1, y1] = points[i];
      return x1 === x0 ? y1 : y0 + ((y1 - y0) * (x - x0)) / (x1 - x0);
    }
  }
  return points[points.length - 1][1];
}
function interpClamped(points, x) {
  if (!points?.length) return 1;
  if (x <= points[0][0]) return points[0][1];
  if (x >= points[points.length - 1][0]) return points[points.length - 1][1];
  return interp(points, x);
}
function toast(msg, ms = 3500) {
  const t = $('#toast');
  t.innerHTML = msg;
  t.classList.remove('hidden');
  clearTimeout(toast._t);
  toast._t = setTimeout(() => t.classList.add('hidden'), ms);
}

// ============================================================ data
async function safeImport(path) {
  try { return await import(path); } catch (e) { console.warn('Could not load', path, e); return {}; }
}
const [wMod, lMod, aMod, rMod, tMod, countriesGeo] = await Promise.all([
  safeImport('./data/wonders.js'),
  safeImport('./data/landmarks.js'),
  safeImport('./data/animals.js'),
  safeImport('./data/resources.js'),
  safeImport('./data/timeline.js'),
  fetch(new URL('../assets/data/countries.geojson', import.meta.url)).then((r) => r.json()),
]);
const WONDERS = wMod.WONDERS || [];
const LANDMARKS = lMod.LANDMARKS || [];
const ANIMALS = aMod.ANIMALS || [];
const RESOURCES = rMod.RESOURCES || [];
const { STATS = {}, ERAS = [], EVENTS = [], CHANGES = [], HIST_MAPS = [] } = tMod;
const byId = (arr) => Object.fromEntries(arr.map((x) => [x.id, x]));
const ANIMAL = byId(ANIMALS), RESOURCE = byId(RESOURCES);
EVENTS.forEach((e, i) => (e.id = e.id || 'ev' + i));

const COUNTRIES = countriesGeo.features
  .filter((f) => f.properties.ISO_A3 !== 'ATA' || true)
  .map((f) => {
    const p = f.properties;
    const iso3 = p.ISO_A3 !== '-99' ? p.ISO_A3 : p.ADM0_A3;
    const iso2 = p.ISO_A2 !== '-99' ? p.ISO_A2 : p.ISO_A2_EH && p.ISO_A2_EH !== '-99' ? p.ISO_A2_EH : null;
    return { kind: 'country', geometry: f.geometry, name: p.NAME, longName: p.NAME_LONG || p.NAME, iso3, iso2, props: p };
  });
const COUNTRY_BY_ISO3 = Object.fromEntries(COUNTRIES.map((c) => [c.iso3, c]));
// Fix a few Natural Earth codes so producer tables line up
if (COUNTRY_BY_ISO3['FRA'] == null) { const f = COUNTRIES.find((c) => c.name === 'France'); if (f) { f.iso3 = 'FRA'; COUNTRY_BY_ISO3.FRA = f; } }
if (COUNTRY_BY_ISO3['NOR'] == null) { const f = COUNTRIES.find((c) => c.name === 'Norway'); if (f) { f.iso3 = 'NOR'; COUNTRY_BY_ISO3.NOR = f; } }
const STATUS = { LC: 'Least Concern', NT: 'Near Threatened', VU: 'Vulnerable', EN: 'Endangered', CR: 'Critically Endangered', EW: 'Extinct in the Wild', EX: 'Extinct', DD: 'Data Deficient' };
const RES_WIKI = { oil: 'Petroleum', 'natural-gas': 'Natural_gas', coal: 'Coal', gold: 'Gold', silver: 'Silver', copper: 'Copper', 'iron-ore': 'Iron_ore', lithium: 'Lithium', cobalt: 'Cobalt', nickel: 'Nickel', uranium: 'Uranium', diamonds: 'Diamond', 'rare-earths': 'Rare-earth_element', platinum: 'Platinum', bauxite: 'Bauxite', phosphate: 'Phosphorite' };
const EVENT_ICON = { eruption: '🌋', earthquake: '🫨', tsunami: '🌊', climate: '🌡️', civilization: '🏺', exploration: '🧭', discovery: '💡', disaster: '⚠️', conflict: '⚔️', space: '🚀', environment: '🌱' };
const EVENT_COLOR = { eruption: '#ff6b3d', earthquake: '#f59f00', tsunami: '#4dabf7', climate: '#ff8787', civilization: '#e599f7', exploration: '#63e6be', discovery: '#ffd43b', disaster: '#fa5252', conflict: '#c92a2a', space: '#91a7ff', environment: '#69db7c' };
const CHANGE_STYLE = {
  ice: { color: '#eef7ff', a: 0.78 }, land: { color: '#b4915c', a: 0.72 }, water: { color: '#3b9cff', a: 0.62 },
  vegetation: { color: '#62c450', a: 0.48 }, loss: { color: '#ff6b3d', a: 0.55 },
};

// ============================================================ state
const state = {
  year: NOW,
  basemap: 'satellite',
  borders: true,
  labels: true,
  layers: { wonders: true, landmarks: true, events: true, changes: true },
  wonderTypes: null, // null = all
  landmarkCats: null,
  animals: new Set(),
  resources: new Set(),
  hoverCountry: null,
  histYear: null,
  histFeatures: null,
  tab: 'wonders',
  altitude: 2.5,
  selectedKey: null,
};

// ============================================================ globe
const globeEl = $('#globe');
const globe = new Globe(globeEl, { rendererConfig: { antialias: true, alpha: true, logarithmicDepthBuffer: true } })
  .backgroundImageUrl(TEX + 'night-sky.png')
  .showAtmosphere(true)
  .atmosphereColor('#6cc6ff')
  .atmosphereAltitude(0.17)
  .showGraticules(false);
globe.controls().minDistance = 100.008;
globe.globeTileEngineMaxLevel(19);
globe.controls().maxDistance = 1200;
globe.pointOfView({ lat: 22, lng: 12, altitude: 2.6 });

const BASEMAPS = {
  satellite: { tiles: (x, y, l) => `https://server.arcgisonline.com/ArcGIS/rest/services/World_Imagery/MapServer/tile/${l}/${y}/${x}`, credit: 'Imagery © Esri, Maxar, Earthstar Geographics' },
  topo: { tiles: (x, y, l) => `https://server.arcgisonline.com/ArcGIS/rest/services/World_Topo_Map/MapServer/tile/${l}/${y}/${x}`, credit: 'Map © Esri, HERE, Garmin, USGS, NGA' },
  streets: { tiles: (x, y, l) => `https://server.arcgisonline.com/ArcGIS/rest/services/World_Street_Map/MapServer/tile/${l}/${y}/${x}`, credit: 'Map © Esri, HERE, Garmin, OpenStreetMap contributors' },
  bluemarble: { img: TEX + 'earth-blue-marble.jpg', bump: TEX + 'earth-topology.png', credit: 'NASA Blue Marble' },
  night: { img: TEX + 'earth-night.jpg', bump: TEX + 'earth-topology.png', credit: 'NASA Earth at Night' },
};
function setBasemap(key) {
  state.basemap = key;
  const b = BASEMAPS[key];
  if (b.tiles) {
    globe.globeImageUrl(null).bumpImageUrl(null).globeTileEngineUrl(b.tiles);
  } else {
    globe.globeTileEngineUrl(null).globeImageUrl(b.img).bumpImageUrl(b.bump || null);
  }
  $('#attribution').textContent = `${b.credit} · Data: Natural Earth, Wikipedia, NOAA, NASA`;
  $('#basemap').value = key;
}
setBasemap('satellite');

// ---- zoom-dependent tuning: rotate/zoom speed, camera near plane, overlay altitude
let altScale = 1;
function onZoom(pov) {
  state.altitude = pov.altitude;
  const c = globe.controls(), h = pov.altitude * 100;
  c.rotateSpeed = clamp(pov.altitude * 0.55, 0.0006, 1.2);
  const want = 1 - (0.18 * h) / (100 + h);
  c.zoomSpeed = clamp(Math.log(want) / Math.log(0.95), 0.05, 3.5);
  const cam = globe.camera();
  const near = clamp(h * 0.05, 0.0002, 1);
  if (Math.abs(cam.near - near) / near > 0.3) { cam.near = near; cam.updateProjectionMatrix(); }
  document.body.classList.toggle('zoomed', pov.altitude < 1.1);
  document.body.classList.toggle('far', pov.altitude > 1.8);
  const s = pov.altitude > 0.6 ? 1 : pov.altitude > 0.15 ? 0.45 : pov.altitude > 0.03 ? 0.12 : pov.altitude > 0.004 ? 0.03 : 0.003;
  if (s !== altScale) { altScale = s; globe.polygonAltitude(globe.polygonAltitude()); globe.htmlAltitude(globe.htmlAltitude()); }
  const fq = Math.round(zoomFade() * 10);
  if (fq !== onZoom._fq) { onZoom._fq = fq; globe.polygonCapColor(polyCap); }
  scaleMinis();
}
globe.onZoom(onZoom);

// ============================================================ geometry helpers
function ellipseGeom([lat, lng, rLat, rLng], scale = 1) {
  const rl = rLat * scale, rg = rLng * scale;
  if (rg >= 170 || Math.abs(lat) + rl >= 89.5) {
    // polar cap / circumpolar band → lat/lng rectangle
    const north = lat > 0;
    const edge = north ? clamp(lat - rl, -89, 89.5) : clamp(lat + rl, -89.5, 89);
    const pole = north ? 89.9 : -89.9;
    const lo0 = rg >= 170 ? -180 : lng - rg, lo1 = rg >= 170 ? 180 : lng + rg;
    const ring = [];
    for (let x = lo0; x <= lo1; x += 6) ring.push([x, edge]);
    ring.push([lo1, edge], [lo1, pole], [lo0, pole], [lo0, edge]);
    return { type: 'Polygon', coordinates: [north ? ring.reverse() : ring] }; // clockwise (d3-geo spherical winding)
  }
  const pts = [];
  const n = 48;
  for (let i = 0; i <= n; i++) {
    const a = -(i / n) * Math.PI * 2; // clockwise ring
    pts.push([lng + rg * Math.cos(a), clamp(lat + rl * Math.sin(a), -89.5, 89.5)]);
  }
  return { type: 'Polygon', coordinates: [pts] };
}

// ============================================================ time logic
const KNOTS = [[0, -20000], [150, -8000], [260, -3000], [380, 0], [560, 1500], [740, 1900], [860, 2000], [1000, NOW]];
const posToYear = (p) => Math.round(interp(KNOTS, clamp(p, 0, 1000)));
const yearToPos = (y) => interp(KNOTS.map(([a, b]) => [b, a]), clamp(y, -20000, NOW));
const exists = (lm, y = state.year) => (lm.built == null || lm.built <= y) && (lm.destroyed == null || lm.destroyed > y);
const animalAlive = (a, y = state.year) => a.extinct == null || y <= a.extinct;
function eventWindow(y) { return y < -5000 ? 1500 : y < -1000 ? 400 : y < 500 ? 150 : y < 1500 ? 60 : y < 1900 ? 20 : y < 2000 ? 6 : 3; }
function visibleEvents(y = state.year) {
  const w = eventWindow(y);
  return EVENTS.filter((e) => Math.abs(e.year - y) <= w).sort((a, b) => Math.abs(a.year - y) - Math.abs(b.year - y));
}
function changeFactor(c, y = state.year) { return clamp(interpClamped(c.keys, y), 0, 1.3); }
function animalZones(a) {
  if (a.pastRange && state.year < a.pastRange.until) return { zones: a.pastRange.zones, past: true };
  return { zones: a.range || [], past: false };
}

// ============================================================ polygons
const changeGeomCache = new Map();
let rangeObjs = [], changeObjs = [];
function buildRangePolys() {
  rangeObjs = [];
  [...state.animals].forEach((id, k) => {
    const a = ANIMAL[id];
    if (!a) return;
    const { zones, past } = animalZones(a);
    const faded = !animalAlive(a);
    zones.forEach((z, i) => rangeObjs.push({ kind: 'range', id: id + '#' + i + (past ? 'p' : ''), animal: a, geometry: ellipseGeom(z), color: a.color, order: k, past, faded }));
  });
}
function buildChangePolys() {
  changeObjs = [];
  if (!state.layers.changes) return;
  CHANGES.forEach((c) => {
    const f = changeFactor(c);
    if (f < 0.02) return;
    const q = Math.round(Math.sqrt(f) * 40) / 40;
    (c.zones || []).forEach((z, i) => {
      const key = c.id + '|' + i + '|' + q;
      if (!changeGeomCache.has(key)) changeGeomCache.set(key, { kind: 'change', change: c, geometry: ellipseGeom(z, q), factor: f, idx: i });
      const o = changeGeomCache.get(key);
      o.factor = f;
      changeObjs.push(o);
    });
  });
}
function countryPolys() {
  const choropleth = currentChoropleth();
  if (state.histFeatures && state.histYear != null) return state.borders ? state.histFeatures : [];
  if (!state.borders && !choropleth) return [];
  return COUNTRIES;
}
function currentChoropleth() {
  const ids = [...state.resources];
  if (!ids.length) return null;
  const r = RESOURCE[ids[ids.length - 1]];
  if (!r) return null;
  const map = {};
  (r.producers || []).forEach((p) => (map[p.iso3] = p));
  return { r, map, max: Math.max(...(r.producers || []).map((p) => p.share || 0), 1) };
}
function refreshPolygons() {
  globe.polygonsData([...countryPolys(), ...changeObjs, ...rangeObjs]);
}
const zoomFade = () => clamp(state.altitude / 1.2, 0.3, 1);
function polyCap(d) {
  if (d.kind === 'country') {
    const ch = currentChoropleth();
    if (ch && !state.histFeatures) {
      const p = ch.map[d.iso3];
      if (p) return hexA(ch.r.color, 0.25 + 0.6 * Math.sqrt((p.share || 0) / ch.max));
    }
    if (d === state.hoverCountry) return 'rgba(120, 210, 255, 0.28)';
    if (d.hist) return d.color;
    return 'rgba(255,255,255,0.012)';
  }
  if (d.kind === 'range') return hexA(d.color, (d.faded ? 0.12 : d.past ? 0.22 : 0.4) * zoomFade());
  if (d.kind === 'change') { const s = CHANGE_STYLE[d.change.kind] || CHANGE_STYLE.water; return hexA(d.change.color || s.color, s.a * clamp(0.45 + d.factor, 0, 1)); }
  return 'rgba(0,0,0,0)';
}
globe
  .polygonGeoJsonGeometry('geometry')
  .polygonCapColor(polyCap)
  .polygonSideColor((d) => (d.kind === 'range' ? hexA(d.color, 0.15) : 'rgba(0,0,0,0)'))
  .polygonStrokeColor((d) => {
    if (d.kind === 'country') return state.borders ? (d.hist ? 'rgba(255,240,200,0.75)' : 'rgba(255,255,255,0.6)') : false;
    if (d.kind === 'range') return hexA(d.color, d.faded ? 0.3 : 0.95);
    return false;
  })
  .polygonAltitude((d) => altScale * (d.kind === 'country' ? (d === state.hoverCountry ? 0.009 : 0.004) : d.kind === 'change' ? 0.005 + d.idx * 0.0002 : 0.008 + d.order * 0.0012))
  .polygonCapCurvatureResolution(3)
  .polygonsTransitionDuration(0)
  .polygonLabel((d) => {
    if (d.kind === 'country') {
      const ch = currentChoropleth();
      const p = ch && !d.hist ? ch.map[d.iso3] : null;
      return `<b>${esc(d.name)}</b>${d.hist ? `<br><span style="opacity:.7">Borders c. ${esc(fmtYear(state.histYear))}</span>` : ''}${p ? `<br>${esc(ch.r.emoji)} ${esc(ch.r.name)}: ${p.share}% of world · ${esc(p.value || '')}` : ''}`;
    }
    if (d.kind === 'range') return `${esc(d.animal.emoji)} <b>${esc(d.animal.name)}</b><br><span style="opacity:.75">${d.past ? 'Historic range' : d.faded ? 'Former range (extinct)' : 'Approximate range'}</span>`;
    if (d.kind === 'change') return `<b>${esc(d.change.name)}</b><br><span style="opacity:.75">${Math.round(d.factor * 100)}% of its mapped extent in ${esc(fmtYear(state.year))}</span>`;
    return '';
  })
  .onPolygonHover((d) => {
    const prev = state.hoverCountry;
    state.hoverCountry = d && d.kind === 'country' ? d : null;
    if (prev !== state.hoverCountry) globe.polygonCapColor(polyCap).polygonAltitude(globe.polygonAltitude());
    globeEl.style.cursor = d ? 'pointer' : null;
  })
  .onPolygonClick((d) => {
    if (d.kind === 'country') d.hist ? showPolity(d) : showCountry(d);
    else if (d.kind === 'range') showAnimal(d.animal.id);
    else if (d.kind === 'change') showChange(d.change);
  });

// ---- historical borders
const histCache = new Map();
const loadHistoric = debounce(async () => {
  const y = state.year;
  if (y >= 2011 || !HIST_MAPS.length) {
    if (state.histFeatures) { state.histFeatures = null; state.histYear = null; refreshPolygons(); renderLegend(); renderYearLabel(); }
    return;
  }
  const mapYear = [...HIST_MAPS].sort((a, b) => a - b).reduce((best, m) => (m <= y ? m : best), Math.min(...HIST_MAPS));
  if (mapYear === state.histYear && state.histFeatures) return;
  try {
    if (!histCache.has(mapYear)) {
      histCache.set(mapYear, fetch(HIST_URL(mapYear)).then((r) => { if (!r.ok) throw new Error(r.status); return r.json(); }).then((gj) =>
        gj.features.filter((f) => f.geometry && f.properties?.NAME).map((f) => ({
          kind: 'country', hist: true, geometry: f.geometry, name: f.properties.NAME,
          subject: f.properties.SUBJECTO, partOf: f.properties.PARTOF, color: hashColor(f.properties.SUBJECTO || f.properties.NAME, 60, 55).replace('hsl', 'hsla').replace(')', ', 0.33)'),
        }))));
    }
    const feats = await histCache.get(mapYear);
    if (state.year >= 2011) return;
    state.histFeatures = feats;
    state.histYear = mapYear;
    refreshPolygons();
    renderLegend();
    renderYearLabel();
    $('#tl-year small') && ($('#tl-year small').textContent = `borders c. ${fmtYear(mapYear)}`);
  } catch (e) {
    histCache.delete(mapYear);
    console.warn('historical borders failed', e);
  }
}, 220);

// ============================================================ html markers
const markerEls = new Map();
function markerEl(d) {
  let el = markerEls.get(d.key);
  if (!el) {
    el = document.createElement('div');
    el.className = 'marker' + (d.small ? ' small' : '') + (d.cls ? ' ' + d.cls : '');
    el.innerHTML = `<div class="dot">${d.emoji}</div><div class="lbl">${esc(d.label)}</div>`;
    el.style.setProperty('--mc', d.color);
    el.addEventListener('click', (ev) => { ev.stopPropagation(); d.open(); });
    el.addEventListener('pointerdown', (ev) => ev.stopPropagation());
    markerEls.set(d.key, el);
  }
  el.classList.toggle('selected', state.selectedKey === d.key);
  return el;
}
function buildMarkers() {
  const m = [];
  if (state.layers.wonders) {
    WONDERS.filter((w) => !state.wonderTypes || state.wonderTypes.has(w.type)).forEach((w) =>
      m.push({ key: 'w:' + w.id, lat: w.lat, lng: w.lng, emoji: w.emoji || '🏔️', label: w.name, color: '#4ade80', open: () => showWonder(w) }));
  }
  if (state.layers.landmarks) {
    LANDMARKS.filter((l) => (!state.landmarkCats || state.landmarkCats.has(l.category)) && exists(l)).forEach((l) =>
      m.push({ key: 'l:' + l.id, lat: l.lat, lng: l.lng, emoji: l.emoji || '🏛️', label: l.name, color: '#fbbf24', open: () => showLandmark(l) }));
  }
  if (state.layers.events) {
    visibleEvents().forEach((e) =>
      m.push({ key: 'e:' + e.id, lat: e.lat, lng: e.lng, emoji: EVENT_ICON[e.type] || '📍', label: `${e.name} (${fmtYear(e.year)})`, color: EVENT_COLOR[e.type] || '#fff', cls: 'event', open: () => showEvent(e) }));
  }
  state.resources.forEach((rid) => {
    const r = RESOURCE[rid];
    (r?.sites || []).forEach((s, i) => m.push({ key: `r:${rid}:${i}`, lat: s.lat, lng: s.lng, emoji: r.emoji, label: s.name, color: r.color, small: true, open: () => showSite(r, s) }));
  });
  state.animals.forEach((id) => {
    const a = ANIMAL[id];
    if (!a) return;
    (a.hotspots || []).forEach((h, i) => m.push({ key: `a:${id}:${i}`, lat: h.lat, lng: h.lng, emoji: a.emoji, label: `${a.name} · ${h.name}`, color: a.color, small: true, open: () => showAnimal(id, h) }));
  });
  return m;
}
globe
  .htmlElement(markerEl)
  .htmlLat('lat').htmlLng('lng')
  .htmlAltitude(() => (altScale < 0.01 ? 0 : 0.004 * altScale))
  .htmlTransitionDuration(0);
function refreshMarkers() {
  const data = buildMarkers();
  globe.htmlElementsData(data);
  globe.ringsData(state.layers.events ? visibleEvents().slice(0, 12) : [])
    .ringColor((e) => (t) => hexA(EVENT_COLOR[e.type] || '#ffffff', 1 - t))
    .ringMaxRadius(3.5).ringPropagationSpeed(2.2).ringRepeatPeriod(1300).ringAltitude(0.002);
}

// ============================================================ 3D animals on the globe
const miniCache = new Map();
let miniList = [];
async function refreshMinis() {
  const wanted = [];
  state.animals.forEach((id) => {
    const a = ANIMAL[id];
    if (!a || !animalAlive(a)) return;
    (a.hotspots || []).slice(0, 2).forEach((h, i) => wanted.push({ key: id + '#' + i, id, lat: h.lat, lng: h.lng, name: `${a.emoji} ${a.name}` }));
  });
  await Promise.all(wanted.map(async (w) => {
    if (!miniCache.has(w.key)) {
      miniCache.set(w.key, createAnimal(w.id).then((inst) => {
        const outer = new THREE.Group(), inner = new THREE.Group();
        // stand the model up in the tangent plane (pointing north) so its side view faces the overhead camera
        inner.rotation.y = -0.35;
        inner.add(inst.object);
        outer.add(inner);
        if (FLYING.has(w.id)) inst.object.position.y = 0.6;
        return { ...w, inst, obj: outer, inner };
      }).catch((e) => { console.warn('model failed', w.id, e); return null; }));
    }
  }));
  const keys = new Set(wanted.map((w) => w.key));
  const list = [];
  for (const k of keys) { const v = await miniCache.get(k); if (v) list.push(v); }
  miniList = list;
  globe.objectsData(miniList).objectLat('lat').objectLng('lng').objectAltitude(0).objectThreeObject((d) => d.obj).objectLabel('name').onObjectClick((d) => showAnimal(d.id));
  scaleMinis();
}
function scaleMinis() {
  const s = clamp(state.altitude * 11, 0.012, 13);
  miniList.forEach((m) => { m.inner.scale.setScalar(s); m.inner.position.set(-s * 0.75, -s * 0.15, s * 0.35); });
}
const clock = new THREE.Timer();
(function tick() {
  requestAnimationFrame(tick);
  clock.update();
  const dt = Math.min(0.05, clock.getDelta()), t = clock.getElapsed();
  miniList.forEach((m) => m.inst.update(t, dt));
})();

// ============================================================ Wikipedia
const wikiCache = new Map();
function wiki(title) {
  if (!title) return Promise.resolve(null);
  if (!wikiCache.has(title)) {
    wikiCache.set(title, fetch(`https://en.wikipedia.org/api/rest_v1/page/summary/${encodeURIComponent(title.replace(/ /g, '_'))}`)
      .then((r) => (r.ok ? r.json() : null)).catch(() => null));
  }
  return wikiCache.get(title);
}
// Wikimedia only renders standard thumbnail widths; try 960px, fall back to the API's own thumbnail.
function wikiThumb(w) {
  const src = w?.thumbnail?.source || w?.originalimage?.source;
  if (!src) return Promise.resolve(null);
  const big = src.replace(/\/(\d+)px-/, '/960px-');
  return new Promise((res) => {
    const im = new Image();
    im.onload = () => res(big);
    im.onerror = () => res(src);
    im.src = big;
  });
}

// ============================================================ info panel
const info = $('#info'), infoBody = $('#info-body');
let viewer = null;
const viewerHost = document.createElement('div');
viewerHost.className = 'viewer3d';
viewerHost.innerHTML = '<div class="v-tabs"></div><div class="v-loading">Loading 3D model…</div><div class="v-hint">🖱️ Drag to rotate · scroll to zoom</div>';

function openInfo(html) {
  infoBody.innerHTML = html;
  info.classList.remove('hidden');
  info.scrollTop = 0;
  if (window.innerWidth <= 760) $('#sidebar').classList.add('collapsed');
}
function closeInfo() {
  info.classList.add('hidden');
  state.selectedKey = null;
  $$('.marker.selected').forEach((e) => e.classList.remove('selected'));
}
$('#info-close').onclick = closeInfo;
function statGrid(stats) {
  const e = Object.entries(stats || {}).filter(([, v]) => v != null && v !== '');
  return e.length ? `<div class="stat-grid">${e.map(([k, v]) => `<div class="stat"><div class="k">${esc(k)}</div><div class="v">${esc(v)}</div></div>`).join('')}</div>` : '';
}
const factList = (f) => (f?.length ? `<h4>Did you know?</h4><ul class="facts">${f.map((x) => `<li>${esc(x)}</li>`).join('')}</ul>` : '');
function heroWithWiki(title, emoji) {
  const id = 'hero' + Math.random().toString(36).slice(2);
  wiki(title).then(async (w) => {
    const img = await wikiThumb(w);
    const el = document.getElementById(id);
    if (!el) return;
    if (img) { el.classList.remove('noimg'); el.textContent = ''; el.style.backgroundImage = `url("${img}")`; el.insertAdjacentHTML('beforeend', '<span class="credit">Photo: Wikipedia / Wikimedia Commons</span>'); }
    const ex = document.getElementById(id + 'x');
    if (ex && w?.extract) ex.innerHTML = `<h4>From Wikipedia</h4><p class="wiki-extract">${esc(w.extract)}</p>`;
  });
  return { hero: `<div class="info-hero noimg" id="${id}">${emoji || ''}</div>`, extract: `<div id="${id}x"></div>` };
}
function wikiLink(title) {
  return title ? `<a class="btn" href="https://en.wikipedia.org/wiki/${encodeURIComponent(title)}" target="_blank" rel="noopener">📖 Wikipedia</a>` : '';
}
// Programmatic flights: re-notify layers (tile engine) once the camera has settled.
function fly(pov, ms = 1600) {
  globe.pointOfView(pov, ms);
  const c = globe.controls();
  [ms + 60, ms + 600].forEach((t) => setTimeout(() => c.dispatchEvent({ type: 'change' }), t));
}
function flyTo(lat, lng, altitude = 0.9, ms = 1600) { fly({ lat, lng, altitude }, ms); }
function markSelected(key) {
  state.selectedKey = key;
  $$('.marker.selected').forEach((e) => e.classList.remove('selected'));
  markerEls.get(key)?.classList.add('selected');
}
function bindActions() {
  $$('[data-fly]', infoBody).forEach((b) => (b.onclick = () => { const [lat, lng, alt] = b.dataset.fly.split(',').map(Number); flyTo(lat, lng, alt || 0.5); }));
  $$('[data-closeup]', infoBody).forEach((b) => (b.onclick = () => {
    const [lat, lng] = b.dataset.closeup.split(',').map(Number);
    if (!BASEMAPS[state.basemap].tiles) setBasemap('satellite');
    fly({ lat, lng, altitude: 0.0012 }, 2600);
  }));
  $$('[data-year]', infoBody).forEach((b) => (b.onclick = () => setYear(+b.dataset.year, true)));
  $$('[data-animal]', infoBody).forEach((b) => (b.onclick = () => showAnimal(b.dataset.animal)));
  $$('[data-country]', infoBody).forEach((b) => (b.onclick = () => { const c = COUNTRY_BY_ISO3[b.dataset.country]; if (c) { showCountry(c); flyToCountry(c); } }));
  $$('[data-site]', infoBody).forEach((b) => (b.onclick = () => { const [rid, i] = b.dataset.site.split('|'); showSite(RESOURCE[rid], RESOURCE[rid].sites[+i]); }));
  $$('[data-item]', infoBody).forEach((b) => (b.onclick = () => openByKey(b.dataset.item)));
}
function closeupBtns(lat, lng) {
  return `<button class="btn" data-fly="${lat},${lng},0.35">🎯 Fly here</button><button class="btn" data-closeup="${lat},${lng}">🛰️ Satellite close-up</button>`;
}

function showWonder(w) {
  markSelected('w:' + w.id);
  flyTo(w.lat, w.lng, 0.7);
  const h = heroWithWiki(w.wiki, w.emoji);
  openInfo(`${h.hero}<div class="info-main">
    <div class="info-kicker">Natural wonder · ${esc(w.type)}</div>
    <h2>${esc(w.emoji)} ${esc(w.name)}</h2>
    <div class="info-sub">📍 ${esc(w.country)} · ${w.lat.toFixed(3)}°, ${w.lng.toFixed(3)}°${w.unesco ? ` · 🏅 UNESCO ${w.unesco}` : ''}</div>
    <p class="info-summary">${esc(w.summary)}</p>
    ${statGrid({ ...(w.stats || {}), ...(w.formed && Math.abs(w.formed) > 3000 ? { Formed: Math.abs(w.formed) >= 1e6 ? `~${fmtNum(Math.abs(w.formed))} years ago` : fmtYear(w.formed) } : {}) })}
    ${factList(w.facts)}${h.extract}
    <div class="info-actions">${closeupBtns(w.lat, w.lng)}${wikiLink(w.wiki)}</div></div>`);
  bindActions();
}
function showLandmark(l) {
  markSelected('l:' + l.id);
  flyTo(l.lat, l.lng, 0.6);
  const h = heroWithWiki(l.wiki, l.emoji);
  const lost = l.destroyed != null && l.destroyed <= state.year, notYet = l.built != null && l.built > state.year;
  openInfo(`${h.hero}<div class="info-main">
    <div class="info-kicker">Landmark · ${esc(l.category)}</div>
    <h2>${esc(l.emoji)} ${esc(l.name)}</h2>
    <div class="info-sub">📍 ${esc([l.city, l.country].filter(Boolean).join(', '))} · ${l.lat.toFixed(4)}°, ${l.lng.toFixed(4)}°</div>
    ${lost ? `<div class="notice">⌛ Destroyed in ${esc(fmtYear(l.destroyed))}. <button class="btn" data-year="${Math.round(((l.built ?? l.destroyed - 100) + l.destroyed) / 2)}">Travel back to see it</button></div>` : ''}
    ${notYet ? `<div class="notice">🏗️ Not built yet in ${esc(fmtYear(state.year))} — completed ${esc(fmtYear(l.built))}. <button class="btn" data-year="${l.built}">Jump to ${esc(fmtYear(l.built))}</button></div>` : ''}
    <p class="info-summary">${esc(l.summary)}</p>
    ${statGrid({ Built: l.built != null ? fmtYear(l.built) : null, ...(l.destroyed != null ? { Destroyed: fmtYear(l.destroyed) } : {}), 'Built by': l.builder, Height: l.height, ...(l.stats || {}) })}
    ${factList(l.facts)}${h.extract}
    <div class="info-actions">${closeupBtns(l.lat, l.lng)}${wikiLink(l.wiki)}</div></div>`);
  bindActions();
}
function showEvent(e) {
  markSelected('e:' + e.id);
  flyTo(e.lat, e.lng, 0.9);
  const h = heroWithWiki(e.wiki, EVENT_ICON[e.type] || '📍');
  openInfo(`${h.hero}<div class="info-main">
    <div class="info-kicker">Event · ${esc(e.type)}</div>
    <h2>${esc(EVENT_ICON[e.type] || '')} ${esc(e.name)}</h2>
    <div class="info-sub">🗓️ ${esc(fmtYear(e.year))} · ${e.lat.toFixed(2)}°, ${e.lng.toFixed(2)}°</div>
    <p class="info-summary">${esc(e.summary)}</p>${h.extract}
    <div class="info-actions"><button class="btn" data-year="${e.year}">⏳ Go to ${esc(fmtYear(e.year))}</button>${closeupBtns(e.lat, e.lng)}${wikiLink(e.wiki)}</div></div>`);
  bindActions();
}
function showChange(c) {
  const f = changeFactor(c);
  const z = c.zones?.[0];
  if (z) flyTo(z[0], z[1], 1.6);
  const pts = c.keys || [];
  openInfo(`<div class="info-hero noimg">${c.kind === 'ice' ? '🧊' : c.kind === 'water' ? '💧' : c.kind === 'land' ? '🌉' : c.kind === 'vegetation' ? '🌿' : '🪓'}</div><div class="info-main">
    <div class="info-kicker">Changing Earth · ${esc(c.kind)}</div>
    <h2>${esc(c.name)}</h2>
    <div class="info-sub">In ${esc(fmtYear(state.year))}: <b>${Math.round(f * 100)}%</b> of the mapped extent</div>
    <p class="info-summary">${esc(c.summary)}</p>
    <h4>Through time</h4>
    <div>${pts.map(([y, v]) => `<div class="producer" data-year="${y}"><span>⏳</span><span>${esc(fmtYear(y))}</span><span>${Math.round(v * 100)}%</span><div class="bar"><div style="width:${clamp(v * 100, 0, 100)}%;background:${(CHANGE_STYLE[c.kind] || CHANGE_STYLE.water).color}"></div></div></div>`).join('')}</div>
    <p class="muted small">Shapes are simplified ellipses scaled by area; click a year to travel there.</p></div>`);
  bindActions();
}
function showSite(r, s) {
  flyTo(s.lat, s.lng, 0.5);
  const i = r.sites.indexOf(s);
  markSelected(`r:${r.id}:${i}`);
  openInfo(`<div class="info-hero noimg">${r.emoji}</div><div class="info-main">
    <div class="info-kicker">${esc(r.name)} · ${esc(s.kind || 'Deposit')}</div>
    <h2>${esc(s.name)}</h2>
    <div class="info-sub">📍 ${esc(s.country)} · ${s.lat.toFixed(2)}°, ${s.lng.toFixed(2)}°</div>
    <p class="info-summary">${esc(s.info)}</p>
    <div class="info-actions">${closeupBtns(s.lat, s.lng)}<button class="btn" data-res="${r.id}">${r.emoji} About ${esc(r.name)}</button></div></div>`);
  bindActions();
  $('[data-res]', infoBody).onclick = () => showResource(r.id, false);
}
function showResource(id, doFly = true) {
  const r = RESOURCE[id];
  if (!r) return;
  if (doFly) fly({ altitude: 2.4 }, 1200);
  const h = heroWithWiki(RES_WIKI[id], r.emoji);
  const max = Math.max(...(r.producers || []).map((p) => p.share || 0), 1);
  openInfo(`${h.hero}<div class="info-main">
    <div class="info-kicker">Natural resource</div>
    <h2>${esc(r.emoji)} ${esc(r.name)}</h2>
    <p class="info-summary">${esc(r.summary)}</p>
    ${statGrid({ Reserves: r.reserves, Production: r.production })}
    ${r.uses?.length ? `<h4>Used for</h4><div class="filter-row">${r.uses.map((u) => `<span class="chip">${esc(u)}</span>`).join('')}</div>` : ''}
    <h4>Top producers</h4>
    ${(r.producers || []).map((p) => `<div class="producer" data-country="${esc(p.iso3)}"><span>${flagEmoji(COUNTRY_BY_ISO3[p.iso3]?.iso2)}</span><span>${esc(p.name)}</span><span>${p.share}%${p.value ? ` · <span class="muted">${esc(p.value)}</span>` : ''}</span><div class="bar"><div style="width:${(p.share / max) * 100}%;background:${r.color}"></div></div></div>`).join('')}
    ${factList(r.facts)}
    <h4>Major deposits & sites (${(r.sites || []).length})</h4>
    ${(r.sites || []).map((s, i) => `<div class="site-row" data-site="${r.id}|${i}"><span>${r.emoji}</span><div><b>${esc(s.name)}</b><small>${esc(s.kind || '')} · ${esc(s.country)}</small></div></div>`).join('')}
    ${h.extract}<div class="info-actions">${wikiLink(RES_WIKI[id])}</div></div>`);
  bindActions();
}
function flagEmoji(iso2) {
  if (!iso2 || iso2.length !== 2) return '🏳️';
  return String.fromCodePoint(...[...iso2.toUpperCase()].map((c) => 0x1f1a5 + c.charCodeAt(0)));
}
function countryCenter(c) {
  let best = null, area = -1;
  const polys = c.geometry.type === 'Polygon' ? [c.geometry.coordinates] : c.geometry.coordinates;
  polys.forEach((p) => {
    const ring = p[0];
    let minX = 999, maxX = -999, minY = 999, maxY = -999;
    ring.forEach(([x, y]) => { minX = Math.min(minX, x); maxX = Math.max(maxX, x); minY = Math.min(minY, y); maxY = Math.max(maxY, y); });
    const a = (maxX - minX) * (maxY - minY);
    if (a > area) { area = a; best = { lat: (minY + maxY) / 2, lng: (minX + maxX) / 2, span: Math.max(maxX - minX, maxY - minY) }; }
  });
  return best;
}
function flyToCountry(c) { const ce = countryCenter(c); if (ce) flyTo(ce.lat, ce.lng, clamp(ce.span / 28, 0.35, 2.2)); }
function showCountry(c) {
  const p = c.props;
  const wikiTitle = { 'United States of America': 'United States', 'Dem. Rep. Congo': 'Democratic Republic of the Congo', 'Central African Rep.': 'Central African Republic', 'S. Sudan': 'South Sudan', 'Bosnia and Herz.': 'Bosnia and Herzegovina', 'Dominican Rep.': 'Dominican Republic', 'Eq. Guinea': 'Equatorial Guinea', 'Solomon Is.': 'Solomon Islands', 'Falkland Is.': 'Falkland Islands', 'Fr. S. Antarctic Lands': 'French Southern and Antarctic Lands', 'W. Sahara': 'Western Sahara', 'N. Cyprus': 'Northern Cyprus', 'Côte d\'Ivoire': 'Ivory Coast', 'eSwatini': 'Eswatini', Macedonia: 'North Macedonia', Georgia: 'Georgia (country)' }[c.name] || c.longName;
  const inCountry = (x) => (x.country || '').split(/[,/;]| and /).map((s) => s.trim()).some((n) => n && (n === c.name || n === c.longName || n === wikiTitle));
  const ws = WONDERS.filter(inCountry), ls = LANDMARKS.filter(inCountry);
  const res = RESOURCES.map((r) => ({ r, p: (r.producers || []).find((x) => x.iso3 === c.iso3) })).filter((x) => x.p);
  const h = heroWithWiki(wikiTitle, flagEmoji(c.iso2));
  openInfo(`${h.hero}<div class="info-main">
    <div class="info-kicker">Country · ${esc(p.CONTINENT || '')}</div>
    <h2>${c.iso2 ? `<img src="https://flagcdn.com/w80/${c.iso2.toLowerCase()}.png" alt="" style="height:22px;border-radius:3px;vertical-align:-2px;margin-right:6px">` : ''}${esc(c.longName)}</h2>
    <div class="info-sub">${esc(p.SUBREGION || '')}${p.FORMAL_EN ? ` · ${esc(p.FORMAL_EN)}` : ''}</div>
    ${statGrid({ Population: p.POP_EST ? fmtNum(p.POP_EST) + (p.POP_YEAR ? ` (${p.POP_YEAR})` : '') : null, GDP: p.GDP_MD ? '$' + fmtNum(p.GDP_MD * 1e6) + (p.GDP_YEAR ? ` (${p.GDP_YEAR})` : '') : null, Economy: (p.ECONOMY || '').replace(/^\d+\.\s*/, ''), Income: (p.INCOME_GRP || '').replace(/^\d+\.\s*/, '') })}
    ${res.length ? `<h4>Major producer of</h4>${res.map(({ r, p: pp }) => `<div class="site-row" data-resid="${r.id}"><span>${r.emoji}</span><div><b>${esc(r.name)}</b><small>${pp.share}% of world production${pp.value ? ' · ' + esc(pp.value) : ''}</small></div></div>`).join('')}` : ''}
    ${ws.length ? `<h4>Natural wonders</h4>${ws.map((w) => `<div class="site-row" data-item="w:${w.id}"><span>${w.emoji}</span><div><b>${esc(w.name)}</b><small>${esc(w.type)}</small></div></div>`).join('')}` : ''}
    ${ls.length ? `<h4>Landmarks</h4>${ls.map((l) => `<div class="site-row" data-item="l:${l.id}"><span>${l.emoji}</span><div><b>${esc(l.name)}</b><small>${esc(l.category)} · ${esc(fmtYear(l.built))}</small></div></div>`).join('')}` : ''}
    ${h.extract}<div class="info-actions">${wikiLink(wikiTitle)}</div></div>`);
  bindActions();
  $$('[data-resid]', infoBody).forEach((b) => (b.onclick = () => { toggleResource(b.dataset.resid, true); showResource(b.dataset.resid); }));
}
function showPolity(d) {
  const title = d.name;
  const h = heroWithWiki(title, '🏳️');
  openInfo(`${h.hero}<div class="info-main">
    <div class="info-kicker">Historical state · borders c. ${esc(fmtYear(state.histYear))}</div>
    <h2>${esc(d.name)}</h2>
    ${d.subject && d.subject !== d.name ? `<div class="info-sub">Part of / subject to: <b>${esc(d.subject)}</b></div>` : ''}
    ${h.extract}
    <p class="muted small">Historical borders from the historical-basemaps project (A. Ourednik). Pre-modern borders are approximate and often contested.</p>
    <div class="info-actions">${wikiLink(title)}</div></div>`);
  bindActions();
}
function showAnimal(id, hotspot) {
  const a = ANIMAL[id];
  if (!a) return;
  if (!state.animals.has(id)) toggleAnimal(id, true);
  if (hotspot) flyTo(hotspot.lat, hotspot.lng, 0.6);
  else { const z = animalZones(a).zones[0]; if (z) flyTo(z[0], z[1], 1.9); }
  const alive = animalAlive(a);
  const w = heroWithWiki(a.wiki, '');
  const sel = [...state.animals];
  openInfo(`<div id="vslot"></div><div class="info-main">
    <div class="info-kicker">${esc(a.group)} <span class="status-pill st-${esc(a.status)}" title="IUCN Red List">${esc(STATUS[a.status] || a.status)}</span></div>
    <h2>${esc(a.emoji)} ${esc(a.name)}</h2>
    <div class="info-sub"><i>${esc(a.scientific)}</i> · Population: ${esc(a.population)}</div>
    ${!alive ? `<div class="notice">💀 Extinct since ${esc(fmtYear(a.extinct))}. Its last range is shown faded. <button class="btn" data-year="${Math.max(-20000, a.extinct - (a.extinct > 0 ? 60 : 1500))}">⏳ Travel back to see it alive</button></div>` : ''}
    ${a.pastRange && state.year >= a.pastRange.until ? `<div class="notice">🗺️ Before ${esc(fmtYear(a.pastRange.until))} this species lived across a much larger area. <button class="btn" data-year="${a.pastRange.until - 50}">See its historic range</button></div>` : ''}
    <p class="info-summary">${esc(a.summary)}</p>
    ${statGrid({ Habitat: a.habitat, Diet: a.diet, Size: a.size, Lifespan: a.lifespan, 'Top speed': a.speed })}
    ${factList(a.facts)}
    ${a.hotspots?.length ? `<h4>Where to see it</h4>${a.hotspots.map((hh) => `<div class="site-row" data-fly="${hh.lat},${hh.lng},0.4"><span>📍</span><div><b>${esc(hh.name)}</b><small>${hh.lat.toFixed(2)}°, ${hh.lng.toFixed(2)}°</small></div></div>`).join('')}` : ''}
    ${w.hero.replace('info-hero', 'info-hero" style="border-radius:12px;margin-top:14px;height:180px')}
    ${w.extract}
    <div class="info-actions">${wikiLink(a.wiki)}</div></div>`);
  // 3D viewer
  $('#vslot').replaceWith(viewerHost);
  if (!viewer) viewer = new AnimalViewer(viewerHost);
  viewerHost.classList.toggle('sea', MARINE.has(id));
  $('.v-loading', viewerHost).classList.remove('hidden');
  viewer.show(id).then(() => $('.v-loading', viewerHost).classList.add('hidden')).catch(() => ($('.v-loading', viewerHost).textContent = '3D model unavailable'));
  const tabs = $('.v-tabs', viewerHost);
  tabs.innerHTML = sel.length > 1 ? sel.map((s) => `<button class="${s === id ? 'active' : ''}" data-a="${s}" title="${esc(ANIMAL[s]?.name)}">${ANIMAL[s]?.emoji}</button>`).join('') : '';
  $$('button', tabs).forEach((b) => (b.onclick = () => showAnimal(b.dataset.a)));
  bindActions();
}
function openByKey(key) {
  const [k, id] = key.split(':');
  if (k === 'w') showWonder(WONDERS.find((x) => x.id === id));
  else if (k === 'l') {
    const l = LANDMARKS.find((x) => x.id === id);
    if (!exists(l)) setYear(l.destroyed != null && l.destroyed <= state.year ? Math.round(((l.built ?? l.destroyed - 100) + l.destroyed) / 2) : l.built, true);
    showLandmark(l);
  } else if (k === 'a') showAnimal(id);
  else if (k === 'r') { toggleResource(id, true); showResource(id); }
  else if (k === 'e') { const e = EVENTS.find((x) => x.id === id); setYear(e.year, true); showEvent(e); }
  else if (k === 'c') { const c = COUNTRIES.find((x) => x.iso3 === id); showCountry(c); flyToCountry(c); }
  else if (k === 'ch') showChange(CHANGES.find((x) => x.id === id));
}

// ============================================================ sidebar
function toggleAnimal(id, on = !state.animals.has(id)) {
  on ? state.animals.add(id) : state.animals.delete(id);
  buildRangePolys(); refreshPolygons(); refreshMarkers(); refreshMinis(); renderLegend();
  if (state.tab === 'animals') renderSide();
}
function toggleResource(id, on = !state.resources.has(id)) {
  if (on) { state.resources.delete(id); state.resources.add(id); } else state.resources.delete(id);
  globe.polygonCapColor(polyCap);
  refreshPolygons(); refreshMarkers(); renderLegend();
  if (state.tab === 'resources') renderSide();
}
const sideContent = $('#side-content');
const listFilter = { wonders: '', landmarks: '', animals: '', resources: '', history: '' };
function switchHtml(key, on) { return `<label class="switch" title="Show on globe"><input type="checkbox" data-layer="${key}" ${on ? 'checked' : ''}><span></span></label>`; }
function filterInput(tab, ph) { return `<input class="list-filter" data-filter="${tab}" placeholder="${ph}" value="${esc(listFilter[tab])}">`; }
const matches = (tab, ...s) => !listFilter[tab] || s.join(' ').toLowerCase().includes(listFilter[tab].toLowerCase());
function renderSide() {
  const tab = state.tab;
  let html = '';
  if (tab === 'wonders') {
    const types = [...new Set(WONDERS.map((w) => w.type))].sort();
    html = `<div class="side-head"><h3>🏔️ Natural Wonders</h3>${switchHtml('wonders', state.layers.wonders)}</div>
      <p class="side-desc">${WONDERS.length} of the planet's most spectacular places. Click one to fly there and learn about it.</p>
      <div class="filter-row"><button class="chip ${!state.wonderTypes ? 'on' : ''}" data-wtype="*">All</button>${types.map((t) => `<button class="chip ${state.wonderTypes?.has(t) ? 'on' : ''}" data-wtype="${esc(t)}">${esc(t)}</button>`).join('')}</div>
      ${filterInput('wonders', 'Filter wonders…')}
      ${WONDERS.filter((w) => (!state.wonderTypes || state.wonderTypes.has(w.type)) && matches('wonders', w.name, w.country, w.type)).sort((a, b) => a.name.localeCompare(b.name)).map((w) => `<div class="item" data-open="w:${w.id}"><span class="em">${w.emoji}</span><span class="nm">${esc(w.name)}<small>${esc(w.type)} · ${esc(w.country)}</small></span></div>`).join('')}`;
  } else if (tab === 'landmarks') {
    const cats = [...new Set(LANDMARKS.map((l) => l.category))].sort();
    html = `<div class="side-head"><h3>🏛️ Landmarks</h3>${switchHtml('landmarks', state.layers.landmarks)}</div>
      <p class="side-desc">${LANDMARKS.length} famous buildings and monuments. Lost wonders reappear when you travel back in time.</p>
      <div class="filter-row"><button class="chip ${!state.landmarkCats ? 'on' : ''}" data-lcat="*">All</button>${cats.map((t) => `<button class="chip ${state.landmarkCats?.has(t) ? 'on' : ''}" data-lcat="${esc(t)}">${esc(t)}</button>`).join('')}</div>
      ${filterInput('landmarks', 'Filter landmarks…')}
      ${LANDMARKS.filter((l) => (!state.landmarkCats || state.landmarkCats.has(l.category)) && matches('landmarks', l.name, l.city, l.country, l.category)).sort((a, b) => a.name.localeCompare(b.name)).map((l) => {
        const tag = l.destroyed != null && l.destroyed <= state.year ? `<span class="tag lost">lost ${esc(fmtYear(l.destroyed))}</span>` : l.built > state.year ? `<span class="tag">built ${esc(fmtYear(l.built))}</span>` : '';
        return `<div class="item" data-open="l:${l.id}"><span class="em">${l.emoji}</span><span class="nm">${esc(l.name)}<small>${esc([l.city, l.country].filter(Boolean).join(', '))}</small></span>${tag}</div>`;
      }).join('')}`;
  } else if (tab === 'animals') {
    const groups = [...new Set(ANIMALS.map((a) => a.group))];
    html = `<div class="side-head"><h3>🐾 Animals</h3><button class="chip" data-clear="animals">Clear</button></div>
      <p class="side-desc">Tick one or many species to paint where they live and place animated 3D models on the globe. Click a name for its 3D model and facts.</p>
      ${filterInput('animals', 'Filter animals…')}
      ${groups.map((g) => {
        const list = ANIMALS.filter((a) => a.group === g && matches('animals', a.name, a.scientific, g));
        if (!list.length) return '';
        return `<div class="group-title">${esc(g)} <button data-allgroup="${esc(g)}">select all</button></div>` + list.map((a) => `<div class="item ${state.animals.has(a.id) ? 'selected' : ''}" style="--item-color:${a.color}">
          <input type="checkbox" data-animal-cb="${a.id}" ${state.animals.has(a.id) ? 'checked' : ''}>
          <span class="em">${a.emoji}</span><span class="nm" data-open="a:${a.id}">${esc(a.name)}<small><i>${esc(a.scientific)}</i></small></span>
          ${a.extinct != null ? `<span class="tag ex">extinct</span>` : `<span class="swatch" style="background:${a.color}"></span>`}</div>`).join('');
      }).join('')}`;
  } else if (tab === 'resources') {
    html = `<div class="side-head"><h3>⛏️ Natural Resources</h3><button class="chip" data-clear="resources">Clear</button></div>
      <p class="side-desc">Choose a resource to see its biggest deposits and colour the top producing countries (the most recently picked resource colours the map).</p>
      ${RESOURCES.map((r) => `<div class="item ${state.resources.has(r.id) ? 'selected' : ''}" style="--item-color:${r.color}">
        <input type="checkbox" data-res-cb="${r.id}" ${state.resources.has(r.id) ? 'checked' : ''}>
        <span class="em">${r.emoji}</span><span class="nm" data-open="r:${r.id}">${esc(r.name)}<small>${(r.sites || []).length} sites · top: ${esc(r.producers?.[0]?.name || '')}</small></span>
        <span class="swatch" style="background:${r.color}"></span></div>`).join('')}`;
  } else if (tab === 'history') {
    const evs = EVENTS.filter((e) => matches('history', e.name, e.type, String(e.year))).sort((a, b) => a.year - b.year);
    html = `<div class="side-head"><h3>⏳ History & Change</h3></div>
      <p class="side-desc">Drag the timeline below to travel from the last Ice Age to today. Borders, ice sheets, coastlines, lakes, forests, landmarks and wildlife change with the year.</p>
      <div class="item"><span class="em">📍</span><span class="nm">Historic events on the globe</span>${switchHtml('events', state.layers.events)}</div>
      <div class="item"><span class="em">🧊</span><span class="nm">Ice sheets, lakes, land bridges & forests</span>${switchHtml('changes', state.layers.changes)}</div>
      <div class="group-title">Eras</div>
      ${ERAS.map((e) => `<div class="item" data-goyear="${Math.max(e.from, -20000)}"><span class="em">🗓️</span><span class="nm">${esc(e.title)}<small>${esc(fmtYear(e.from))} – ${esc(fmtYear(Math.min(e.to, NOW)))}</small></span></div>`).join('')}
      <div class="group-title">Changing landscapes</div>
      ${CHANGES.map((c) => `<div class="item" data-open="ch:${c.id}"><span class="em">${c.kind === 'ice' ? '🧊' : c.kind === 'water' ? '💧' : c.kind === 'land' ? '🌉' : c.kind === 'vegetation' ? '🌿' : '🪓'}</span><span class="nm">${esc(c.name)}<small>${esc(c.kind)}</small></span></div>`).join('')}
      <div class="group-title">Events (${evs.length})</div>
      ${filterInput('history', 'Filter events…')}
      ${evs.map((e) => `<div class="item" data-open="e:${e.id}"><span class="em">${EVENT_ICON[e.type] || '📍'}</span><span class="nm">${esc(e.name)}<small>${esc(fmtYear(e.year))} · ${esc(e.type)}</small></span></div>`).join('')}`;
  }
  sideContent.innerHTML = html;
}
sideContent.addEventListener('click', (ev) => {
  const t = ev.target;
  const open = t.closest('[data-open]');
  const cbA = t.closest('[data-animal-cb]'), cbR = t.closest('[data-res-cb]');
  if (cbA) return toggleAnimal(cbA.dataset.animalCb, cbA.checked);
  if (cbR) return toggleResource(cbR.dataset.resCb, cbR.checked);
  if (t.matches('[data-layer]')) {
    state.layers[t.dataset.layer] = t.checked;
    if (t.dataset.layer === 'changes') { buildChangePolys(); refreshPolygons(); }
    refreshMarkers();
    return;
  }
  const wt = t.closest('[data-wtype]'), lc = t.closest('[data-lcat]');
  if (wt || lc) {
    const key = wt ? 'wonderTypes' : 'landmarkCats', v = wt ? wt.dataset.wtype : lc.dataset.lcat;
    if (v === '*') state[key] = null;
    else { const s = state[key] || new Set(); s.has(v) ? s.delete(v) : s.add(v); state[key] = s.size ? s : null; }
    renderSide(); refreshMarkers();
    return;
  }
  const clr = t.closest('[data-clear]');
  if (clr) {
    if (clr.dataset.clear === 'animals') { state.animals.clear(); buildRangePolys(); refreshMinis(); }
    else state.resources.clear();
    globe.polygonCapColor(polyCap); refreshPolygons(); refreshMarkers(); renderLegend(); renderSide();
    return;
  }
  const ag = t.closest('[data-allgroup]');
  if (ag) { ANIMALS.filter((a) => a.group === ag.dataset.allgroup).forEach((a) => state.animals.add(a.id)); buildRangePolys(); refreshPolygons(); refreshMarkers(); refreshMinis(); renderLegend(); renderSide(); return; }
  const gy = t.closest('[data-goyear]');
  if (gy) return setYear(+gy.dataset.goyear, true);
  if (open) {
    const key = open.dataset.open;
    if (key.startsWith('r:')) { toggleResource(key.slice(2), true); showResource(key.slice(2)); }
    else openByKey(key);
  }
});
sideContent.addEventListener('input', (ev) => {
  const f = ev.target.closest('[data-filter]');
  if (!f) return;
  listFilter[f.dataset.filter] = f.value;
  const pos = f.selectionStart;
  renderSide();
  const nf = $(`[data-filter="${f.dataset.filter}"]`, sideContent);
  nf.focus(); nf.setSelectionRange(pos, pos);
});
$$('.side-tab').forEach((b) => (b.onclick = () => {
  state.tab = b.dataset.tab;
  $$('.side-tab').forEach((x) => x.classList.toggle('active', x === b));
  renderSide();
  sideContent.scrollTop = 0;
}));
$('#menu-toggle').onclick = () => $('#sidebar').classList.toggle('collapsed');

// ============================================================ legend
function renderLegend() {
  const lg = $('#legend');
  const parts = [];
  if (state.animals.size) {
    parts.push('<div class="lg-title">Animal ranges</div>' + [...state.animals].map((id) => { const a = ANIMAL[id]; return a ? `<div class="lg-row" data-lg-animal="${id}"><span class="swatch" style="background:${a.color}"></span>${a.emoji} ${esc(a.name)}${!animalAlive(a) ? ' <span class="muted">(extinct)</span>' : ''}</div>` : ''; }).join(''));
  }
  const ch = currentChoropleth();
  if (ch) {
    parts.push(`<div class="lg-title" style="margin-top:8px">${ch.r.emoji} ${esc(ch.r.name)} – share of world production</div><div class="lg-grad" style="background:linear-gradient(90deg, ${hexA(ch.r.color, 0.25)}, ${hexA(ch.r.color, 0.85)})"></div><div style="display:flex;justify-content:space-between" class="muted"><span>low</span><span>${ch.max}%</span></div>${state.histFeatures ? '<div class="muted" style="margin-top:4px">Production data is for today, so countries are coloured only on present-day maps (2011+).</div>' : ''}`);
    if (state.resources.size > 1) parts.push([...state.resources].map((id) => `<div class="lg-row" data-lg-res="${id}">${RESOURCE[id].emoji} ${esc(RESOURCE[id].name)} sites</div>`).join(''));
  }
  lg.innerHTML = parts.join('');
  lg.classList.toggle('hidden', !parts.length);
  $$('[data-lg-animal]', lg).forEach((r) => (r.onclick = () => showAnimal(r.dataset.lgAnimal)));
  $$('[data-lg-res]', lg).forEach((r) => (r.onclick = () => showResource(r.dataset.lgRes)));
}

// ============================================================ search
const searchIndex = () => [
  ...WONDERS.map((w) => ({ key: 'w:' + w.id, name: w.name, sub: w.country, emoji: w.emoji, kind: 'Wonder' })),
  ...LANDMARKS.map((l) => ({ key: 'l:' + l.id, name: l.name, sub: [l.city, l.country].filter(Boolean).join(', '), emoji: l.emoji, kind: 'Landmark' })),
  ...ANIMALS.map((a) => ({ key: 'a:' + a.id, name: a.name, sub: a.scientific, emoji: a.emoji, kind: 'Animal' })),
  ...RESOURCES.map((r) => ({ key: 'r:' + r.id, name: r.name, sub: 'Natural resource', emoji: r.emoji, kind: 'Resource' })),
  ...COUNTRIES.map((c) => ({ key: 'c:' + c.iso3, name: c.longName, sub: c.props.CONTINENT, emoji: flagEmoji(c.iso2), kind: 'Country' })),
  ...EVENTS.map((e) => ({ key: 'e:' + e.id, name: e.name, sub: fmtYear(e.year), emoji: EVENT_ICON[e.type] || '📍', kind: 'Event' })),
];
let SEARCH = null, srActive = 0;
const sInput = $('#search'), sRes = $('#search-results');
sInput.addEventListener('input', () => {
  SEARCH = SEARCH || searchIndex();
  const q = sInput.value.trim().toLowerCase();
  if (!q) return sRes.classList.add('hidden');
  const hits = SEARCH.map((x) => {
    const n = x.name.toLowerCase();
    const score = n.startsWith(q) ? 0 : n.includes(q) ? 1 : (x.sub || '').toLowerCase().includes(q) ? 2 : 9;
    return { x, score };
  }).filter((h) => h.score < 9).sort((a, b) => a.score - b.score || a.x.name.localeCompare(b.x.name)).slice(0, 14);
  srActive = 0;
  sRes.innerHTML = hits.length ? hits.map((h, i) => `<div class="sr-item ${i === 0 ? 'active' : ''}" data-key="${h.x.key}"><span class="sr-emoji">${h.x.emoji}</span><span>${esc(h.x.name)}<br><small class="muted">${esc(h.x.sub || '')}</small></span><span class="sr-kind">${h.x.kind}</span></div>`).join('') : '<div class="sr-item muted">No matches</div>';
  sRes.classList.remove('hidden');
});
sInput.addEventListener('keydown', (ev) => {
  const items = $$('.sr-item[data-key]', sRes);
  if (ev.key === 'ArrowDown' || ev.key === 'ArrowUp') {
    ev.preventDefault();
    srActive = clamp(srActive + (ev.key === 'ArrowDown' ? 1 : -1), 0, items.length - 1);
    items.forEach((it, i) => it.classList.toggle('active', i === srActive));
  } else if (ev.key === 'Enter' && items[srActive]) { pickSearch(items[srActive].dataset.key); }
  else if (ev.key === 'Escape') sRes.classList.add('hidden');
});
sRes.addEventListener('click', (ev) => { const it = ev.target.closest('[data-key]'); if (it) pickSearch(it.dataset.key); });
document.addEventListener('click', (ev) => { if (!ev.target.closest('.search-wrap')) sRes.classList.add('hidden'); });
function pickSearch(key) { sRes.classList.add('hidden'); sInput.value = ''; sInput.blur(); openByKey(key); }

// ============================================================ timeline UI
const slider = $('#tl-slider');
function renderTicks() {
  const ticks = window.innerWidth < 760 ? [-20000, -5000, 1, 1000, 1500, 1900, NOW] : [-20000, -10000, -5000, -3000, -1000, 1, 500, 1000, 1500, 1800, 1900, 1950, 2000, NOW];
  $('#tl-ticks').innerHTML = ticks.map((y) => `<span style="left:${yearToPos(y) / 10}%">${y === NOW ? 'Now' : y < 0 ? (-y >= 1000 ? -y / 1000 + 'k BC' : -y + ' BC') : y === 1 ? 'AD 1' : y}</span>`).join('');
}
function sparkline(points) {
  const pts = points.filter(([x]) => x >= -20000 && x <= NOW);
  if (pts.length < 2) return '';
  const ys = pts.map((p) => p[1]), lo = Math.min(...ys), hi = Math.max(...ys);
  const X = (x) => (yearToPos(x) / 1000) * 54, Y = (v) => 24 - ((v - lo) / (hi - lo || 1)) * 22;
  const cur = interp(pts, state.year);
  return `<svg viewBox="0 0 54 26"><polyline fill="none" stroke="rgba(140,200,255,.8)" stroke-width="1.3" points="${pts.map(([x, v]) => `${X(x).toFixed(1)},${Y(v).toFixed(1)}`).join(' ')}"/>${cur != null ? `<circle cx="${X(state.year).toFixed(1)}" cy="${Y(cur).toFixed(1)}" r="2.6" fill="#fff"/>` : ''}</svg>`;
}
const STAT_FMT = {
  population: (v) => fmtNum(v), co2: (v) => Math.round(v) + ' ppm', temp: (v) => (v > 0 ? '+' : '') + v.toFixed(2) + ' °C',
  seaLevel: (v) => (v > 0 ? '+' : '') + (Math.abs(v) < 2 ? v.toFixed(2) : Math.round(v)) + ' m', seaIce: (v) => v.toFixed(2) + ' M km²', forest: (v) => v.toFixed(2) + ' B ha',
};
function renderStats() {
  const html = Object.entries(STATS).map(([k, s]) => {
    const last = s.points[s.points.length - 1];
    const stale = last && state.year > last[0];
    const v = stale ? last[1] : interp(s.points, state.year);
    const now = last ? last[1] : null;
    const f = STAT_FMT[k] || ((x) => fmtNum(x, s.unit ? ' ' + s.unit : ''));
    let d = '';
    if (v != null && now != null && state.year < last[0] - 0.5) {
      const diff = now - v;
      const rel = Math.abs(diff) / (Math.abs(v) || 1);
      const fd = k === 'population' ? fmtNum(Math.abs(diff)) : k === 'temp' || k === 'seaLevel' ? Math.abs(diff).toFixed(2) + (k === 'temp' ? ' °C' : ' m') : k === 'co2' ? Math.round(Math.abs(diff)) + ' ppm' : Math.abs(diff).toFixed(2);
      d = `<span class="d ${rel < 0.005 ? 'flat' : diff > 0 ? 'up' : 'down'}">${diff > 0 ? '▲' : '▼'} ${fd} by ${last[0]}</span>`;
    } else if (v != null) d = `<span class="d flat">${stale ? 'latest data: ' + last[0] : 'latest'}</span>`;
    else d = `<span class="d flat">no data before ${fmtYear(s.points[0][0])}</span>`;
    return `<div class="tl-stat" title="${esc(s.label)}"><span class="k">${esc(s.label)}</span>${sparkline(s.points)}<span class="v">${v != null ? f(v) : '—'} ${d}</span></div>`;
  }).join('');
  $('#tl-stats').innerHTML = html;
}
function renderYearLabel() {
  const y = state.year;
  const ago = NOW - y;
  $('#tl-year').innerHTML = `${esc(fmtYear(y))}<small>${y >= NOW ? 'today' : ago === 1 ? '1 year ago' : `${ago.toLocaleString('en-US')} years ago`}${state.histYear != null ? ` · borders c. ${esc(fmtYear(state.histYear))}` : ''}</small>`;
  const era = ERAS.find((e) => y >= e.from && y <= e.to) || ERAS[ERAS.length - 1];
  $('#tl-era').innerHTML = era ? `<b>${esc(era.title)}</b> — ${esc(era.text)}` : '';
  $('#tl-era').title = era ? era.text : '';
  const evs = visibleEvents().slice(0, 14).sort((a, b) => a.year - b.year);
  $('#tl-events').innerHTML = evs.map((e) => `<span class="ev-chip" data-ev="${e.id}">${EVENT_ICON[e.type] || '📍'} ${esc(fmtYear(e.year))} · ${esc(e.name)}</span>`).join('');
}
$('#tl-events').addEventListener('click', (ev) => { const c = ev.target.closest('[data-ev]'); if (c) showEvent(EVENTS.find((e) => e.id === c.dataset.ev)); });

let yearRaf = null, lastLandmarkSig = '';
function setYear(y, syncSlider = false, opts = {}) {
  state.year = clamp(Math.round(y), -20000, NOW);
  if (state.year === 0) state.year = 1;
  if (syncSlider) slider.value = yearToPos(state.year);
  if (yearRaf) return;
  yearRaf = requestAnimationFrame(() => {
    yearRaf = null;
    renderYearLabel();
    renderStats();
    buildChangePolys();
    buildRangePolys();
    refreshPolygons();
    refreshMarkers();
    const sig = [...state.animals].map((id) => animalAlive(ANIMAL[id])).join();
    if (sig !== lastLandmarkSig) { lastLandmarkSig = sig; refreshMinis(); renderLegend(); }
    loadHistoric();
    if (state.tab === 'landmarks' && !opts.playing) renderSide();
  });
}
slider.addEventListener('input', () => setYear(posToYear(+slider.value)));
$$('.tl-jumps button').forEach((b) => (b.onclick = () => setYear(+b.dataset.jump, true)));
let playTimer = null;
$('#tl-play').onclick = () => {
  if (playTimer) { clearInterval(playTimer); playTimer = null; $('#tl-play').textContent = '▶'; renderSide(); return; }
  if (+slider.value >= 1000) slider.value = 0;
  $('#tl-play').textContent = '⏸';
  playTimer = setInterval(() => {
    const p = +slider.value + 2;
    slider.value = p;
    setYear(posToYear(p), false, { playing: true });
    if (p >= 1000) $('#tl-play').click();
  }, 90);
};

// ============================================================ top controls
$('#basemap').onchange = (e) => setBasemap(e.target.value);
$('#toggle-borders').onclick = (e) => { state.borders = !state.borders; e.currentTarget.classList.toggle('on', state.borders); refreshPolygons(); globe.polygonStrokeColor(globe.polygonStrokeColor()); };
$('#toggle-labels').onclick = (e) => { state.labels = !state.labels; e.currentTarget.classList.toggle('on', state.labels); document.body.classList.toggle('show-labels', state.labels); };
$('#toggle-rotate').onclick = (e) => { const c = globe.controls(); c.autoRotate = !c.autoRotate; c.autoRotateSpeed = 0.35; e.currentTarget.classList.toggle('on', c.autoRotate); };
const help = $('#help');
$('#help-btn').onclick = () => help.classList.remove('hidden');
$$('.modal-close', help).forEach((b) => (b.onclick = () => help.classList.add('hidden')));
help.onclick = (e) => { if (e.target === help) help.classList.add('hidden'); };
document.addEventListener('keydown', (e) => { if (e.key === 'Escape') { help.classList.add('hidden'); closeInfo(); } });
window.addEventListener('resize', debounce(() => { globe.width(window.innerWidth).height(window.innerHeight); renderTicks(); }, 150));

// ============================================================ boot
document.body.classList.add('show-labels');
renderTicks();
renderSide();
setYear(NOW, true);
onZoom(globe.pointOfView());
try { if (!localStorage.getItem('ee-visited')) { help.classList.remove('hidden'); localStorage.setItem('ee-visited', '1'); } } catch { /* storage unavailable */ }
setTimeout(() => $('#loader').classList.add('fade'), 600);
setTimeout(() => $('#loader').remove(), 1600);
window.__earth = { fly, globe, state, setYear, showAnimal, toggleAnimal, toggleResource, showResource, openByKey };

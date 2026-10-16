import { Globe } from '../vendor/earth-libs.js';
import { THREE, createAnimal, FLYING } from './animals3d.js';
import { icon } from './icons.js';
import { createReader } from './reader.js';
import { createCountries } from './countries.js';
import { createNews } from './news.js';
import { createLearn } from './learn.js';
import { createLive, gibsTiles } from './live.js';

// ============================================================ helpers
const $ = (s, r = document) => r.querySelector(s);
const $$ = (s, r = document) => [...r.querySelectorAll(s)];
const esc = (s) => String(s ?? '').replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
const clamp = (v, a, b) => Math.max(a, Math.min(b, v));
const TEX = 'https://cdn.jsdelivr.net/npm/three-globe@2.45.1/example/img/';
const HIST_URL = (y) => `https://cdn.jsdelivr.net/gh/aourednik/historical-basemaps@master/geojson/world_${y < 0 ? 'bc' + -y : y}.geojson`;
const NOW = 2026;
const REDUCED_MOTION = matchMedia('(prefers-reduced-motion: reduce)').matches;
const debounce = (fn, ms) => { let t; return (...a) => { clearTimeout(t); t = setTimeout(() => fn(...a), ms); }; };
function hexA(hex, a) {
  const c = new THREE.Color(hex);
  return `rgba(${Math.round(c.r * 255)},${Math.round(c.g * 255)},${Math.round(c.b * 255)},${a})`;
}
function hashColor(str, s = 55, l = 55) {
  let h = 0;
  for (const ch of String(str)) h = (h * 31 + ch.charCodeAt(0)) >>> 0;
  return `hsla(${h % 360}, ${s}%, ${l}%, 0.33)`;
}
function fmtYear(y) {
  y = Math.round(y);
  if (y < 0) return `${(-y).toLocaleString('en-US')} BC`;
  if (y === 0) return '1 BC';
  if (y < 1000) return `AD ${y}`;
  return `${y}`;
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
function toast(msg, ms = 3200) {
  const t = $('#toast');
  t.textContent = msg;
  t.classList.remove('hidden');
  clearTimeout(toast._t);
  toast._t = setTimeout(() => t.classList.add('hidden'), ms);
}
const fillIcons = (root = document) => $$('[data-icon]', root).forEach((el) => (el.outerHTML = icon(el.dataset.icon)));

// ============================================================ data
async function safeImport(path) {
  try { return await import(path); } catch (e) { console.warn('Could not load', path, e); return {}; }
}
const [wMod, lMod, aMod, rMod, tMod, countriesGeo, toursMod] = await Promise.all([
  safeImport('./data/wonders.js'),
  safeImport('./data/landmarks.js'),
  safeImport('./data/animals.js'),
  safeImport('./data/resources.js'),
  safeImport('./data/timeline.js'),
  fetch(new URL('../assets/data/countries.geojson', import.meta.url)).then((r) => r.json()),
  safeImport('./data/tours.js'),
]);
const TOURS = toursMod.TOURS || [];
const WONDERS = wMod.WONDERS || [];
const LANDMARKS = lMod.LANDMARKS || [];
const ANIMALS = aMod.ANIMALS || [];
const RESOURCES = rMod.RESOURCES || [];
const { STATS = {}, ERAS = [], EVENTS = [], CHANGES = [], HIST_MAPS = [] } = tMod;
const byId = (arr) => Object.fromEntries(arr.map((x) => [x.id, x]));
const ANIMAL = byId(ANIMALS), RESOURCE = byId(RESOURCES), WONDER = byId(WONDERS), LANDMARK = byId(LANDMARKS);
EVENTS.forEach((e, i) => (e.id = e.id || 'ev' + i));
const EVENT_BY_ID = byId(EVENTS);
const CURATED_WIKI = new Set([...WONDERS, ...LANDMARKS].map((x) => (x.wiki || '').replace(/_/g, ' ').toLowerCase()));

const COUNTRIES = countriesGeo.features.map((f) => {
  const p = f.properties;
  const iso3 = p.ISO_A3 !== '-99' ? p.ISO_A3 : p.ADM0_A3;
  const iso2 = p.ISO_A2 !== '-99' ? p.ISO_A2 : p.ISO_A2_EH && p.ISO_A2_EH !== '-99' ? p.ISO_A2_EH : null;
  return { kind: 'country', geometry: f.geometry, name: p.NAME, longName: p.NAME_LONG || p.NAME, iso3, iso2, props: p };
});
const COUNTRY_BY_ISO3 = Object.fromEntries(COUNTRIES.map((c) => [c.iso3, c]));
['France:FRA', 'Norway:NOR'].forEach((s) => { const [n, code] = s.split(':'); if (!COUNTRY_BY_ISO3[code]) { const f = COUNTRIES.find((c) => c.name === n); if (f) { f.iso3 = code; COUNTRY_BY_ISO3[code] = f; } } });

const STATUS = { LC: 'Least Concern', NT: 'Near Threatened', VU: 'Vulnerable', EN: 'Endangered', CR: 'Critically Endangered', EW: 'Extinct in the Wild', EX: 'Extinct', DD: 'Data Deficient' };
const RES_WIKI = { oil: 'Petroleum', 'natural-gas': 'Natural_gas', coal: 'Coal', gold: 'Gold', silver: 'Silver', copper: 'Copper', 'iron-ore': 'Iron_ore', lithium: 'Lithium', cobalt: 'Cobalt', nickel: 'Nickel', uranium: 'Uranium', diamonds: 'Diamond', 'rare-earths': 'Rare-earth_element', platinum: 'Platinum', bauxite: 'Bauxite', phosphate: 'Phosphorite' };
const RES_SYMBOL = { oil: 'Oil', 'natural-gas': 'Gas', coal: 'Coal', gold: 'Au', silver: 'Ag', copper: 'Cu', 'iron-ore': 'Fe', lithium: 'Li', cobalt: 'Co', nickel: 'Ni', uranium: 'U', diamonds: 'Dia', 'rare-earths': 'REE', platinum: 'Pt', bauxite: 'Al', phosphate: 'P' };
const WONDER_ICON = { Canyon: 'mountain', Mountain: 'mountain-snow', Volcano: 'flame', Waterfall: 'droplets', Reef: 'fish', Rainforest: 'trees', Desert: 'sun', 'Glacier & Ice': 'snowflake', Lake: 'waves', River: 'waves', Cave: 'mountain', Island: 'tree-palm', 'Rock Formation': 'mountain', 'Geyser & Hot Spring': 'cloud-fog', 'Natural Phenomenon': 'sparkles', 'Coast & Fjord': 'sailboat', 'Valley & Plateau': 'mountain' };
const LANDMARK_ICON = { 'Ancient Wonder': 'gem', 'Ancient Site': 'landmark', Religious: 'church', 'Palace & Castle': 'castle', 'Tower & Skyscraper': 'building-2', 'Monument & Statue': 'flag', 'Bridge & Engineering': 'milestone', 'Museum & Culture': 'library', 'Modern Architecture': 'building', Fortification: 'shield' };
const EVENT_ICON = { eruption: 'flame', earthquake: 'activity', tsunami: 'waves', climate: 'thermometer', civilization: 'scroll', exploration: 'compass', discovery: 'lightbulb', disaster: 'triangle-alert', conflict: 'swords', space: 'rocket', environment: 'leaf' };
const EVENT_COLOR = { eruption: '#ff7a4d', earthquake: '#f5a524', tsunami: '#5aaef5', climate: '#ff8f8f', civilization: '#e2a2f2', exploration: '#6fdcbf', discovery: '#ffd75e', disaster: '#ff6464', conflict: '#e05555', space: '#9db1ff', environment: '#7fdc8c' };
const CHANGE_STYLE = {
  ice: { color: '#eef7ff', a: 0.78, icon: 'snowflake' }, land: { color: '#b4915c', a: 0.72, icon: 'mountain' }, water: { color: '#3b9cff', a: 0.62, icon: 'droplets' },
  vegetation: { color: '#62c450', a: 0.48, icon: 'leaf' }, loss: { color: '#ff6b3d', a: 0.55, icon: 'trees' },
};
const C_WONDER = '#7fd18f', C_LANDMARK = '#f0b54a', C_WIKI = '#c9d3ea';
const wonderIcon = (w) => WONDER_ICON[w.type] || 'mountain';
const landmarkIcon = (l) => LANDMARK_ICON[l.category] || 'landmark';

// ============================================================ state
const state = {
  year: NOW,
  basemap: 'satellite',
  borders: true,
  labels: true,
  nearby: true,
  layers: { wonders: true, landmarks: true, events: true, changes: true },
  wonderTypes: null,
  landmarkCats: null,
  animals: new Set(),
  resources: new Set(),
  hoverCountry: null,
  histYear: null,
  histFeatures: null,
  tab: 'wonders',
  altitude: 2.5,
  selectedKey: null,
  pickMode: false,
  quizCountry: null,
  guess: null,
};

let news = null, reloadNews = null, pickHandler = null, live = null, reloadLive = null; // feature modules created after the globe is set up

// ============================================================ globe
const globeEl = $('#globe');
const globe = new Globe(globeEl, { rendererConfig: { antialias: true, alpha: true, logarithmicDepthBuffer: true } })
  .backgroundImageUrl(TEX + 'night-sky.png')
  .showAtmosphere(true)
  .atmosphereColor('#7fb6e8')
  .atmosphereAltitude(0.16)
  .showGraticules(false);
globe.controls().minDistance = 100.008;
globe.controls().maxDistance = 1200;
globe.globeTileEngineMaxLevel(19);
globe.pointOfView({ lat: 22, lng: 12, altitude: 2.6 });

const BASEMAPS = {
  satellite: { tiles: (x, y, l) => `https://server.arcgisonline.com/ArcGIS/rest/services/World_Imagery/MapServer/tile/${l}/${y}/${x}`, credit: 'Imagery © Esri, Maxar, Earthstar Geographics' },
  topo: { tiles: (x, y, l) => `https://server.arcgisonline.com/ArcGIS/rest/services/World_Topo_Map/MapServer/tile/${l}/${y}/${x}`, credit: 'Map © Esri, HERE, Garmin, USGS, NGA' },
  streets: { tiles: (x, y, l) => `https://server.arcgisonline.com/ArcGIS/rest/services/World_Street_Map/MapServer/tile/${l}/${y}/${x}`, credit: 'Map © Esri, HERE, Garmin, OpenStreetMap contributors' },
  bluemarble: { img: TEX + 'earth-blue-marble.jpg', bump: TEX + 'earth-topology.png', credit: 'NASA Blue Marble' },
  night: { img: TEX + 'earth-night.jpg', bump: TEX + 'earth-topology.png', credit: 'NASA Earth at Night' },
  gibs: { dynamic: true, credit: 'Imagery: NASA GIBS / Worldview, MODIS Terra' },
  blackmarble: { tiles: (x, y, l) => `https://gibs.earthdata.nasa.gov/wmts/epsg3857/best/VIIRS_Black_Marble/default/2016-01-01/GoogleMapsCompatible_Level8/${l}/${y}/${x}.png`, max: 8, credit: 'NASA Black Marble 2016 (VIIRS night lights)' },
  seasons: { dynamic: 'seasons', max: 8, credit: 'NASA Blue Marble Next Generation, 2004' },
  temperature: { dynamic: 'lst', max: 7, credit: 'NASA MODIS Terra land surface temperature (day)' },
};
function setBasemap(key) {
  if (!BASEMAPS[key]) key = 'satellite';
  state.basemap = key;
  let b = BASEMAPS[key];
  if (b.dynamic === 'seasons') {
    const mm = String(state.month || new Date().getMonth() + 1).padStart(2, '0');
    b = { ...b, tiles: (x, y, l) => `https://gibs.earthdata.nasa.gov/wmts/epsg3857/best/BlueMarble_NextGeneration/default/2004-${mm}-01/GoogleMapsCompatible_Level8/${l}/${y}/${x}.jpeg`, credit: `${b.credit}, ${['January','February','March','April','May','June','July','August','September','October','November','December'][+mm - 1]}` };
  } else if (b.dynamic === 'lst') {
    const date = state.year >= 2001 && state.year < 2026 ? `${state.year}-07-15` : new Date(Date.now() - 3 * 864e5).toISOString().slice(0, 10);
    b = { ...b, tiles: (x, y, l) => `https://gibs.earthdata.nasa.gov/wmts/epsg3857/best/MODIS_Terra_Land_Surface_Temp_Day/default/${date}/GoogleMapsCompatible_Level7/${l}/${y}/${x}.png`, credit: `${b.credit}, ${date}` };
  } else if (b.dynamic) {
    const date = live?.gibsDateFor(state.year);
    if (!date) { toast('NASA daily imagery starts in February 2000. Showing the satellite mosaic instead.'); return setBasemap('satellite'); }
    b = { ...b, tiles: gibsTiles(date), credit: `${b.credit}, ${date}` };
    state.gibsDate = date;
  }
  globe.globeTileEngineMaxLevel(key === 'gibs' ? 9 : b.max || 19);
  if (b.tiles) globe.globeImageUrl(null).bumpImageUrl(null).globeTileEngineUrl(b.tiles);
  else globe.globeTileEngineUrl(null).globeImageUrl(b.img).bumpImageUrl(b.bump || null);
  // The tile engine only fetches on camera change, so nudge it after swapping sources.
  [80, 700].forEach((t) => setTimeout(() => globe.controls().dispatchEvent({ type: 'change' }), t));
  $('#attribution').textContent = `${b.credit} · Data: Natural Earth, Wikipedia, NOAA, NASA`;
  $('#basemap').value = key;
  writeHash();
}

// Programmatic flights: re-notify layers (tile engine) once the camera has settled.
function fly(pov, ms = 1600) {
  ms = REDUCED_MOTION ? 0 : ms;
  globe.pointOfView(pov, ms);
  const c = globe.controls();
  [ms + 60, ms + 600].forEach((t) => setTimeout(() => c.dispatchEvent({ type: 'change' }), t));
}
const flyTo = (lat, lng, altitude = 0.9, ms = 1600) => fly({ lat, lng, altitude }, ms);

// ---- zoom-dependent tuning
let altScale = 1;
const zoomFade = () => clamp(state.altitude / 1.2, 0.3, 1);
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
  queueNearby();
  live?.onZoom(pov.altitude);
}
globe.onZoom(onZoom);

// ============================================================ geometry
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
  for (let i = 0; i <= 48; i++) {
    const a = -(i / 48) * Math.PI * 2; // clockwise ring
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
const lostMidYear = (l) => Math.round(((l.built ?? l.destroyed - 100) + l.destroyed) / 2);
function eventWindow(y) { return y < -5000 ? 1500 : y < -1000 ? 400 : y < 500 ? 150 : y < 1500 ? 60 : y < 1900 ? 20 : y < 2000 ? 6 : 3; }
function visibleEvents(y = state.year) {
  const w = eventWindow(y);
  return EVENTS.filter((e) => Math.abs(e.year - y) <= w).sort((a, b) => Math.abs(a.year - y) - Math.abs(b.year - y));
}
const changeFactor = (c, y = state.year) => clamp(interpClamped(c.keys, y), 0, 1.3);
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
function currentChoropleth() {
  const ids = [...state.resources];
  const r = ids.length ? RESOURCE[ids[ids.length - 1]] : null;
  if (!r) return null;
  const map = {};
  (r.producers || []).forEach((p) => (map[p.iso3] = p));
  return { r, map, max: Math.max(...(r.producers || []).map((p) => p.share || 0), 1) };
}
function countryPolys() {
  if (state.histFeatures && state.histYear != null) return state.borders ? state.histFeatures : [];
  if (!state.borders && !currentChoropleth()) return [];
  return COUNTRIES;
}
const refreshPolygons = () => globe.polygonsData([...countryPolys(), ...changeObjs, ...rangeObjs]);
function polyCap(d) {
  if (d.kind === 'country') {
    if (state.quizCountry) return d.iso3 === state.quizCountry ? 'rgba(240, 181, 74, 0.62)' : 'rgba(255,255,255,0.02)';
    const ch = currentChoropleth();
    if (ch && !state.histFeatures) {
      const p = ch.map[d.iso3];
      if (p) return hexA(ch.r.color, 0.25 + 0.6 * Math.sqrt((p.share || 0) / ch.max));
    }
    if (d === state.hoverCountry) return 'rgba(240, 181, 74, 0.2)';
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
    if (state.quizCountry || state.pickMode) return '';
    if (d.kind === 'country') {
      const ch = currentChoropleth();
      const p = ch && !d.hist ? ch.map[d.iso3] : null;
      return `<b>${esc(d.name)}</b>${d.hist ? `<br><span style="opacity:.75">Borders c. ${esc(fmtYear(state.histYear))}</span>` : ''}${p ? `<br>${esc(ch.r.name)}: ${p.share}% of world production` : ''}`;
    }
    if (d.kind === 'range') return `<b>${esc(d.animal.name)}</b><br><span style="opacity:.75">${d.past ? 'Historic range' : d.faded ? 'Former range (extinct)' : 'Approximate range'}</span>`;
    if (d.kind === 'change') return `<b>${esc(d.change.name)}</b><br><span style="opacity:.75">${Math.round(d.factor * 100)}% of its mapped extent in ${esc(fmtYear(state.year))}</span>`;
    return '';
  })
  .onPolygonHover((d) => {
    const prev = state.hoverCountry;
    state.hoverCountry = d && d.kind === 'country' ? d : null;
    if (prev !== state.hoverCountry) globe.polygonCapColor(polyCap).polygonAltitude(globe.polygonAltitude());
    globeEl.style.cursor = d ? 'pointer' : null;
  })
  .onPolygonClick((d, ev, pt) => {
    if (pickHandler) { if (pt) pickHandler({ lat: pt.lat, lng: pt.lng }); return; }
    if (state.quizCountry) return;
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
          kind: 'country', hist: true, geometry: f.geometry, name: f.properties.NAME, subject: f.properties.SUBJECTO, color: hashColor(f.properties.SUBJECTO || f.properties.NAME, 60, 55),
        }))));
    }
    const feats = await histCache.get(mapYear);
    if (state.year >= 2011) return;
    state.histFeatures = feats;
    state.histYear = mapYear;
    refreshPolygons();
    renderLegend();
    renderYearLabel();
  } catch (e) {
    histCache.delete(mapYear);
    console.warn('historical borders failed', e);
    toast('Historical borders could not be loaded. Check your connection and move the timeline to retry.');
  }
}, 220);

// ============================================================ html markers
const markerEls = new Map();
function markerEl(d) {
  let el = markerEls.get(d.key);
  if (!el) {
    el = document.createElement('div');
    el.className = 'marker' + (d.small ? ' small' : '') + (d.cls ? ' ' + d.cls : '');
    el.setAttribute('role', 'button');
    el.tabIndex = 0;
    el.setAttribute('aria-label', d.label);
    el.innerHTML = `<div class="dot">${d.symbol ? `<b>${esc(d.symbol)}</b>` : icon(d.icon)}</div><div class="lbl">${esc(d.label)}</div>`;
    el.style.setProperty('--mc', d.color);
    el.addEventListener('click', (ev) => { ev.stopPropagation(); d.open(); });
    el.addEventListener('keydown', (ev) => { if (ev.key === 'Enter' || ev.key === ' ') { ev.preventDefault(); d.open(); } });
    el.addEventListener('pointerdown', (ev) => ev.stopPropagation());
    markerEls.set(d.key, el);
  }
  el.classList.toggle('selected', state.selectedKey === d.key);
  return el;
}
function buildMarkers() {
  const m = [];
  if (state.guess) {
    m.push({ key: 'q:guess:' + state.guess.guess.lat, lat: state.guess.guess.lat, lng: state.guess.guess.lng, icon: 'crosshair', label: 'Your answer', color: '#9db1ff', open: () => {} });
    m.push({ key: 'q:truth:' + state.guess.truth.lat, lat: state.guess.truth.lat, lng: state.guess.truth.lng, icon: 'map-pin', label: state.guess.truth.name, color: '#f0b54a', open: () => {} });
  }
  if (state.pickMode || state.guess) return m;
  if (state.layers.wonders) {
    WONDERS.filter((w) => !state.wonderTypes || state.wonderTypes.has(w.type)).forEach((w) =>
      m.push({ key: 'w:' + w.id, lat: w.lat, lng: w.lng, icon: wonderIcon(w), label: w.name, color: C_WONDER, open: () => showWonder(w) }));
  }
  if (state.layers.landmarks) {
    LANDMARKS.filter((l) => (!state.landmarkCats || state.landmarkCats.has(l.category)) && exists(l)).forEach((l) =>
      m.push({ key: 'l:' + l.id, lat: l.lat, lng: l.lng, icon: landmarkIcon(l), label: l.name, color: C_LANDMARK, open: () => showLandmark(l) }));
  }
  if (state.layers.events) {
    visibleEvents().forEach((e) =>
      m.push({ key: 'e:' + e.id, lat: e.lat, lng: e.lng, icon: EVENT_ICON[e.type] || 'map-pin', label: `${e.name} (${fmtYear(e.year)})`, color: EVENT_COLOR[e.type] || '#fff', cls: 'event', open: () => showEvent(e) }));
  }
  state.resources.forEach((rid) => {
    const r = RESOURCE[rid];
    (r?.sites || []).forEach((s, i) => m.push({ key: `r:${rid}:${i}`, lat: s.lat, lng: s.lng, symbol: RES_SYMBOL[rid], label: s.name, color: r.color, small: true, open: () => showSite(r, s) }));
  });
  state.animals.forEach((id) => {
    const a = ANIMAL[id];
    (a?.hotspots || []).forEach((h, i) => m.push({ key: `a:${id}:${i}`, lat: h.lat, lng: h.lng, icon: 'paw-print', label: `${a.name} · ${h.name}`, color: a.color, small: true, open: () => showAnimal(id, h) }));
  });
  if (state.tab === 'news' && news) {
    news.pins().slice(0, 150).forEach((e, i) => m.push({ key: 'n:' + (e.lat.toFixed(3) + e.lng.toFixed(3) + i), lat: e.lat, lng: e.lng, icon: 'newspaper', label: (e.paper || e.text.replace(/<[^>]+>/g, '')).slice(0, 80), color: '#e9e3d2', small: true, cls: 'news', open: () => (e.kind === 'paper' ? news.state.items.indexOf(e) >= 0 && openPaper(e) : showNewsItem(e)) }));
  }
  if (live) {
    const L = live.layers();
    L.eonet.forEach((e) => m.push({ key: 'eo:' + e.id, lat: e.lat, lng: e.lng, icon: e.icon, label: e.title, color: '#ff8a7a', small: true, open: () => showEonet(e) }));
    if (L.iss) m.push({ key: 'iss', lat: L.iss.lat, lng: L.iss.lng, icon: 'rocket', label: `ISS · ${Math.round(L.iss.alt)} km up`, color: '#9db1ff', open: () => showIss(L.iss) });
  }
  if (state.nearby && state.altitude < NEARBY_ALT) {
    nearbyVisible().forEach((p) => m.push({ key: 'p:' + p.pageid, lat: p.lat, lng: p.lng, icon: 'book-open', label: p.title, color: C_WIKI, cls: 'wiki', open: () => showWikiPlace(p) }));
  }
  return m;
}
globe.htmlElement(markerEl).htmlLat('lat').htmlLng('lng').htmlAltitude(() => (altScale < 0.01 ? 0 : 0.004 * altScale)).htmlTransitionDuration(0);
function refreshMarkers() {
  globe.htmlElementsData(buildMarkers());
  globe.ringsData(state.layers.events && !REDUCED_MOTION ? visibleEvents().slice(0, 12) : [])
    .ringColor((e) => (t) => hexA(EVENT_COLOR[e.type] || '#ffffff', 1 - t))
    .ringMaxRadius(3.5).ringPropagationSpeed(2.2).ringRepeatPeriod(1300).ringAltitude(0.002);
}

// ============================================================ nearby places (Wikipedia geosearch)
const NEARBY_ALT = 0.008; // ≈ 50 km camera height
const nearbyPlaces = new Map();
let nearbyCenter = null;
const nearbyFetched = new Set();
function nearbyVisible() {
  if (!nearbyCenter) return [];
  const span = Math.max(0.05, state.altitude * 57 * 1.2);
  // thin dense cities on a zoom-aware grid so markers don't pile up (nearest to the view centre win)
  const cell = span / 16, seen = new Set(), out = [];
  const near = [...nearbyPlaces.values()]
    .map((p) => ({ p, d: Math.hypot(p.lat - nearbyCenter.lat, (p.lng - nearbyCenter.lng) * Math.cos((p.lat * Math.PI) / 180)) }))
    .filter((x) => x.d < span)
    .sort((a, b) => a.d - b.d);
  for (const { p } of near) {
    const k = Math.floor(p.lat / cell) + ':' + Math.floor(p.lng / cell);
    if (seen.has(k)) continue;
    seen.add(k);
    out.push(p);
    if (out.length >= 160) break;
  }
  return out;
}
const queueNearby = debounce(async () => {
  if (!state.nearby || state.altitude >= NEARBY_ALT) { if (nearbyCenter) { nearbyCenter = null; refreshMarkers(); } return; }
  const { lat, lng } = globe.pointOfView();
  nearbyCenter = { lat, lng };
  const key = `${lat.toFixed(2)},${lng.toFixed(2)}`;
  if (!nearbyFetched.has(key)) {
    nearbyFetched.add(key);
    const radius = Math.round(clamp(state.altitude * 6371000 * 0.6, 1500, 10000));
    try {
      const j = await getJSON(`https://en.wikipedia.org/w/api.php?action=query&list=geosearch&gscoord=${lat.toFixed(5)}|${lng.toFixed(5)}&gsradius=${radius}&gslimit=500&format=json&origin=*`);
      if (!j) throw new Error('geosearch unavailable');
      (j.query?.geosearch || []).forEach((g) => {
        if (CURATED_WIKI.has(g.title.toLowerCase())) return;
        nearbyPlaces.set(g.pageid, { pageid: g.pageid, title: g.title, lat: g.lat, lng: g.lon });
      });
    } catch (e) {
      nearbyFetched.delete(key);
      console.warn('nearby search failed', e);
    }
  }
  refreshMarkers();
}, 500);

// ============================================================ 3D animal miniatures
const miniCache = new Map();
let miniList = [];
async function refreshMinis() {
  const wanted = [];
  state.animals.forEach((id) => {
    const a = ANIMAL[id];
    if (!a || !animalAlive(a)) return;
    (a.hotspots || []).slice(0, 2).forEach((h, i) => wanted.push({ key: id + '#' + i, id, lat: h.lat, lng: h.lng, name: a.name }));
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
  const list = [];
  for (const w of wanted) { const v = await miniCache.get(w.key); if (v) list.push(v); }
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
  if (!REDUCED_MOTION) miniList.forEach((m) => m.inst.update(t, dt));
})();

// ============================================================ Wikipedia text & photos
const wikiCache = new Map(), photoCache = new Map();
const wikiTitle = (t) => encodeURIComponent(String(t).replace(/ /g, '_'));
/** fetch JSON with a short backoff when Wikimedia rate-limits (429/503). Resolves null on failure. */
async function getJSON(url, tries = 3) {
  for (let i = 0; i < tries; i++) {
    let wait = 0;
    try {
      const r = await fetch(url);
      if (r.ok) return await r.json();
      if (r.status !== 429 && r.status !== 503) return null;
      wait = Math.min(10, +r.headers.get('retry-after') || 0) * 1000;
    } catch { /* network error: retry */ }
    await new Promise((res) => setTimeout(res, Math.max(wait, 800 * 2 ** i)));
  }
  return null;
}
function wiki(title) {
  if (!title) return Promise.resolve(null);
  if (!wikiCache.has(title)) wikiCache.set(title, getJSON(`https://en.wikipedia.org/api/rest_v1/page/summary/${wikiTitle(title)}`));
  return wikiCache.get(title);
}
const big = (src) => src.replace(/\/(\d+)px-/, '/960px-');
const SKIP_IMG = /(\.svg|map|locator|location|flag|logo|icon|coat[_ ]of[_ ]arms|emblem|seal|signature|range|distribution|diagram|chart|graph|plan\b|blank)/i;
/** Real photographs for an article: the lead image plus its gallery (Wikimedia Commons). */
function photos(title, keywords = null) {
  if (!title) return Promise.resolve([]);
  if (!photoCache.has(title)) {
    photoCache.set(title, Promise.all([
      wiki(title),
      getJSON(`https://en.wikipedia.org/api/rest_v1/page/media-list/${wikiTitle(title)}`),
    ]).then(([sum, media]) => {
      const out = [], seen = new Set();
      const fileKey = (src) => decodeURIComponent((src.split('/').filter((s) => /\.(jpe?g|png|webp|tiff?)$/i.test(s))[0] || src).replace(/^\d+px-/, ''));
      const lead = sum?.thumbnail?.source;
      if (lead && !/\.svg/i.test(lead)) {
        seen.add(fileKey(lead));
        out.push({ thumb: lead, full: big(lead), fallback: lead, caption: sum.description || sum.title, file: null });
      }
      (media?.items || []).forEach((it) => {
        if (it.type !== 'image' || it.showInGallery === false || SKIP_IMG.test(it.title || '')) return;
        const src = it.srcset?.[0]?.src;
        if (!src) return;
        const url = src.startsWith('//') ? 'https:' + src : src;
        const k = fileKey(url);
        if (seen.has(k)) { if (out[0] && !out[0].file) out[0].file = it.title; return; }
        seen.add(k);
        out.push({ thumb: url, full: big(url), fallback: url, caption: it.caption?.text || '', file: it.title });
      });
      if (!sum && !media) photoCache.delete(title);
      if (keywords?.length) {
        // articles about a species often show relatives too; prefer photos that name this one
        const rx = new RegExp(keywords.map((k) => k.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')).join('|'), 'i');
        const own = out.filter((p, i) => i === 0 || rx.test((p.file || '') + ' ' + (p.caption || '')));
        if (own.length >= 3) return own.slice(0, 12);
      }
      return out.slice(0, 12);
    }));
  }
  return photoCache.get(title);
}
/** Hero photo + thumbnail strip, filled asynchronously. */
function gallery(title, name, fallbackIcon, keywords = null) {
  const id = 'g' + Math.random().toString(36).slice(2);
  photos(title, keywords).then((list) => {
    const root = document.getElementById(id);
    if (!root) return;
    const fig = $('.hero', root), strip = $('.thumbs', root);
    if (!list.length) { $('.hero-empty', fig).innerHTML = icon(fallbackIcon); return; }
    const show = (i) => {
      const p = list[i];
      let img = $('img', fig);
      if (!img) { img = document.createElement('img'); fig.prepend(img); img.addEventListener('click', () => openLightbox(list, +img.dataset.i, name)); }
      img.classList.remove('ready');
      img.dataset.i = i;
      img.alt = p.caption ? `${name}: ${p.caption.slice(0, 140)}` : `Photo of ${name}`;
      img.onload = () => img.classList.add('ready');
      img.onerror = () => { if (img.src !== p.fallback) img.src = p.fallback; };
      img.src = p.full;
      $('.hero-empty', fig).innerHTML = '';
      $('figcaption', fig).innerHTML = `<span>${list.length > 1 ? `${i + 1} / ${list.length} · ` : ''}Wikimedia Commons</span>${p.file ? `<a href="https://commons.wikimedia.org/wiki/${wikiTitle(p.file)}" target="_blank" rel="noopener">Author &amp; licence</a>` : ''}`;
      $$('button', strip).forEach((b, k) => b.setAttribute('aria-current', String(k === i)));
    };
    if (list.length > 1) {
      strip.innerHTML = list.map((p, i) => `<button aria-label="Show photo ${i + 1} of ${list.length}"><img src="${esc(p.thumb)}" alt="" loading="lazy"></button>`).join('');
      $$('button', strip).forEach((b, i) => (b.onclick = () => show(i)));
    }
    show(0);
  });
  return `<div id="${id}"><figure class="hero"><div class="hero-empty"></div><figcaption></figcaption></figure><div class="thumbs"></div></div>`;
}
function wikiExtract(title) {
  const id = 'x' + Math.random().toString(36).slice(2);
  wiki(title).then((w) => {
    const el = document.getElementById(id);
    if (el && w?.extract) el.innerHTML = `<h3>From Wikipedia</h3><p class="wiki-extract">${esc(w.extract)}</p>`;
  });
  return `<div id="${id}"></div>`;
}

// ---- lightbox
const lb = $('#lightbox');
let lbList = [], lbIdx = 0, lbName = '', lbReturn = null;
function lbShow(i) {
  lbIdx = (i + lbList.length) % lbList.length;
  const p = lbList[lbIdx];
  let img = $('img', lb);
  if (!img) { img = document.createElement('img'); $('.lb-bar', lb).before(img); }
  img.onerror = () => { if (img.src !== p.fallback) img.src = p.fallback; };
  img.src = p.full;
  img.alt = p.caption ? `${lbName}: ${p.caption}` : `Photo of ${lbName}`;
  $('.lb-bar', lb).innerHTML = `<span>${esc(lbName)}${p.caption ? ' · ' + esc(p.caption.slice(0, 200)) : ''}</span><span>${lbIdx + 1} / ${lbList.length}</span>${p.file ? `<a href="https://commons.wikimedia.org/wiki/${wikiTitle(p.file)}" target="_blank" rel="noopener">Author &amp; licence on Wikimedia Commons</a>` : ''}${p.link ? `<a href="${esc(p.link)}" target="_blank" rel="noopener">${esc(p.linkText || 'Source')}</a>` : ''}`;
  $$('.lb-nav', lb).forEach((b) => b.classList.toggle('hidden', lbList.length < 2));
}
function openLightbox(list, i, name) {
  lbList = list; lbName = name; lbReturn = document.activeElement;
  lb.classList.remove('hidden');
  lbShow(i);
  $('.lb-close', lb).focus();
}
function closeLightbox() { lb.classList.add('hidden'); lbReturn?.focus?.(); }
$('.lb-close', lb).onclick = closeLightbox;
$('.lb-prev', lb).onclick = () => lbShow(lbIdx - 1);
$('.lb-next', lb).onclick = () => lbShow(lbIdx + 1);
lb.addEventListener('click', (e) => { if (e.target === lb) closeLightbox(); });

// ============================================================ info card
const welcome = $('#welcome');
const info = $('#info'), infoBody = $('#info-body');
function openInfo(html, key) {
  if (!welcome.classList.contains('hidden')) hideWelcome();
  infoBody.innerHTML = html;
  info.classList.remove('hidden');
  info.scrollTop = 0;
  if (key !== undefined) markSelected(key);
  if (window.innerWidth <= 760) $('#sidebar').classList.add('collapsed');
  bindActions();
}
function closeInfo() {
  info.classList.add('hidden');
  markSelected(null);
}
$('#info-close').onclick = () => (learn.active() ? learn.stop() : closeInfo());
function figures(obj) {
  const e = Object.entries(obj || {}).filter(([, v]) => v != null && v !== '');
  return e.length ? `<dl class="figures">${e.map(([k, v]) => `<dt>${esc(k)}</dt><dd>${esc(v)}</dd>`).join('')}</dl>` : '';
}
const factList = (f, title = 'Did you know?') => (f?.length ? `<h3>${title}</h3><ul class="facts">${f.map((x) => `<li>${esc(x)}</li>`).join('')}</ul>` : '');
const meta = (iconName, color, parts) => `<div class="meta" style="--c:${color}">${icon(iconName)}${parts.filter(Boolean).map((p, i) => (i ? '<span class="sep">·</span>' : '') + `<span>${p}</span>`).join('')}</div>`;
const wikiLink = (title) => (title ? `<a class="btn" href="https://en.wikipedia.org/wiki/${wikiTitle(title)}" target="_blank" rel="noopener">${icon('book-open')}Read on Wikipedia</a>` : '');
const closeupBtns = (lat, lng) => `<button class="btn primary" data-closeup="${lat},${lng}">${icon('satellite')}Satellite close-up</button><button class="btn" data-fly="${lat},${lng},0.35">${icon('crosshair')}Fly here</button>`;
const hereBtn = (lat, lng, km, name) => `<button class="btn" data-here="${lat},${lng},${km}" data-here-name="${esc(name)}">${icon('hourglass')}What happened here?</button>`;
const hereCache = new Map();
/** "What happened here?": dated events, foundings and constructions near a point, from Wikidata. */
async function showHere(lat, lng, km, name) {
  const key = `${lat.toFixed(3)},${lng.toFixed(3)},${km}`;
  openInfo(`<div class="info-main"><h2>What happened here?</h2>${meta('hourglass', '#e2a2f2', [esc(name), `within ${km} km`])}<p class="muted">Searching Wikidata for dated events and places around ${esc(name)}. This can take up to half a minute…</p></div>`, null);
  if (!hereCache.has(key)) {
    const q = `SELECT ?e ?eLabel ?eDescription ?date ?k ?article ?links ?c WHERE {
      SERVICE wikibase:around { ?e wdt:P625 ?c . bd:serviceParam wikibase:center "Point(${lng} ${lat})"^^geo:wktLiteral . bd:serviceParam wikibase:radius "${km}" . }
      { ?e wdt:P585 ?date BIND("event" AS ?k) } UNION { ?e wdt:P580 ?date BIND("began" AS ?k) } UNION { ?e wdt:P571 ?date BIND("founded" AS ?k) }
      ?article schema:about ?e ; schema:isPartOf <https://en.wikipedia.org/> .
      ?e wikibase:sitelinks ?links .
      SERVICE wikibase:label { bd:serviceParam wikibase:language "en". }
    } ORDER BY DESC(?links) LIMIT 160`;
    hereCache.set(key, getJSON(`https://query.wikidata.org/sparql?format=json&query=${encodeURIComponent(q)}`, 2).then((j) => {
      if (!j) { hereCache.delete(key); return null; }
      const seen = new Set();
      return (j.results?.bindings || []).map((b) => {
        const raw = parseInt(b.date.value, 10); // astronomical numbering: 0 = 1 BC, -43 = 44 BC
        const year = raw <= 0 ? raw - 1 : raw;
        const pt = /Point\(([-\d.]+) ([-\d.]+)\)/.exec(b.c?.value || '');
        return { name: b.eLabel.value, desc: b.eDescription?.value || '', year, kind: b.k.value, title: decodeURIComponent(b.article.value.split('/wiki/')[1]).replace(/_/g, ' '), lat: pt ? +pt[2] : null, lng: pt ? +pt[1] : null };
      }).filter((x) => !isNaN(x.year) && !seen.has(x.name + x.year) && seen.add(x.name + x.year)).sort((a, b) => a.year - b.year);
    }));
  }
  const rows = await hereCache.get(key);
  if (!info.querySelector('.info-main h2')?.textContent.startsWith('What happened here')) return; // the learner moved on
  if (!rows) { openInfo(`<div class="info-main"><h2>What happened here?</h2><p class="muted">Wikidata did not answer in time. <button class="btn" data-here="${lat},${lng},${km}" data-here-name="${esc(name)}">${icon('refresh-cw')}Try again</button></p></div>`, null); return; }
  const era = (y) => (y < -3000 ? 'Prehistory' : y < 500 ? 'Antiquity' : y < 1500 ? 'Middle Ages' : y < 1800 ? 'Early modern' : y < 1945 ? '1800–1945' : 'Since 1945');
  let last = '';
  openInfo(`<div class="info-main"><h2>What happened here?</h2>
    ${meta('hourglass', '#e2a2f2', [esc(name), `within ${km} km`, `${rows.length} dated records`])}
    <p class="muted small">Events, foundings and constructions with a date and a location in Wikidata, the most widely documented first. Choose a year to travel there, or a name to read about it.</p>
    ${rows.map((r) => { const head = era(r.year) !== last ? `<h3>${(last = era(r.year))}</h3>` : ''; return `${head}<div class="here-row"><button class="here-year num" data-year="${r.year}">${esc(fmtYear(r.year))}</button><div><button class="here-name" data-read="" data-title="${esc(r.title)}">${esc(r.name)}</button><small>${r.kind === 'founded' ? 'Founded or built' : r.kind === 'began' ? 'Began' : 'Event'}${r.desc ? ' · ' + esc(r.desc) : ''}</small></div></div>`; }).join('') || '<p class="muted">No dated records found nearby. Try a larger area.</p>'}
    <p class="muted small">Data: Wikidata (CC0).</p></div>`, null);
}
const readBtn = (key, title) => (title ? `<button class="btn" data-read="${esc(key)}" data-title="${esc(String(title).replace(/_/g, ' '))}">${icon('book-open')}Read full article</button>` : '');
const kmBetween = (a, b) => { const R = 6371, dLat = ((b.lat - a.lat) * Math.PI) / 180, dLng = ((b.lng - a.lng) * Math.PI) / 180; const s = Math.sin(dLat / 2) ** 2 + Math.cos((a.lat * Math.PI) / 180) * Math.cos((b.lat * Math.PI) / 180) * Math.sin(dLng / 2) ** 2; return 2 * R * Math.asin(Math.sqrt(s)); };
function relatedTo(pt, selfKey, limit = 6) {
  if (!pt || pt.lat == null) return [];
  return [...WONDERS.map((w) => ({ key: 'w:' + w.id, name: w.name, lat: w.lat, lng: w.lng })), ...LANDMARKS.map((l) => ({ key: 'l:' + l.id, name: l.name, lat: l.lat, lng: l.lng }))]
    .filter((x) => x.key !== selfKey).map((x) => ({ ...x, km: kmBetween(pt, x) })).filter((x) => x.km < 1500).sort((a, b) => a.km - b.km).slice(0, limit);
}
/** Margin data for the article reader, from our own curated datasets. */
function itemFor(key) {
  if (!key) return null;
  const i = key.indexOf(':'), k = key.slice(0, i), id = key.slice(i + 1);
  if (k === 'w' && WONDER[id]) { const w = WONDER[id]; return { kind: `Natural wonder · ${w.type}`, place: w.country, lat: w.lat, lng: w.lng, figures: { ...(w.stats || {}), 'UNESCO World Heritage': w.unesco || null }, dates: [w.unesco ? { year: w.unesco, label: 'Inscribed as UNESCO World Heritage' } : null].filter(Boolean), related: relatedTo(w, key) }; }
  if (k === 'l' && LANDMARK[id]) { const l = LANDMARK[id]; return { kind: `Landmark · ${l.category}`, place: [l.city, l.country].filter(Boolean).join(', '), lat: l.lat, lng: l.lng, figures: { 'Built by': l.builder, Height: l.height, ...(l.stats || {}) }, dates: [l.built != null ? { year: l.built, label: 'Completed' } : null, l.destroyed != null ? { year: l.destroyed, label: 'Destroyed' } : null].filter(Boolean), related: relatedTo(l, key) }; }
  if (k === 'a' && ANIMAL[id]) { const a = ANIMAL[id], h = a.hotspots?.[0]; return { kind: `Animal · ${a.group}`, place: a.habitat, lat: h?.lat, lng: h?.lng, figures: { 'IUCN status': STATUS[a.status] || a.status, Population: a.population, Size: a.size, Lifespan: a.lifespan, 'Top speed': a.speed, Diet: a.diet }, dates: [a.extinct != null ? { year: a.extinct, label: 'Last known alive' } : null, a.pastRange ? { year: a.pastRange.until - 50, label: 'Historic range still intact' } : null].filter(Boolean), source: 'Range shown on the globe is a simplified approximation.' }; }
  if (k === 'e' && EVENT_BY_ID[id]) { const e = EVENT_BY_ID[id]; return { kind: `Event · ${e.type}`, place: null, lat: e.lat, lng: e.lng, dates: [{ year: e.year, label: e.name }], related: relatedTo(e, key) }; }
  if (k === 'c' && COUNTRY_BY_ISO3[id]) { const c = COUNTRY_BY_ISO3[id], ce = countryCenter(c), p = c.props; return { kind: `Country · ${p.CONTINENT || ''}`, place: p.SUBREGION, lat: ce?.lat, lng: ce?.lng, figures: { Population: p.POP_EST ? fmtNum(p.POP_EST) : null, GDP: p.GDP_MD ? '$' + fmtNum(p.GDP_MD * 1e6) : null } }; }
  if (k === 'r' && RESOURCE[id]) { const r = RESOURCE[id]; return { kind: 'Natural resource', figures: { Reserves: r.reserves, Production: r.production } }; }
  if (k === 'p') { const p = nearbyPlaces.get(+id); return p ? { kind: 'Place', lat: p.lat, lng: p.lng, related: relatedTo(p, key) } : null; }
  return null;
}
const coords = (lat, lng) => `<span class="num">${Math.abs(lat).toFixed(3)}° ${lat >= 0 ? 'N' : 'S'}, ${Math.abs(lng).toFixed(3)}° ${lng >= 0 ? 'E' : 'W'}</span>`;
function markSelected(key) {
  state.selectedKey = key;
  $$('.marker.selected').forEach((e) => e.classList.remove('selected'));
  if (key) markerEls.get(key)?.classList.add('selected');
  writeHash();
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
  $$('[data-res]', infoBody).forEach((b) => (b.onclick = () => { toggleResource(b.dataset.res, true); showResource(b.dataset.res, false); }));
  $$('[data-here]', infoBody).forEach((b) => (b.onclick = () => { const [la, lo, km] = b.dataset.here.split(',').map(Number); showHere(la, lo, km, b.dataset.hereName); }));
  $$('[data-read]', infoBody).forEach((b) => (b.onclick = () => reader.open({ title: b.dataset.title, item: itemFor(b.dataset.read) })));
  $$('[data-remove-animal]', infoBody).forEach((b) => (b.onclick = () => { toggleAnimal(b.dataset.removeAnimal, false); closeInfo(); }));
}

function showWonder(w) {
  flyTo(w.lat, w.lng, 0.7);
  const formed = w.formed && Math.abs(w.formed) > 3000 ? (Math.abs(w.formed) >= 1e6 ? `~${fmtNum(Math.abs(w.formed))} years ago` : fmtYear(w.formed)) : null;
  openInfo(`${gallery(w.wiki, w.name, wonderIcon(w))}<div class="info-main">
    <h2>${esc(w.name)}</h2>
    ${meta(wonderIcon(w), C_WONDER, [esc(w.type), esc(w.country), coords(w.lat, w.lng)])}
    <p class="summary">${esc(w.summary)}</p>
    ${figures({ ...(w.stats || {}), Formed: formed, 'UNESCO World Heritage': w.unesco || null })}
    ${factList(w.facts)}${wikiExtract(w.wiki)}
    <div class="actions">${readBtn('w:' + w.id, w.wiki)}${closeupBtns(w.lat, w.lng)}${hereBtn(w.lat, w.lng, 40, w.name)}${wikiLink(w.wiki)}</div></div>`, 'w:' + w.id);
}
function showLandmark(l) {
  flyTo(l.lat, l.lng, 0.6);
  const lost = l.destroyed != null && l.destroyed <= state.year, notYet = l.built != null && l.built > state.year;
  openInfo(`${gallery(l.wiki, l.name, landmarkIcon(l))}<div class="info-main">
    <h2>${esc(l.name)}</h2>
    ${meta(landmarkIcon(l), C_LANDMARK, [esc(l.category), esc([l.city, l.country].filter(Boolean).join(', ')), coords(l.lat, l.lng)])}
    ${lost ? `<div class="notice">Destroyed in ${esc(fmtYear(l.destroyed))}. <button class="btn" data-year="${lostMidYear(l)}">${icon('hourglass')}Travel back to see it</button></div>` : ''}
    ${notYet ? `<div class="notice">Not built yet in ${esc(fmtYear(state.year))}; completed ${esc(fmtYear(l.built))}. <button class="btn" data-year="${l.built}">${icon('hourglass')}Jump to ${esc(fmtYear(l.built))}</button></div>` : ''}
    <p class="summary">${esc(l.summary)}</p>
    ${figures({ Completed: l.built != null ? fmtYear(l.built) : null, Destroyed: l.destroyed != null ? fmtYear(l.destroyed) : null, 'Built by': l.builder, Height: l.height, ...(l.stats || {}) })}
    ${factList(l.facts)}${wikiExtract(l.wiki)}
    <div class="actions">${readBtn('l:' + l.id, l.wiki)}${closeupBtns(l.lat, l.lng)}${hereBtn(l.lat, l.lng, 8, l.name)}${wikiLink(l.wiki)}</div></div>`, 'l:' + l.id);
}
function showEvent(e) {
  flyTo(e.lat, e.lng, 0.9);
  openInfo(`${gallery(e.wiki, e.name, EVENT_ICON[e.type] || 'map-pin')}<div class="info-main">
    <h2>${esc(e.name)}</h2>
    ${meta(EVENT_ICON[e.type] || 'map-pin', EVENT_COLOR[e.type], [`<span class="num">${esc(fmtYear(e.year))}</span>`, esc(e.type[0].toUpperCase() + e.type.slice(1)), coords(e.lat, e.lng)])}
    <p class="summary">${esc(e.summary)}</p>${wikiExtract(e.wiki)}
    <div class="actions">${readBtn('e:' + e.id, e.wiki)}<button class="btn primary" data-year="${e.year}">${icon('hourglass')}Go to ${esc(fmtYear(e.year))}</button><button class="btn" data-fly="${e.lat},${e.lng},0.35">${icon('crosshair')}Fly here</button>${wikiLink(e.wiki)}</div></div>`, 'e:' + e.id);
}
function showChange(c) {
  const f = changeFactor(c), z = c.zones?.[0], st = CHANGE_STYLE[c.kind] || CHANGE_STYLE.water;
  if (z) flyTo(z[0], z[1], 1.6);
  openInfo(`${gallery((c.name || '').replace(/ /g, '_'), c.name, st.icon)}<div class="info-main">
    <h2>${esc(c.name)}</h2>
    ${meta(st.icon, st.color, [esc(c.kind === 'loss' ? 'Forest loss' : c.kind[0].toUpperCase() + c.kind.slice(1)), `In ${esc(fmtYear(state.year))}: <b class="num">${Math.round(f * 100)}%</b> of its mapped extent`])}
    <p class="summary">${esc(c.summary)}</p>
    <h3>Through time</h3>
    ${(c.keys || []).map(([y, v]) => `<button class="row-btn" data-year="${y}"><span class="rb-name num">${esc(fmtYear(y))}</span><span class="rb-val">${Math.round(v * 100)}%</span><span class="bar"><span style="display:block;height:100%;width:${clamp(v * 100, 0, 100)}%;background:${st.color};border-radius:99px"></span></span></button>`).join('')}
    <p class="muted small">Shapes are simplified ellipses scaled by area. Choose a year to travel there.</p></div>`, 'ch:' + c.id);
}
function showSite(r, s) {
  flyTo(s.lat, s.lng, 0.5);
  openInfo(`<div class="info-main">
    <h2>${esc(s.name)}</h2>
    ${meta('pickaxe', r.color, [esc(s.kind || 'Deposit'), esc(r.name), esc(s.country), coords(s.lat, s.lng)])}
    <p class="summary">${esc(s.info)}</p>
    <div class="actions">${closeupBtns(s.lat, s.lng)}<button class="btn" data-res="${r.id}">${icon('info')}About ${esc(r.name.toLowerCase())}</button></div></div>`, `r:${r.id}:${r.sites.indexOf(s)}`);
}
function showResource(id, doFly = true) {
  const r = RESOURCE[id];
  if (!r) return;
  if (doFly) fly({ altitude: 2.4 }, 1200);
  const max = Math.max(...(r.producers || []).map((p) => p.share || 0), 1);
  openInfo(`${gallery(RES_WIKI[id], r.name, 'pickaxe')}<div class="info-main">
    <h2>${esc(r.name)}</h2>
    ${meta('pickaxe', r.color, ['Natural resource', `${(r.sites || []).length} major sites on the map`])}
    <p class="summary">${esc(r.summary)}</p>
    ${figures({ Reserves: r.reserves, Production: r.production, 'Used for': (r.uses || []).join(', ') })}
    <h3>Top producers</h3>
    ${(r.producers || []).map((p) => `<button class="row-btn" data-country="${esc(p.iso3)}"><span class="rb-name">${esc(p.name)}</span><span class="rb-val">${p.share}%${p.value ? ' · ' + esc(p.value) : ''}</span><span class="bar"><span style="display:block;height:100%;width:${(p.share / max) * 100}%;background:${r.color};border-radius:99px"></span></span></button>`).join('')}
    ${factList(r.facts)}
    <h3>Major deposits and sites</h3>
    ${(r.sites || []).map((s, i) => `<button class="row-btn" data-site="${r.id}|${i}"><span class="rb-name">${esc(s.name)}</span><span class="rb-val">${esc(s.country)}</span><small>${esc(s.kind || '')}</small></button>`).join('')}
    ${wikiExtract(RES_WIKI[id])}<div class="actions">${readBtn('r:' + id, RES_WIKI[id])}${wikiLink(RES_WIKI[id])}</div></div>`, 'r:' + id);
}
function countryCenter(c) {
  let best = null, area = -1;
  const polys = c.geometry.type === 'Polygon' ? [c.geometry.coordinates] : c.geometry.coordinates;
  polys.forEach((p) => {
    let minX = 999, maxX = -999, minY = 999, maxY = -999;
    p[0].forEach(([x, y]) => { minX = Math.min(minX, x); maxX = Math.max(maxX, x); minY = Math.min(minY, y); maxY = Math.max(maxY, y); });
    const a = (maxX - minX) * (maxY - minY);
    if (a > area) { area = a; best = { lat: (minY + maxY) / 2, lng: (minX + maxX) / 2, span: Math.max(maxX - minX, maxY - minY) }; }
  });
  return best;
}
function flyToCountry(c) { const ce = countryCenter(c); if (ce) flyTo(ce.lat, ce.lng, clamp(ce.span / 28, 0.35, 2.2)); }
const COUNTRY_WIKI = { 'United States of America': 'United States', 'Dem. Rep. Congo': 'Democratic Republic of the Congo', 'Central African Rep.': 'Central African Republic', 'S. Sudan': 'South Sudan', 'Bosnia and Herz.': 'Bosnia and Herzegovina', 'Dominican Rep.': 'Dominican Republic', 'Eq. Guinea': 'Equatorial Guinea', 'Solomon Is.': 'Solomon Islands', 'Falkland Is.': 'Falkland Islands', 'Fr. S. Antarctic Lands': 'French Southern and Antarctic Lands', 'W. Sahara': 'Western Sahara', 'N. Cyprus': 'Northern Cyprus', "Côte d'Ivoire": 'Ivory Coast', eSwatini: 'Eswatini', Macedonia: 'North Macedonia', Georgia: 'Georgia (country)' };
function showCountry(c) {
  const p = c.props;
  const title = COUNTRY_WIKI[c.name] || c.longName;
  const inCountry = (x) => (x.country || '').split(/[,/;]| and /).map((s) => s.trim()).some((n) => n && (n === c.name || n === c.longName || n === title));
  const ws = WONDERS.filter(inCountry), ls = LANDMARKS.filter(inCountry);
  const res = RESOURCES.map((r) => ({ r, pp: (r.producers || []).find((x) => x.iso3 === c.iso3) })).filter((x) => x.pp);
  openInfo(`${gallery(title, c.longName, 'flag')}<div class="info-main">
    <h2>${c.iso2 ? `<img src="https://flagcdn.com/w80/${c.iso2.toLowerCase()}.png" alt="Flag of ${esc(c.longName)}" style="height:22px;border-radius:3px;vertical-align:-2px;margin-right:8px">` : ''}${esc(c.longName)}</h2>
    ${meta('globe', '#c9d3ea', [esc(p.CONTINENT || ''), esc(p.SUBREGION || ''), p.FORMAL_EN && p.FORMAL_EN !== c.longName ? esc(p.FORMAL_EN) : ''])}
    ${countries.factsHtml(c.iso3)}
    ${figures({ Population: p.POP_EST ? fmtNum(p.POP_EST) + (p.POP_YEAR ? ` (${p.POP_YEAR})` : '') : null, GDP: p.GDP_MD ? '$' + fmtNum(p.GDP_MD * 1e6) + (p.GDP_YEAR ? ` (${p.GDP_YEAR})` : '') : null, Economy: (p.ECONOMY || '').replace(/^\d+\.\s*/, ''), 'Income group': (p.INCOME_GRP || '').replace(/^\d+\.\s*/, '') })}
    <h3>Over time</h3><div class="charts" id="c-charts"></div>
    ${res.length ? `<h3>Major producer of</h3>${res.map(({ r, pp }) => `<button class="row-btn" data-res="${r.id}"><span class="rb-name">${esc(r.name)}</span><span class="rb-val">${pp.share}% of world</span>${pp.value ? `<small>${esc(pp.value)}</small>` : ''}</button>`).join('')}` : ''}
    ${ws.length ? `<h3>Natural wonders</h3>${ws.map((w) => `<button class="row-btn" data-item="w:${w.id}"><span class="rb-name">${esc(w.name)}</span><span class="rb-val">${esc(w.type)}</span></button>`).join('')}` : ''}
    ${ls.length ? `<h3>Landmarks</h3>${ls.map((l) => `<button class="row-btn" data-item="l:${l.id}"><span class="rb-name">${esc(l.name)}</span><span class="rb-val num">${esc(fmtYear(l.built))}</span></button>`).join('')}` : ''}
    ${wikiExtract(title)}<div class="actions">${readBtn('c:' + c.iso3, title)}${(() => { const ce = countryCenter(c); return ce ? hereBtn(+ce.lat.toFixed(3), +ce.lng.toFixed(3), Math.round(Math.min(400, Math.max(60, ce.span * 25))), c.longName) : ''; })()}${wikiLink(title)}</div></div>`, 'c:' + c.iso3);
  countries.mountIndicators(document.getElementById('c-charts'), [c.iso3], [c.longName]);
}
function showPolity(d) {
  openInfo(`${gallery(d.name, d.name, 'flag')}<div class="info-main">
    <h2>${esc(d.name)}</h2>
    ${meta('scroll', '#e2a2f2', [`Historical state, borders c. <span class="num">${esc(fmtYear(state.histYear))}</span>`, d.subject && d.subject !== d.name ? `part of ${esc(d.subject)}` : ''])}
    ${wikiExtract(d.name)}
    <p class="muted small">Historical borders come from the historical-basemaps project (A. Ourednik). Pre-modern borders are approximate and often contested.</p>
    <div class="actions">${readBtn('h:' + d.name, d.name)}${wikiLink(d.name)}</div></div>`, null);
}
function showWikiPlace(p) {
  openInfo(`${gallery(p.title, p.title, 'book-open')}<div class="info-main">
    <h2>${esc(p.title)}</h2>
    ${meta('book-open', C_WIKI, ['Nearby place on Wikipedia', coords(p.lat, p.lng)])}
    ${wikiExtract(p.title)}
    <div class="actions">${readBtn('p:' + p.pageid, p.title)}${closeupBtns(p.lat, p.lng)}${hereBtn(p.lat, p.lng, 3, p.title)}${wikiLink(p.title)}</div></div>`, 'p:' + p.pageid);
}
function showAnimal(id, hotspot) {
  const a = ANIMAL[id];
  if (!a) return;
  if (!state.animals.has(id)) toggleAnimal(id, true);
  if (hotspot) flyTo(hotspot.lat, hotspot.lng, 0.6);
  else { const z = animalZones(a).zones[0]; if (z) flyTo(z[0], z[1], 1.9); }
  const alive = animalAlive(a);
  const others = [...state.animals].filter((s) => s !== id && ANIMAL[s]);
  const kw = [...a.name.split(/[\s-]+/), ...a.scientific.split(/\s+/), a.wiki.replace(/_/g, ' ')].filter((w) => w.length > 3 && !/^(great|common|white|red|green|grey|gray|american|african|king|giant|wild)$/i.test(w));
  openInfo(`${gallery(a.wiki, a.name, 'paw-print', kw)}<div class="info-main">
    <h2>${esc(a.name)}</h2>
    <div class="meta" style="--c:${a.color}">${icon('paw-print')}<span><i>${esc(a.scientific)}</i></span><span class="sep">·</span><span>${esc(a.group)}</span><span class="status st-${esc(a.status)}" title="IUCN Red List status">${esc(STATUS[a.status] || a.status)}</span></div>
    ${!alive ? `<div class="notice">Extinct since ${esc(fmtYear(a.extinct))}; its last range is shown faded. <button class="btn" data-year="${Math.max(-20000, a.extinct - (a.extinct > 0 ? 60 : 1500))}">${icon('hourglass')}Travel back to see it alive</button></div>` : ''}
    ${a.pastRange && state.year >= a.pastRange.until ? `<div class="notice">Before ${esc(fmtYear(a.pastRange.until))} it lived across a much larger area. <button class="btn" data-year="${a.pastRange.until - 50}">${icon('hourglass')}See its historic range</button></div>` : ''}
    <p class="summary">${esc(a.summary)}</p>
    ${figures({ Population: a.population, Habitat: a.habitat, Diet: a.diet, Size: a.size, Lifespan: a.lifespan, 'Top speed': a.speed })}
    ${factList(a.facts)}
    ${a.hotspots?.length ? `<h3>Where to see it</h3>${a.hotspots.map((h) => `<button class="row-btn" data-fly="${h.lat},${h.lng},0.4"><span class="rb-name">${esc(h.name)}</span><span class="rb-val">${coords(h.lat, h.lng)}</span></button>`).join('')}` : ''}
    ${wikiExtract(a.wiki)}
    ${others.length ? `<h3>Also on the map</h3><div class="chips-row">${others.map((s) => `<button class="chip" data-animal="${s}"><span class="swatch" style="background:${ANIMAL[s].color}"></span>${esc(ANIMAL[s].name)}</button>`).join('')}</div>` : ''}
    <div class="actions">${readBtn('a:' + id, a.wiki)}${wikiLink(a.wiki)}<button class="btn" data-remove-animal="${id}">${icon('x')}Remove from map</button></div></div>`, 'a:' + id);
}
function openByKey(key) {
  const i = key.indexOf(':'), k = key.slice(0, i), id = key.slice(i + 1);
  if (k === 'w' && WONDER[id]) showWonder(WONDER[id]);
  else if (k === 'l' && LANDMARK[id]) {
    const l = LANDMARK[id];
    if (!exists(l)) setYear(l.destroyed != null && l.destroyed <= state.year ? lostMidYear(l) : l.built, true);
    showLandmark(l);
  } else if (k === 'a' && ANIMAL[id]) showAnimal(id);
  else if (k === 'r' && RESOURCE[id]) { toggleResource(id, true); showResource(id); }
  else if (k === 'e' && EVENT_BY_ID[id]) { const e = EVENT_BY_ID[id]; setYear(e.year, true); showEvent(e); }
  else if (k === 'c' && COUNTRY_BY_ISO3[id]) { const c = COUNTRY_BY_ISO3[id]; showCountry(c); flyToCountry(c); }
  else if (k === 'ch') { const c = CHANGES.find((x) => x.id === id); if (c) showChange(c); }
}

// ============================================================ sidebar
function toggleAnimal(id, on = !state.animals.has(id)) {
  on ? state.animals.add(id) : state.animals.delete(id);
  buildRangePolys(); refreshPolygons(); refreshMarkers(); refreshMinis(); renderLegend(); writeHash();
  if (state.tab === 'animals') renderSide();
}
function toggleResource(id, on = !state.resources.has(id)) {
  state.resources.delete(id);
  if (on) state.resources.add(id);
  globe.polygonCapColor(polyCap);
  refreshPolygons(); refreshMarkers(); renderLegend(); writeHash();
  if (state.tab === 'resources') renderSide();
}
const sideContent = $('#side-content');
const listFilter = { wonders: '', landmarks: '', animals: '', resources: '', history: '' };
const switchHtml = (key, on, label) => `<label class="switch"><input type="checkbox" data-layer="${key}" ${on ? 'checked' : ''}><span class="track"></span><span${label ? '' : ' class="sr-only"'}>${label || 'Show on globe'}</span></label>`;
const filterInput = (tab, ph) => `<input class="list-filter" type="search" data-filter="${tab}" placeholder="${ph}" aria-label="${ph}" value="${esc(listFilter[tab])}">`;
const matches = (tab, ...s) => !listFilter[tab] || s.join(' ').toLowerCase().includes(listFilter[tab].toLowerCase());
const dotIc = (name, color) => `<span class="dot-ic" style="--c:${color}">${icon(name)}</span>`;
const dotSym = (sym, color) => `<span class="dot-ic" style="--c:${color};font-size:11px;font-weight:800">${esc(sym)}</span>`;
function renderSide() {
  const tab = state.tab;
  let html = '';
  if (tab === 'wonders') {
    const types = [...new Set(WONDERS.map((w) => w.type))].sort();
    const list = WONDERS.filter((w) => (!state.wonderTypes || state.wonderTypes.has(w.type)) && matches('wonders', w.name, w.country, w.type)).sort((a, b) => a.name.localeCompare(b.name));
    html = `<div class="side-head"><h2>Natural wonders</h2>${switchHtml('wonders', state.layers.wonders)}</div>
      <p class="side-desc">${WONDERS.length} of the planet's most spectacular places. Choose one to fly there.</p>
      <div class="filter-row"><button class="chip" aria-pressed="${!state.wonderTypes}" data-wtype="*">All</button>${types.map((t) => `<button class="chip" aria-pressed="${!!state.wonderTypes?.has(t)}" data-wtype="${esc(t)}">${esc(t)}</button>`).join('')}</div>
      ${filterInput('wonders', 'Filter wonders')}
      ${list.map((w) => `<button class="item" data-open="w:${w.id}">${dotIc(wonderIcon(w), C_WONDER)}<span class="nm">${esc(w.name)}<small>${esc(w.type)} · ${esc(w.country)}</small></span></button>`).join('') || '<p class="side-desc">No wonders match this filter.</p>'}`;
  } else if (tab === 'landmarks') {
    const cats = [...new Set(LANDMARKS.map((l) => l.category))].sort();
    const list = LANDMARKS.filter((l) => (!state.landmarkCats || state.landmarkCats.has(l.category)) && matches('landmarks', l.name, l.city, l.country, l.category)).sort((a, b) => a.name.localeCompare(b.name));
    html = `<div class="side-head"><h2>Landmarks</h2>${switchHtml('landmarks', state.layers.landmarks)}</div>
      <p class="side-desc">${LANDMARKS.length} famous buildings and monuments. Lost wonders reappear when you travel back in time.</p>
      <div class="filter-row"><button class="chip" aria-pressed="${!state.landmarkCats}" data-lcat="*">All</button>${cats.map((t) => `<button class="chip" aria-pressed="${!!state.landmarkCats?.has(t)}" data-lcat="${esc(t)}">${esc(t)}</button>`).join('')}</div>
      ${filterInput('landmarks', 'Filter landmarks')}
      ${list.map((l) => {
        const tag = l.destroyed != null && l.destroyed <= state.year ? `<span class="tag lost">lost ${esc(fmtYear(l.destroyed))}</span>` : l.built > state.year ? `<span class="tag">built ${esc(fmtYear(l.built))}</span>` : '';
        return `<button class="item" data-open="l:${l.id}">${dotIc(landmarkIcon(l), C_LANDMARK)}<span class="nm">${esc(l.name)}<small>${esc([l.city, l.country].filter(Boolean).join(', '))}</small></span>${tag}</button>`;
      }).join('') || '<p class="side-desc">No landmarks match this filter.</p>'}`;
  } else if (tab === 'animals') {
    const groups = [...new Set(ANIMALS.map((a) => a.group))];
    html = `<div class="side-head"><h2>Animals</h2><button class="chip" data-clear="animals" ${state.animals.size ? '' : 'disabled'}>Clear ${state.animals.size || ''}</button></div>
      <p class="side-desc">Tick one or many species to paint where they live and place animated 3D miniatures on the globe. Choose a name for photos and facts.</p>
      ${filterInput('animals', 'Filter animals')}
      ${groups.map((g) => {
        const list = ANIMALS.filter((a) => a.group === g && matches('animals', a.name, a.scientific, g));
        if (!list.length) return '';
        return `<div class="group-title">${esc(g)} <button data-allgroup="${esc(g)}">Select all</button></div>` + list.map((a) => `<div class="item ${state.animals.has(a.id) ? 'selected' : ''}" style="--c:${a.color}">
          <input type="checkbox" data-animal-cb="${a.id}" ${state.animals.has(a.id) ? 'checked' : ''} aria-label="Show ${esc(a.name)} on the globe">
          <button class="nm" data-open="a:${a.id}">${esc(a.name)}<small><i>${esc(a.scientific)}</i></small></button>
          ${a.extinct != null ? '<span class="tag lost">extinct</span>' : `<span class="swatch" style="background:${a.color}" aria-hidden="true"></span>`}</div>`).join('');
      }).join('') || '<p class="side-desc">No animals match this filter.</p>'}`;
  } else if (tab === 'resources') {
    html = `<div class="side-head"><h2>Natural resources</h2><button class="chip" data-clear="resources" ${state.resources.size ? '' : 'disabled'}>Clear ${state.resources.size || ''}</button></div>
      <p class="side-desc">Choose a resource to see its biggest deposits and colour the top producing countries. The most recently chosen resource colours the map.</p>
      ${RESOURCES.map((r) => `<div class="item ${state.resources.has(r.id) ? 'selected' : ''}" style="--c:${r.color}">
        <input type="checkbox" data-res-cb="${r.id}" ${state.resources.has(r.id) ? 'checked' : ''} aria-label="Show ${esc(r.name)} on the globe">
        ${dotSym(RES_SYMBOL[r.id] || '', r.color)}
        <button class="nm" data-open="r:${r.id}">${esc(r.name)}<small>${(r.sites || []).length} sites · top producer ${esc(r.producers?.[0]?.name || '')}</small></button></div>`).join('')}`;
  } else if (tab === 'history') {
    const evs = EVENTS.filter((e) => matches('history', e.name, e.type, String(e.year))).sort((a, b) => a.year - b.year);
    html = `<div class="side-head"><h2>History and change</h2></div>
      <p class="side-desc">Drag the timeline to travel from the last Ice Age to today. Borders, ice sheets, coastlines, lakes, forests, landmarks and wildlife follow the year.</p>
      <div class="item">${dotIc('map-pin', '#ffd75e')}<span class="nm">Historical events</span>${switchHtml('events', state.layers.events)}</div>
      <div class="item">${dotIc('snowflake', '#eef7ff')}<span class="nm">Ice, lakes, land bridges and forests</span>${switchHtml('changes', state.layers.changes)}</div>
      <div class="group-title">Eras</div>
      ${ERAS.map((e) => `<button class="item" data-goyear="${Math.max(e.from, -20000)}">${dotIc('calendar', '#c9d3ea')}<span class="nm">${esc(e.title)}<small class="num">${esc(fmtYear(e.from))} – ${esc(fmtYear(Math.min(e.to, NOW)))}</small></span></button>`).join('')}
      <div class="group-title">Changing landscapes</div>
      ${CHANGES.map((c) => { const st = CHANGE_STYLE[c.kind] || CHANGE_STYLE.water; return `<button class="item" data-open="ch:${c.id}">${dotIc(st.icon, st.color)}<span class="nm">${esc(c.name)}<small>${esc(c.kind === 'loss' ? 'forest loss' : c.kind)}</small></span></button>`; }).join('')}
      <div class="group-title">Events (${evs.length})</div>
      ${filterInput('history', 'Filter events')}
      ${evs.map((e) => `<button class="item" data-open="e:${e.id}">${dotIc(EVENT_ICON[e.type] || 'map-pin', EVENT_COLOR[e.type])}<span class="nm">${esc(e.name)}<small class="num">${esc(fmtYear(e.year))} · ${esc(e.type)}</small></span></button>`).join('')}`;
  }
  if (PANELS[tab]) { PANELS[tab].render(sideContent); return; }
  sideContent.innerHTML = html;
}
sideContent.addEventListener('click', (ev) => {
  const t = ev.target;
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
    globe.polygonCapColor(polyCap); refreshPolygons(); refreshMarkers(); renderLegend(); renderSide(); writeHash();
    return;
  }
  const ag = t.closest('[data-allgroup]');
  if (ag) { ANIMALS.filter((a) => a.group === ag.dataset.allgroup).forEach((a) => state.animals.add(a.id)); buildRangePolys(); refreshPolygons(); refreshMarkers(); refreshMinis(); renderLegend(); renderSide(); writeHash(); return; }
  const gy = t.closest('[data-goyear]');
  if (gy) return setYear(+gy.dataset.goyear, true);
  const open = t.closest('[data-open]');
  if (open) openByKey(open.dataset.open);
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
// Panels contributed by feature modules (countries, news, learn) render through this registry.
const PANELS = {};
function selectTab(tab, { reopen = true } = {}) {
  const sb = $('#sidebar');
  if (state.tab === tab && reopen && !sb.classList.contains('panel-closed')) { sb.classList.add('panel-closed'); return; }
  sb.classList.remove('panel-closed', 'collapsed');
  const prev = state.tab;
  state.tab = tab;
  $$('.rail-btn').forEach((x) => x.setAttribute('aria-selected', String(x.dataset.tab === tab)));
  if (prev !== tab) PANELS[prev]?.leave?.();
  renderSide();
  PANELS[tab]?.enter?.();
  sideContent.scrollTop = 0;
  writeHash();
}
$$('.rail-btn').forEach((b) => (b.onclick = () => selectTab(b.dataset.tab)));
$('#menu-toggle').onclick = () => $('#sidebar').classList.toggle('collapsed');

// ============================================================ legend
function renderLegend() {
  const lg = $('#legend');
  const parts = [];
  if (state.animals.size) {
    parts.push('<div class="lg-title">Animal ranges (approximate)</div>' + [...state.animals].map((id) => { const a = ANIMAL[id]; return a ? `<button class="lg-row" data-lg-animal="${id}"><span class="swatch" style="background:${a.color}"></span>${esc(a.name)}${!animalAlive(a) ? ' <span class="muted">(extinct)</span>' : ''}</button>` : ''; }).join(''));
  }
  const ch = currentChoropleth();
  if (ch) {
    parts.push(`<div class="lg-title" style="margin-top:8px">${esc(ch.r.name)}: share of world production</div><div class="lg-grad" style="background:linear-gradient(90deg, ${hexA(ch.r.color, 0.25)}, ${hexA(ch.r.color, 0.85)})"></div><div style="display:flex;justify-content:space-between" class="muted num"><span>low</span><span>${ch.max}%</span></div>${state.histFeatures ? '<div class="muted" style="margin-top:4px">Production data is for today, so countries are coloured only on present-day maps (2011 onwards).</div>' : ''}`);
    if (state.resources.size > 1) parts.push([...state.resources].map((id) => `<button class="lg-row" data-lg-res="${id}"><span class="swatch" style="background:${RESOURCE[id].color}"></span>${esc(RESOURCE[id].name)} sites</button>`).join(''));
  }
  lg.innerHTML = parts.join('');
  lg.classList.toggle('hidden', !parts.length);
  $$('[data-lg-animal]', lg).forEach((r) => (r.onclick = () => showAnimal(r.dataset.lgAnimal)));
  $$('[data-lg-res]', lg).forEach((r) => (r.onclick = () => showResource(r.dataset.lgRes)));
}

// ============================================================ search
const searchIndex = () => [
  ...WONDERS.map((w) => ({ key: 'w:' + w.id, name: w.name, sub: w.country, icon: wonderIcon(w), c: C_WONDER, kind: 'Wonder' })),
  ...LANDMARKS.map((l) => ({ key: 'l:' + l.id, name: l.name, sub: [l.city, l.country].filter(Boolean).join(', '), icon: landmarkIcon(l), c: C_LANDMARK, kind: 'Landmark' })),
  ...ANIMALS.map((a) => ({ key: 'a:' + a.id, name: a.name, sub: a.scientific, icon: 'paw-print', c: a.color, kind: 'Animal' })),
  ...RESOURCES.map((r) => ({ key: 'r:' + r.id, name: r.name, sub: 'Natural resource', icon: 'pickaxe', c: r.color, kind: 'Resource' })),
  ...COUNTRIES.map((c) => ({ key: 'c:' + c.iso3, name: c.longName, sub: c.props.CONTINENT, icon: 'flag', c: '#c9d3ea', kind: 'Country' })),
  ...EVENTS.map((e) => ({ key: 'e:' + e.id, name: e.name, sub: fmtYear(e.year), icon: EVENT_ICON[e.type] || 'map-pin', c: EVENT_COLOR[e.type], kind: 'Event' })),
];
let SEARCH = null, srActive = 0;
const sInput = $('#search'), sRes = $('#search-results');
const closeSearch = () => { sRes.classList.add('hidden'); sInput.setAttribute('aria-expanded', 'false'); };
sInput.addEventListener('input', () => {
  SEARCH = SEARCH || searchIndex();
  const q = sInput.value.trim().toLowerCase();
  if (!q) return closeSearch();
  const hits = SEARCH.map((x) => {
    const n = x.name.toLowerCase();
    return { x, score: n.startsWith(q) ? 0 : n.includes(q) ? 1 : (x.sub || '').toLowerCase().includes(q) ? 2 : 9 };
  }).filter((h) => h.score < 9).sort((a, b) => a.score - b.score || a.x.name.localeCompare(b.x.name)).slice(0, 14);
  srActive = 0;
  sRes.innerHTML = hits.length
    ? hits.map((h, i) => `<button class="sr-item ${i === 0 ? 'active' : ''}" role="option" data-key="${h.x.key}"><span class="sr-ic" style="--c:${h.x.c}">${icon(h.x.icon)}</span><span>${esc(h.x.name)}<br><small>${esc(h.x.sub || '')}</small></span><span class="sr-kind">${h.x.kind}</span></button>`).join('')
    : `<div class="sr-item muted">Nothing matches “${esc(sInput.value.trim())}”. Try a place, species or country name.</div>`;
  sRes.classList.remove('hidden');
  sInput.setAttribute('aria-expanded', 'true');
});
sInput.addEventListener('keydown', (ev) => {
  const items = $$('.sr-item[data-key]', sRes);
  if (ev.key === 'ArrowDown' || ev.key === 'ArrowUp') {
    ev.preventDefault();
    srActive = clamp(srActive + (ev.key === 'ArrowDown' ? 1 : -1), 0, items.length - 1);
    items.forEach((it, i) => it.classList.toggle('active', i === srActive));
  } else if (ev.key === 'Enter' && items[srActive]) pickSearch(items[srActive].dataset.key);
  else if (ev.key === 'Escape') closeSearch();
});
sRes.addEventListener('click', (ev) => { const it = ev.target.closest('[data-key]'); if (it) pickSearch(it.dataset.key); });
document.addEventListener('click', (ev) => { if (!ev.target.closest('.search-wrap')) closeSearch(); });
function pickSearch(key) { closeSearch(); sInput.value = ''; sInput.blur(); openByKey(key); }

// ============================================================ timeline
const slider = $('#tl-slider');
function renderMarks() {
  // one hairline per year that has a historical border map (positions follow the slider's non-linear scale)
  $('#tl-marks').innerHTML = [...HIST_MAPS, 2011].filter((y) => y >= -20000).map((y) => `<i style="left:${yearToPos(y) / 10}%"></i>`).join('');
}
function renderTicks() {
  const ticks = window.innerWidth < 760 ? [-20000, -5000, 1, 1500, 1900, NOW] : [-20000, -10000, -5000, -3000, -1000, 1, 500, 1000, 1500, 1800, 1900, 1950, 2000, NOW];
  $('#tl-ticks').innerHTML = ticks.map((y) => `<span style="left:${yearToPos(y) / 10}%">${y === NOW ? 'Now' : y < 0 ? (-y >= 1000 ? -y / 1000 + 'k BC' : -y + ' BC') : y === 1 ? 'AD 1' : y}</span>`).join('');
}
/** The slider track is coloured by global temperature (blue = colder, red = warmer than 1850–1900). */
function paintTempTrack() {
  const pts = STATS.temp?.points;
  if (!pts) return;
  const stops = [];
  for (let p = 0; p <= 1000; p += 40) {
    const y = posToYear(p);
    const t = interpClamped(pts, y);
    const c = t < 0 ? new THREE.Color('#d9d2c3').lerp(new THREE.Color('#3f7fe0'), clamp(-t / 6, 0, 1)) : new THREE.Color('#d9d2c3').lerp(new THREE.Color('#e0453a'), clamp(t / 1.5, 0, 1));
    stops.push(`#${c.getHexString()} ${p / 10}%`);
  }
  slider.style.setProperty('--temp-track', `linear-gradient(90deg, ${stops.join(', ')})`);
}
function sparkline(points) {
  const pts = points.filter(([x]) => x >= -20000 && x <= NOW);
  if (pts.length < 2) return '';
  const ys = pts.map((p) => p[1]), lo = Math.min(...ys), hi = Math.max(...ys);
  const X = (x) => (yearToPos(x) / 1000) * 56, Y = (v) => 24 - ((v - lo) / (hi - lo || 1)) * 22;
  const cur = interp(pts, clamp(state.year, pts[0][0], pts[pts.length - 1][0]));
  return `<svg viewBox="0 0 56 26" aria-hidden="true"><polyline fill="none" stroke="rgba(214,226,255,.55)" stroke-width="1.3" points="${pts.map(([x, v]) => `${X(x).toFixed(1)},${Y(v).toFixed(1)}`).join(' ')}"/>${state.year >= pts[0][0] && cur != null ? `<circle cx="${X(clamp(state.year, pts[0][0], pts[pts.length - 1][0])).toFixed(1)}" cy="${Y(cur).toFixed(1)}" r="2.6" fill="#f0b54a"/>` : ''}</svg>`;
}
const STAT_FMT = {
  population: (v) => fmtNum(v), co2: (v) => Math.round(v) + ' ppm', temp: (v) => (v > 0 ? '+' : '') + v.toFixed(2) + ' °C',
  seaLevel: (v) => (v > 0 ? '+' : '') + (Math.abs(v) < 2 ? v.toFixed(2) : Math.round(v)) + ' m', seaIce: (v) => v.toFixed(2) + ' M km²', forest: (v) => v.toFixed(2) + ' B ha',
};
function renderStats() {
  $('#tl-stats').innerHTML = Object.entries(STATS).map(([k, s]) => {
    const last = s.points[s.points.length - 1];
    const stale = last && state.year > last[0];
    const v = stale ? last[1] : interp(s.points, state.year);
    const f = STAT_FMT[k] || ((x) => fmtNum(x, s.unit ? ' ' + s.unit : ''));
    let d;
    if (v != null && last && state.year < last[0] - 0.5) {
      const diff = last[1] - v;
      const rel = Math.abs(diff) / (Math.abs(v) || 1);
      const fd = k === 'population' ? fmtNum(Math.abs(diff)) : k === 'temp' || k === 'seaLevel' ? Math.abs(diff).toFixed(2) + (k === 'temp' ? ' °C' : ' m') : k === 'co2' ? Math.round(Math.abs(diff)) + ' ppm' : Math.abs(diff).toFixed(2);
      d = `<span class="d ${rel < 0.005 ? 'flat' : diff > 0 ? 'up' : 'down'}">${diff > 0 ? '▲' : '▼'} ${fd} by ${last[0]}</span>`;
    } else if (v != null) d = `<span class="d flat">${stale ? 'latest data ' + last[0] : 'latest'}</span>`;
    else d = `<span class="d flat">no data before ${fmtYear(s.points[0][0])}</span>`;
    return `<div class="tl-stat" title="${esc(s.label)}"><span class="k">${esc(s.label)}</span>${sparkline(s.points)}<span class="v">${v != null ? f(v) : '—'} ${d}</span></div>`;
  }).join('');
}
function renderYearLabel() {
  const y = state.year, ago = NOW - y;
  $('#tl-year').innerHTML = `${esc(fmtYear(y))}<small>${y >= NOW ? 'today' : ago === 1 ? '1 year ago' : `${ago.toLocaleString('en-US')} years ago`}${state.histYear != null ? ` · borders c. ${esc(fmtYear(state.histYear))}` : ''}</small>`;
  slider.setAttribute('aria-valuetext', fmtYear(y));
  const era = ERAS.find((e) => y >= e.from && y <= e.to) || ERAS[ERAS.length - 1];
  $('#tl-era').innerHTML = era ? `<b>${esc(era.title)}</b>. ${esc(era.text)}` : '';
  $('#tl-era').title = era ? era.text : '';
  $('#tl-events').innerHTML = visibleEvents().slice(0, 14).sort((a, b) => a.year - b.year).map((e) => `<button class="ev-chip" data-ev="${e.id}" style="--c:${EVENT_COLOR[e.type]}">${icon(EVENT_ICON[e.type] || 'map-pin')}<span class="num">${esc(fmtYear(e.year))}</span> ${esc(e.name)}</button>`).join('');
}
$('#tl-events').addEventListener('click', (ev) => { const c = ev.target.closest('[data-ev]'); if (c) showEvent(EVENT_BY_ID[c.dataset.ev]); });

let yearRaf = null, lastAliveSig = '';
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
    if (sig !== lastAliveSig) { lastAliveSig = sig; refreshMinis(); renderLegend(); }
    loadHistoric();
    if (state.tab === 'landmarks' && !opts.playing) renderSide();
    if (!opts.playing) writeHash();
    if (!opts.playing && reloadNews) reloadNews();
    if (!opts.playing && reloadLive) reloadLive();
  });
}
slider.addEventListener('input', () => setYear(posToYear(+slider.value)));
$$('.tl-jumps button').forEach((b) => (b.onclick = () => setYear(+b.dataset.jump, true)));
let playTimer = null;
const playBtn = $('#tl-play');
function stopPlay() {
  clearInterval(playTimer); playTimer = null;
  playBtn.innerHTML = icon('play'); playBtn.setAttribute('aria-label', 'Play through time');
  renderSide(); writeHash();
}
playBtn.onclick = () => {
  if (playTimer) return stopPlay();
  if (+slider.value >= 1000) slider.value = 0;
  playBtn.innerHTML = icon('pause'); playBtn.setAttribute('aria-label', 'Pause');
  playTimer = setInterval(() => {
    const p = +slider.value + 2;
    slider.value = p;
    setYear(posToYear(p), false, { playing: true });
    if (p >= 1000) stopPlay();
  }, 90);
};

// ============================================================ shareable links
const writeHash = debounce(() => {
  const q = new URLSearchParams();
  if (state.year !== NOW) q.set('y', state.year);
  if (state.animals.size) q.set('a', [...state.animals].join(','));
  if (state.resources.size) q.set('r', [...state.resources].join(','));
  if (state.selectedKey && !/^[rap]:.*:/.test(state.selectedKey) && !state.selectedKey.startsWith('p:')) q.set('k', state.selectedKey);
  if (state.basemap !== 'satellite') q.set('m', state.basemap);
  if (state.tab !== 'wonders') q.set('t', state.tab);
  const s = q.toString();
  history.replaceState(null, '', s ? '#' + s : location.pathname + location.search);
}, 400);
function readHash() {
  const q = new URLSearchParams(location.hash.slice(1));
  if (q.get('m')) setBasemap(q.get('m'));
  (q.get('a') || '').split(',').filter((id) => ANIMAL[id]).forEach((id) => state.animals.add(id));
  (q.get('r') || '').split(',').filter((id) => RESOURCE[id]).forEach((id) => state.resources.add(id));
  const v = (q.get('v') || '').split(',').map(Number);
  return { tab: q.get('t'), year: q.has('y') ? +q.get('y') : NOW, key: q.get('k'), view: v.length === 3 && v.every((x) => !isNaN(x)) ? v : null };
}
$('#share-btn').onclick = async () => {
  writeHash.flush?.();
  const pov = globe.pointOfView();
  const url = new URL(location.href);
  const q = new URLSearchParams(url.hash.slice(1));
  q.set('v', [pov.lat.toFixed(3), pov.lng.toFixed(3), pov.altitude.toFixed(4)].join(','));
  url.hash = q.toString();
  try { await navigator.clipboard.writeText(url.href); toast('Link copied. It opens this exact view, year and selection.'); }
  catch { prompt('Copy this link:', url.href); }
};

// ============================================================ top controls & welcome
const pressToggle = (btn, on) => btn.setAttribute('aria-pressed', String(on));
$('#basemap').onchange = (e) => setBasemap(e.target.value);
$('#toggle-borders').onclick = (e) => { state.borders = !state.borders; pressToggle(e.currentTarget, state.borders); refreshPolygons(); globe.polygonStrokeColor(globe.polygonStrokeColor()); };
$('#toggle-labels').onclick = (e) => { state.labels = !state.labels; pressToggle(e.currentTarget, state.labels); document.body.classList.toggle('show-labels', state.labels); };
$('#toggle-nearby').onclick = (e) => { state.nearby = !state.nearby; pressToggle(e.currentTarget, state.nearby); queueNearby(); refreshMarkers(); if (state.nearby && state.altitude >= NEARBY_ALT) toast('Zoom in closer to see nearby places from Wikipedia.'); };
$('#toggle-rotate').onclick = (e) => { const c = globe.controls(); c.autoRotate = !c.autoRotate; c.autoRotateSpeed = 0.35; pressToggle(e.currentTarget, c.autoRotate); };
const hideWelcome = () => { welcome.classList.add('hidden'); try { localStorage.setItem('ee-visited', '1'); } catch { /* storage unavailable */ } };
$('#help-btn').onclick = () => welcome.classList.toggle('hidden');
$('#welcome-ok').onclick = hideWelcome;
document.addEventListener('keydown', (e) => {
  if (!lb.classList.contains('hidden')) {
    if (e.key === 'Escape') closeLightbox();
    else if (e.key === 'ArrowLeft') lbShow(lbIdx - 1);
    else if (e.key === 'ArrowRight') lbShow(lbIdx + 1);
    return;
  }
  if (e.key === 'Escape') { if (!welcome.classList.contains('hidden')) hideWelcome(); else closeInfo(); }
});
window.addEventListener('resize', debounce(() => { globe.width(window.innerWidth).height(window.innerHeight); renderTicks(); }, 150));

// ============================================================ article reader
const reader = createReader({ esc, fmtYear, getJSON, flyTo, setYear: (y, s) => setYear(y, s), openByKey: (k) => openByKey(k), toast, openLightbox });

// ============================================================ countries atlas
const countries = createCountries({ esc, getJSON, COUNTRIES, COUNTRY_BY_ISO3, state, setYear: (y, s) => setYear(y, s), openInfo: (h, k) => openInfo(h, k) });
PANELS.countries = { render: (el) => countries.renderPanel(el) };
countries.ready.then(() => { if (state.tab === 'countries') renderSide(); });

// ============================================================ news
const openPaper = (e) => { const items = news.state.items; const i = items.indexOf(e); openLightbox(items.map((x) => ({ full: x.big, fallback: x.thumb, caption: `${x.paper}, ${x.city ? x.city + ', ' : ''}${x.state}`, link: x.url, linkText: 'Read this page at the Library of Congress' })), i, 'Front page'); };
const NEWS_COUNTRY = { 'United States of America': 'United States', 'Dem. Rep. Congo': 'Democratic Republic of the Congo', 'Central African Rep.': 'Central African Republic', 'S. Sudan': 'South Sudan', 'Bosnia and Herz.': 'Bosnia and Herzegovina', 'Dominican Rep.': 'Dominican Republic', 'Eq. Guinea': 'Equatorial Guinea', 'Solomon Is.': 'Solomon Islands', "Côte d'Ivoire": 'Ivory Coast', Czechia: 'Czech Republic', 'Republic of the Congo': 'Republic of the Congo' };
function showNewsItem(e) {
  if (!e) return;
  if (e.lat != null) flyTo(e.lat, e.lng, 0.9);
  openInfo(`<div class="info-main">
    <h2 class="news-title">${e.text.replace(/<a [^>]*class="src"[^>]*>.*?<\/a>/g, '').replace(/<[^>]+>/g, '').replace(/\s*\(\s*\)\s*$/, '').slice(0, 220)}</h2>
    ${meta('newspaper', '#e9e3d2', [e.date ? `<span class="num">${esc(e.date)}${e.year && !/\d{4}/.test(e.date) ? ', ' + esc(fmtYear(e.year)) : ''}</span>` : '', e.category ? esc(e.category) : '', e.place ? `${esc(e.place)}` : 'Location not mapped'])}
    <p class="summary news-body">${e.text}</p>
    ${e.sources?.length ? `<h3>Reported by</h3><div class="chips-row">${e.sources.map((s) => `<a class="chip" href="${esc(s.url)}" target="_blank" rel="noopener">${icon('external-link')}${esc(s.name || new URL(s.url).hostname)}</a>`).join('')}</div>` : ''}
    ${e.titles?.length ? `<h3>Background</h3><div class="chips-row">${e.titles.slice(0, 8).map((t) => `<button class="chip" data-read="" data-title="${esc(t)}">${icon('book-open')}${esc(t)}</button>`).join('')}</div>` : ''}
    <div class="actions">${e.lat != null ? closeupBtns(e.lat, e.lng) : ''}</div></div>`, null);
  $$('.news-body a[data-wiki]', infoBody).forEach((a) => (a.onclick = (ev) => { ev.preventDefault(); reader.open({ title: a.dataset.wiki }); }));
}
news = createNews({
  esc, getJSON, fmtYear, state,
  setYear: (y, s) => setYear(y, s),
  refresh: () => { if (state.tab === 'news') renderSide(); refreshMarkers(); },
  countryNames: () => [...new Set(COUNTRIES.map((c) => NEWS_COUNTRY[c.name] || c.name))].sort(),
  showNewsItem: (e) => showNewsItem(e),
  showPaper: (items, i) => {
    const e = items[i];
    if (e.lat != null) flyTo(e.lat, e.lng, 0.9);
    openLightbox(items.map((x) => ({ full: x.big, fallback: x.thumb, caption: `${x.paper}, ${x.city ? x.city + ', ' : ''}${x.state}`, link: x.url, linkText: 'Read this page at the Library of Congress' })), i, 'Front page');
  },
  openArticle: (t) => reader.open({ title: t }),
});
PANELS.news = { render: (el) => news.renderPanel(el), enter: () => { news.load(); refreshMarkers(); }, leave: () => refreshMarkers() };
reloadNews = debounce(() => { if (state.tab === 'news' && !playTimer) news.load(); }, 600);

// ============================================================ learn (tours + quizzes)
globe.onGlobeClick((pt) => pickHandler?.(pt));
globe.arcColor(() => ['#9db1ff', '#f0b54a']).arcStroke(0.5).arcDashLength(0.4).arcDashGap(0.1).arcDashAnimateTime(REDUCED_MOTION ? 0 : 1600).arcAltitudeAutoScale(0.35);
const learn = createLearn({
  esc, fmtYear, TOURS, WONDERS, LANDMARKS, ANIMALS, COUNTRIES,
  countryInfo: (iso3) => countries.info(iso3),
  countryCenter, km: kmBetween, fly, gallery, toast,
  openInfo: (h, k) => openInfo(h, k), closeInfo: () => closeInfo(), openByKey: (k) => openByKey(k),
  setYear: (y, s) => setYear(y, s),
  refreshPanel: () => { if (state.tab === 'learn') renderSide(); },
  tourOn: (on) => document.body.classList.toggle('learning', on),
  setLayers: ({ animals, resources }) => {
    if (animals) { state.animals = new Set(animals.filter((a) => ANIMAL[a])); buildRangePolys(); refreshMinis(); }
    if (resources) { state.resources = new Set(resources.filter((r) => RESOURCE[r])); globe.polygonCapColor(polyCap); }
    if (animals || resources) { refreshPolygons(); refreshMarkers(); renderLegend(); }
  },
  pickOnGlobe: (fn) => { pickHandler = fn; state.pickMode = !!fn; refreshMarkers(); globeEl.style.cursor = fn ? 'crosshair' : null; },
  highlightCountry: (iso3) => { state.quizCountry = iso3; if (iso3 && state.histFeatures) setYear(NOW, true); globe.polygonCapColor(polyCap); refreshPolygons(); },
  showGuess: (g) => { state.guess = g; globe.arcsData(g ? [{ startLat: g.guess.lat, startLng: g.guess.lng, endLat: g.truth.lat, endLng: g.truth.lng }] : []); refreshMarkers(); },
});
PANELS.learn = { render: (el) => learn.renderPanel(el) };

// ============================================================ live earth
live = createLive({ THREE, globe, esc, state, getJSON, fly, openLightbox, nightTexture: TEX + 'earth-night.jpg', layersChanged: () => { refreshMarkers(); refreshQuakes(); }, setBasemap: (k) => { setBasemap(k); renderSide(); }, refresh: () => { if (state.tab === 'live') renderSide(); } });
PANELS.live = { render: (el) => live.renderPanel(el), enter: () => live.loadEpic() };
reloadLive = debounce(() => {
  if (state.basemap === 'gibs') { const d = live.gibsDateFor(state.year); if (!d) setBasemap('satellite'); else if (d !== state.gibsDate) setBasemap('gibs'); }
  if (state.tab === 'live') { live.loadEpic(); renderSide(); }
}, 700);

// live layer helpers: earthquakes as columns (height = magnitude, colour = depth), EONET and ISS cards
function refreshQuakes() {
  const q = live ? live.layers().quakes : [];
  globe.pointsData(q).pointLat('lat').pointLng('lng').pointAltitude((d) => Math.max(0.005, (d.mag - 2) * 0.025)).pointRadius((d) => 0.18 + d.mag * 0.06)
    .pointColor((d) => (d.depth < 70 ? '#ff9f43' : d.depth < 300 ? '#e8b04b' : '#c9a0ff')).pointResolution(8).pointsMerge(false)
    .pointLabel((d) => `<b>M${d.mag.toFixed(1)}</b> ${esc(d.place || '')}<br><span style="opacity:.75">${new Date(d.time).toUTCString().slice(5, 22)} UTC · ${Math.round(d.depth)} km deep</span>`)
    .onPointClick((d) => showQuake(d));
}
function showQuake(d) {
  flyTo(d.lat, d.lng, 0.8);
  openInfo(`<div class="info-main"><h2>Magnitude ${d.mag.toFixed(1)} earthquake</h2>
    ${meta('activity', '#ff9f43', [esc(d.place || ''), `<span class="num">${esc(new Date(d.time).toUTCString().slice(5, 22))} UTC</span>`])}
    ${figures({ Magnitude: d.mag.toFixed(1), Depth: Math.round(d.depth) + ' km', Location: `${d.lat.toFixed(2)}°, ${d.lng.toFixed(2)}°` })}
    <p class="muted small">Shallow earthquakes (orange, under 70 km) are usually felt most strongly. Data: USGS Earthquake Hazards Program, public domain.</p>
    <div class="actions"><a class="btn primary" href="${esc(d.url)}" target="_blank" rel="noopener">${icon('external-link')}USGS event page</a>${readBtn(null, 'Earthquake')}</div></div>`, null);
}
function showEonet(e) {
  flyTo(e.lat, e.lng, 0.8);
  openInfo(`<div class="info-main"><h2>${esc(e.title)}</h2>
    ${meta(e.icon, '#ff8a7a', [esc(e.catTitle || ''), e.date ? `<span class="num">${esc(new Date(e.date).toUTCString().slice(5, 16))}</span>` : '', coords(e.lat, e.lng)])}
    <p class="summary">An ongoing natural event tracked by NASA's Earth Observatory Natural Event Tracker (EONET).</p>
    ${e.sources.length ? `<h3>Sources</h3><div class="chips-row">${e.sources.map((s) => `<a class="chip" href="${esc(s.url)}" target="_blank" rel="noopener">${icon('external-link')}${esc(s.id)}</a>`).join('')}</div>` : ''}
    <div class="actions">${closeupBtns(e.lat, e.lng)}</div></div>`, null);
}
function showIss(p) {
  openInfo(`<div class="info-main"><h2>International Space Station</h2>
    ${meta('rocket', '#9db1ff', ['Live position', coords(p.lat, p.lng)])}
    ${figures({ Altitude: Math.round(p.alt) + ' km', Speed: Math.round(p.vel).toLocaleString('en-US') + ' km/h', Sunlight: p.vis === 'daylight' ? 'In sunlight' : 'In Earth’s shadow' })}
    <p class="summary">The ISS circles Earth about every 92 minutes, about 16 times a day. Position from wheretheiss.at.</p>
    <div class="actions"><button class="btn primary" data-fly="${p.lat},${p.lng},1.2">${icon('crosshair')}Follow it</button>${readBtn(null, 'International Space Station')}</div></div>`, null);
}

// ============================================================ measuring tool
function startMeasure() {
  if (learn.active()) learn.stop();
  const pts = [];
  toast('Click the first point on the globe.');
  learnCtxPick((pt) => {
    pts.push(pt);
    if (pts.length === 1) { state.guess = { guess: pt, truth: { ...pt, name: 'Start' } }; refreshMarkers(); toast('Now click the second point.'); return; }
    learnCtxPick(null);
    const [a, b] = pts, km = kmBetween(a, b);
    state.guess = { guess: a, truth: { ...b, name: `${Math.round(km).toLocaleString('en-US')} km` } };
    globe.arcsData([{ startLat: a.lat, startLng: a.lng, endLat: b.lat, endLng: b.lng }]);
    refreshMarkers();
    const t = (h) => (h < 1 ? `${Math.round(h * 60)} min` : h < 48 ? `${h.toFixed(1)} h` : `${(h / 24).toFixed(1)} days`);
    openInfo(`<div class="info-main"><h2 class="num">${km < 10 ? km.toFixed(2) : Math.round(km).toLocaleString('en-US')} km</h2>
      ${meta('ruler', '#9db1ff', ['Great-circle distance', `${Math.round(km * 0.621371).toLocaleString('en-US')} miles`, `${Math.round(km / 1.852).toLocaleString('en-US')} nautical miles`])}
      ${figures({ 'Walking (5 km/h)': t(km / 5), 'Driving (80 km/h, straight line)': t(km / 80), 'Airliner (850 km/h)': t(km / 850), 'Light': `${((km / 299792) * 1000).toFixed(2)} ms`, 'Share of Earth’s circumference': `${((km / 40075) * 100).toFixed(1)}%` })}
      <p class="muted small">The shortest path over Earth's surface (a great circle), the route planes and ships approximate. Real roads and flight paths are longer.</p>
      <div class="actions"><button class="btn primary" data-measure-again>${icon('ruler')}Measure again</button><button class="btn" data-measure-clear>${icon('x')}Clear</button></div></div>`, null);
    $('[data-measure-again]', infoBody).onclick = startMeasure;
    $('[data-measure-clear]', infoBody).onclick = () => { state.guess = null; globe.arcsData([]); refreshMarkers(); closeInfo(); };
  });
}
function learnCtxPick(fn) { pickHandler = fn; state.pickMode = !!fn; refreshMarkers(); globeEl.style.cursor = fn ? 'crosshair' : null; }
$('#measure-btn').onclick = startMeasure;

// ============================================================ boot
fillIcons();
document.body.classList.add('show-labels');
const boot = readHash();
setBasemap(state.basemap);
renderTicks();
renderMarks();
paintTempTrack();
renderSide();
buildRangePolys();
setYear(boot.year, true);
onZoom(globe.pointOfView());
if (state.animals.size) refreshMinis();
renderLegend();
if (boot.view) fly({ lat: boot.view[0], lng: boot.view[1], altitude: boot.view[2] }, 0);
if (boot.tab && $(`.rail-btn[data-tab="${boot.tab}"]`)) selectTab(boot.tab);
if (boot.key) setTimeout(() => openByKey(boot.key), 400);
try { if (!localStorage.getItem('ee-visited') && !location.hash) welcome.classList.remove('hidden'); } catch { welcome.classList.remove('hidden'); }
setTimeout(() => $('#loader').classList.add('fade'), 500);
setTimeout(() => $('#loader').remove(), 1400);
// offline support (service worker) on https or localhost
if ('serviceWorker' in navigator && (location.protocol === 'https:' || location.hostname === 'localhost')) navigator.serviceWorker.register('sw.js').catch((e) => console.warn('service worker', e));
window.__earth = { fly, globe, state, setYear, showAnimal, toggleAnimal, toggleResource, showResource, openByKey };

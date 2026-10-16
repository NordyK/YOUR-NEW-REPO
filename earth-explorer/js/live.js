// Live Earth: NASA EPIC whole-disk photographs, a real-time day/night terminator, and
// NASA GIBS daily satellite imagery for any date since February 2000 (follows the timeline).
import { icon } from './icons.js';

const GIBS_START = '2000-02-24';
export const gibsTiles = (date) => (x, y, l) => `https://gibs.earthdata.nasa.gov/wmts/epsg3857/best/MODIS_Terra_CorrectedReflectance_TrueColor/default/${date}/GoogleMapsCompatible_Level9/${l}/${y}/${x}.jpg`;

/** Subsolar point (where the Sun is directly overhead) for a moment in time. */
export function subsolar(d = new Date()) {
  const start = Date.UTC(d.getUTCFullYear(), 0, 0);
  const doy = (d - start) / 864e5;
  const g = ((2 * Math.PI) / 365) * (doy - 1 + (d.getUTCHours() - 12) / 24);
  const decl = 0.006918 - 0.399912 * Math.cos(g) + 0.070257 * Math.sin(g) - 0.006758 * Math.cos(2 * g) + 0.000907 * Math.sin(2 * g) - 0.002697 * Math.cos(3 * g) + 0.00148 * Math.sin(3 * g);
  const eqt = 229.18 * (0.000075 + 0.001868 * Math.cos(g) - 0.032077 * Math.sin(g) - 0.014615 * Math.cos(2 * g) - 0.040849 * Math.sin(2 * g)); // minutes
  const utcMin = d.getUTCHours() * 60 + d.getUTCMinutes() + d.getUTCSeconds() / 60;
  let lng = -((utcMin + eqt) / 4 - 180);
  lng = ((lng + 540) % 360) - 180;
  return { lat: (decl * 180) / Math.PI, lng };
}

export function createLive(ctx) {
  const { THREE, globe, esc } = ctx;
  const st = { shade: false, epic: null, epicFor: '', loading: false, gibsDate: null };

  // ---------------------------------------------------------------- day / night shade
  const mat = new THREE.ShaderMaterial({
    transparent: true, depthWrite: false,
    uniforms: { sunDir: { value: new THREE.Vector3(1, 0, 0) }, strength: { value: 0.62 } },
    // the globe renders with a logarithmic depth buffer, so custom shaders must write log depth too
    vertexShader: `#include <common>
      #include <logdepthbuf_pars_vertex>
      varying vec3 vN;
      void main(){ vN = normalize(position); gl_Position = projectionMatrix * modelViewMatrix * vec4(position,1.0);
        #include <logdepthbuf_vertex>
      }`,
    fragmentShader: `#include <logdepthbuf_pars_fragment>
      uniform vec3 sunDir; uniform float strength; varying vec3 vN;
      void main(){
        #include <logdepthbuf_fragment>
        float d = dot(vN, normalize(sunDir));
        // (smoothstep needs edge0 < edge1, so invert instead of swapping the edges)
        float night = 1.0 - smoothstep(-0.12, 0.06, d);
        float twilight = (1.0 - smoothstep(0.0, 0.12, d)) * (1.0 - night);
        vec3 col = mix(vec3(0.02,0.04,0.12), vec3(0.35,0.18,0.05), twilight * 0.6);
        gl_FragColor = vec4(col, night * strength + twilight * 0.12); }`,
  });
  const shade = new THREE.Mesh(new THREE.SphereGeometry(100.12, 96, 64), mat);
  shade.visible = false;
  shade.renderOrder = 2;
  globe.scene().add(shade);
  function updateSun() {
    const s = subsolar();
    const c = globe.getCoords(s.lat, s.lng, 0);
    mat.uniforms.sunDir.value.set(c.x, c.y, c.z).normalize();
    return s;
  }
  setInterval(() => { if (st.shade) updateSun(); }, 60000);
  function setShade(on) {
    st.shade = on;
    if (on) updateSun();
    shade.visible = on && ctx.state.altitude > 0.003;
  }

  // ---------------------------------------------------------------- EPIC
  async function loadEpic() {
    const y = ctx.state.year;
    const date = y >= 2026 || y < 2015 ? '' : `${y}-07-01`;
    if (st.epicFor === date && st.epic) return;
    st.epicFor = date; st.loading = true; ctx.refresh();
    let list = await ctx.getJSON(`https://epic.gsfc.nasa.gov/api/natural${date ? '/date/' + date : ''}`);
    if (date && (!Array.isArray(list) || !list.length)) list = await ctx.getJSON(`https://epic.gsfc.nasa.gov/api/natural/date/${y}-07-02`);
    st.loading = false;
    st.epic = Array.isArray(list) && list.length ? list : null;
    ctx.refresh();
  }
  const epicUrl = (e, kind = 'thumbs', ext = 'jpg') => { const [d] = e.date.split(' '); const [yy, mm, dd] = d.split('-'); return `https://epic.gsfc.nasa.gov/archive/natural/${yy}/${mm}/${dd}/${kind}/${e.image}.${ext}`; };

  // ---------------------------------------------------------------- GIBS date
  function gibsDateFor(year) {
    if (year < 2000) return null;
    if (year >= 2026) { const d = new Date(Date.now() - 2 * 864e5); return d.toISOString().slice(0, 10); }
    return `${year}-07-15`;
  }

  function renderPanel(el) {
    const s = subsolar();
    const y = ctx.state.year;
    const e = st.epic;
    const pickIdx = e ? Math.min(e.length - 1, Math.floor(e.length / 2)) : 0;
    el.innerHTML = `<div class="side-head"><h2>Live Earth</h2></div>
      <p class="side-desc">The planet as it looks right now, or on a chosen day: photographs of the whole sunlit Earth from 1.5 million km away, daily satellite maps, and where it is day or night.</p>
      <div class="item">${'<span class="dot-ic" style="--c:#f0b54a">' + icon('sun-moon') + '</span>'}<span class="nm">Day and night now<small>Sun overhead at <span class="num">${Math.abs(s.lat).toFixed(1)}° ${s.lat >= 0 ? 'N' : 'S'}, ${Math.abs(s.lng).toFixed(1)}° ${s.lng >= 0 ? 'E' : 'W'}</span></small></span>
        <label class="switch"><input type="checkbox" data-shade ${st.shade ? 'checked' : ''}><span class="track"></span><span class="sr-only">Show day and night</span></label></div>
      <div class="item"><span class="dot-ic" style="--c:#9db1ff">${icon('satellite')}</span><span class="nm">Satellite map of a day<small>${y < 2000 ? 'NASA daily imagery starts in 2000. Move the timeline later.' : `NASA MODIS, ${esc(gibsDateFor(y))}${y >= 2026 ? ' (latest)' : ''}`}</small></span>
        <button class="btn" data-gibs ${y < 2000 ? 'disabled' : ''}>${ctx.state.basemap === 'gibs' ? 'Showing' : 'Show'}</button></div>
      <div class="group-title">The whole Earth from space</div>
      ${st.loading ? '<p class="muted">Fetching NASA EPIC photographs…</p>' : e ? `<figure class="epic"><button class="epic-img" data-epic="${pickIdx}" aria-label="Open the full photograph"><img src="${esc(epicUrl(e[pickIdx]))}" alt="The sunlit face of Earth photographed by NASA's EPIC camera on ${esc(e[pickIdx].date)} UTC"></button>
          <figcaption><span class="num">${esc(e[pickIdx].date)} UTC</span>. Centred on <span class="num">${e[pickIdx].centroid_coordinates.lat.toFixed(1)}°, ${e[pickIdx].centroid_coordinates.lon.toFixed(1)}°</span>. NASA EPIC camera on the DSCOVR spacecraft at the Sun–Earth L1 point.</figcaption></figure>
          <div class="epic-strip">${e.map((x, i) => `<button data-epic="${i}" aria-label="Photo at ${esc(x.date)} UTC"><img src="${esc(epicUrl(x))}" alt="" loading="lazy"><span class="num">${esc(x.date.slice(11, 16))}</span></button>`).join('')}</div>
          <button class="btn" data-epic-fly="${e[pickIdx].centroid_coordinates.lat},${e[pickIdx].centroid_coordinates.lon}">${icon('crosshair')}Turn the globe to match</button>`
        : `<p class="muted">${y < 2015 ? 'EPIC photographs start in June 2015. Move the timeline to 2015 or later.' : 'No EPIC photographs could be loaded for this date.'}</p>`}
      <p class="muted small" style="margin-top:12px">Imagery: NASA EPIC team; NASA GIBS / Worldview (MODIS Terra). Public domain.</p>`;
    el.querySelector('[data-shade]').onchange = (ev) => setShade(ev.target.checked);
    el.querySelector('[data-gibs]')?.addEventListener('click', () => ctx.setBasemap('gibs'));
    el.querySelectorAll('[data-epic]').forEach((b) => (b.onclick = () => ctx.openLightbox(e.map((x) => ({ full: epicUrl(x, 'png', 'png'), fallback: epicUrl(x), caption: `${x.date} UTC, centred on ${x.centroid_coordinates.lat.toFixed(1)}°, ${x.centroid_coordinates.lon.toFixed(1)}°`, file: null })), +b.dataset.epic, 'Earth from DSCOVR/EPIC')));
    el.querySelector('[data-epic-fly]')?.addEventListener('click', (ev) => { const [la, lo] = ev.currentTarget.dataset.epicFly.split(',').map(Number); ctx.fly({ lat: la, lng: lo, altitude: 3.2 }, 1800); });
  }

  return {
    renderPanel, loadEpic, setShade, gibsDateFor,
    onZoom: (alt) => { shade.visible = st.shade && alt > 0.003; },
    get shade() { return st.shade; },
  };
}

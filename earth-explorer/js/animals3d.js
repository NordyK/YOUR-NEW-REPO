// Animated 3D animal miniatures for the globe: glTF models where available, procedural models otherwise.
// Convention for every model: head points +X, up is +Y, feet rest on y = 0.
//
// Procedural animals are built entirely in code. Each one is a single SkinnedMesh (one shared geometry per
// species, so many instances are cheap): smooth lofted forms (superellipse rings swept along spines and limbs)
// bound to a hand-built bone rig. Coats use vertex colours (countershading, masks, socks, baked occlusion) plus
// resolution-independent shader patterns evaluated on the rest pose (stripes, spots, rosettes, giraffe network,
// scutes, mottling), so markings stay crisp up close and fade cleanly to their average colour when tiny.
// Legs are posed by two-bone IK each frame with planted feet and real footfall sequences (walk, trot, pace),
// and every animal mixes locomotion with idle behaviour (grazing, looking around, ear flicks, tail swishes).
import { THREE, GLTFLoader, SkeletonUtils } from '../vendor/earth-libs.js';

export { THREE };

const D = Math.PI / 180;
const MODEL_DIR = new URL('../assets/models/', import.meta.url).href;


// ================================================================ toolkit
const TAU = Math.PI * 2;
const clamp = (v, a = 0, b = 1) => (v < a ? a : v > b ? b : v);
const lerp = (a, b, t) => a + (b - a) * t;
const sstep = (a, b, x) => { const t = clamp((x - a) / (b - a)); return t * t * (3 - 2 * t); };
const fract = (x) => x - Math.floor(x);
const spow = (x, e) => (x < 0 ? -Math.pow(-x, e) : Math.pow(x, e));
const hash1 = (n) => fract(Math.sin(n * 12.9898 + 4.1414) * 43758.5453);
function vnoise(x, y, z) {
  const xi = Math.floor(x), yi = Math.floor(y), zi = Math.floor(z);
  let u = x - xi, v = y - yi, w = z - zi;
  u = u * u * (3 - 2 * u); v = v * v * (3 - 2 * v); w = w * w * (3 - 2 * w);
  const h = (a, b, c) => hash1((xi + a) * 157.31 + (yi + b) * 113.17 + (zi + c) * 271.93);
  return lerp(lerp(lerp(h(0, 0, 0), h(1, 0, 0), u), lerp(h(0, 1, 0), h(1, 1, 0), u), v),
    lerp(lerp(h(0, 0, 1), h(1, 0, 1), u), lerp(h(0, 1, 1), h(1, 1, 1), u), v), w);
}
const fbm = (x, y, z) => vnoise(x, y, z) * 0.55 + vnoise(x * 2.03 + 5.2, y * 2.03, z * 2.03) * 0.3 + vnoise(x * 4.1, y * 4.1 + 1.7, z * 4.1) * 0.15;
/** 1 while walking, 0 while idling: walks for `walk` seconds out of every `per`, with eased ramps. */
function walkEnvelope(tau, per = 16, walk = 10, ramp = 1.3) { const x = ((tau % per) + per) % per; return sstep(0, ramp, x) * (1 - sstep(walk - ramp, walk, x)); }
/** Mostly 0 with a brief 0→1→0 blip once every 1/rate seconds (ear flicks, blinks, tail flicks). */
const twitch = (t, rate, seed = 0, width = 0.06) => { const x = fract(t * rate + seed); return x < width ? Math.sin((x / width) * Math.PI) : 0; };
let _instance = 0;
/** Per-instance time offset so herds of the same species do not move in lock-step (deterministic per page). */
const nextOffset = () => (_instance++ * 7.31) % 41;

// colours ---------------------------------------------------------
const _colCache = new Map();
/** Shared (read-only) linear colour for a hex string. */
function C(h) { if (h && h.isColor) return h; let c = _colCache.get(h); if (!c) { c = new THREE.Color(h); _colCache.set(h, c); } return c; }
const mix = (a, b, t) => new THREE.Color().copy(C(a)).lerp(C(b), clamp(t));
const mul = (a, k) => new THREE.Color().copy(C(a)).multiplyScalar(k);

// coat shader -----------------------------------------------------
// Patterns are evaluated per pixel on the bind-pose position, so they ride along with the skinned surface.
// aPatA = (mode, mask, stripe coordinate, scale), aPatB = (ink colour, parameter).
const PAT = { zebra: 1, tiger: 2, spots: 3, rosettes: 4, openRosettes: 5, network: 6, cells: 7, mottle: 8 };
const PATTERN_GLSL = /* glsl */`
varying vec4 vPatA;
varying vec4 vPatB;
varying vec3 vRest;
float ahH(vec3 p) { p = fract(p * 0.3183099 + 0.1); p *= 17.0; return fract(p.x * p.y * p.z * (p.x + p.y + p.z)); }
float ahN(vec3 x) {
  vec3 i = floor(x), f = fract(x); f = f * f * (3.0 - 2.0 * f);
  return mix(mix(mix(ahH(i), ahH(i + vec3(1.0, 0.0, 0.0)), f.x), mix(ahH(i + vec3(0.0, 1.0, 0.0)), ahH(i + vec3(1.0, 1.0, 0.0)), f.x), f.y),
             mix(mix(ahH(i + vec3(0.0, 0.0, 1.0)), ahH(i + vec3(1.0, 0.0, 1.0)), f.x), mix(ahH(i + vec3(0.0, 1.0, 1.0)), ahH(i + vec3(1.0)), f.x), f.y), f.z);
}
vec3 ahW(vec3 p) {                      // Worley: F1, F2, id of nearest cell
  vec3 i = floor(p), f = fract(p);
  float d1 = 9.0, d2 = 9.0, id = 0.0;
  for (int z = -1; z <= 1; z++) for (int y = -1; y <= 1; y++) for (int x = -1; x <= 1; x++) {
    vec3 g = vec3(float(x), float(y), float(z)), c = i + g;
    vec3 r = g + 0.12 + 0.76 * vec3(ahH(c), ahH(c + 17.3), ahH(c + 41.9)) - f;
    float d = dot(r, r);
    if (d < d1) { d2 = d1; d1 = d; id = ahH(c + 7.7); } else if (d < d2) { d2 = d; }
  }
  return vec3(sqrt(d1), sqrt(d2), id);
}
float ahLess(float v, float thr) { float aa = max(fwidth(v), 1e-4); return 1.0 - smoothstep(thr - aa, thr + aa, v); }
vec3 ahPattern(vec3 base) {
  float mode = floor(vPatA.x + 0.5), mask = vPatA.y;
  if (mode < 0.5 || mask < 0.003) return base;
  float sc = vPatA.w, prm = vPatB.a;
  vec3 ink = vPatB.rgb, p = vRest * sc;
  float px = length(fwidth(p));
  float fade = 1.0 - smoothstep(0.3, 0.8, px);
  float f = 0.0, avg = 0.0;
  if (mode < 2.5) {                     // 1 zebra / 2 tiger stripes along a per-vertex coordinate
    bool tg = mode > 1.5;
    float x = vPatA.z * sc + (ahN(p * (tg ? 0.5 : 0.35)) - 0.5) * (tg ? 1.3 : 0.6) + (ahN(p * 1.9 + 3.1) - 0.5) * (tg ? 0.4 : 0.14);
    float d = abs(fract(x) - 0.5) * 2.0;
    float thr = prm * mask;
    if (tg) thr *= 0.35 + 1.25 * smoothstep(0.2, 0.75, ahN(vec3(floor(x + 0.5) * 7.1, 0.0, 0.0) + p * vec3(0.15, 0.8, 0.8)));
    f = 1.0 - ahLess(d, 1.0 - thr);
    avg = thr * 0.85;
  } else if (mode < 3.5) {              // 3 solid spots
    vec3 w = ahW(p);
    f = ahLess(w.x, prm * mask * (0.75 + 0.45 * w.z));
    avg = 0.22 * mask;
  } else if (mode < 5.5) {              // 4 rosettes with centre dots, 5 open rosettes
    vec3 w = ahW(p);
    float r = prm * (0.82 + 0.3 * w.z);
    float ring = ahLess(abs(w.x - r), prm * 0.2 * (0.4 + 0.6 * mask)) * step(0.28, ahN(p * 3.1 + w.z * 9.0));
    base = mix(base, base * 0.78, (1.0 - smoothstep(r - 0.06, r, w.x)) * mask * 0.85);
    f = ring * smoothstep(0.0, 0.4, mask);
    if (mode < 4.5) f = max(f, ahLess(w.x, prm * 0.17) * step(0.5, w.z) * mask);
    avg = 0.3 * mask;
  } else if (mode < 6.5) {              // 6 giraffe network: ink = pale lines between patches
    vec3 w = ahW(p);
    base *= 0.84 + 0.32 * w.z;
    f = ahLess(w.y - w.x, prm + (1.0 - mask) * 1.5);
    avg = 0.4;
  } else if (mode < 7.5) {              // 7 cells: scutes / scales, ink = seams
    vec3 w = ahW(p);
    base *= 0.84 + 0.28 * w.z + 0.18 * (ahN(p * 5.0) - 0.5);
    f = ahLess(w.y - w.x, prm) * mask;
    avg = 0.2 * mask;
  } else {                              // 8 mottle
    float n = ahN(p) * 0.6 + ahN(p * 2.3 + 4.0) * 0.4;
    f = smoothstep(0.56, 0.64, n) * mask;
    avg = 0.15 * mask;
  }
  return mix(base, ink, clamp(mix(avg, f, fade), 0.0, 1.0));
}
`;
let _mats = null;
function animalMaterials() {
  if (_mats) return _mats;
  const coat = new THREE.MeshStandardMaterial({ vertexColors: true, roughness: 0.84, metalness: 0 });
  coat.onBeforeCompile = (sh) => {
    sh.vertexShader = sh.vertexShader
      .replace('#include <common>', '#include <common>\nattribute vec4 aPatA;\nattribute vec4 aPatB;\nvarying vec4 vPatA;\nvarying vec4 vPatB;\nvarying vec3 vRest;')
      .replace('#include <begin_vertex>', '#include <begin_vertex>\nvPatA = aPatA; vPatB = aPatB; vRest = position;');
    sh.fragmentShader = sh.fragmentShader
      .replace('#include <common>', '#include <common>\n' + PATTERN_GLSL)
      .replace('#include <color_fragment>', '#include <color_fragment>\ndiffuseColor.rgb = ahPattern(diffuseColor.rgb);');
  };
  coat.customProgramCacheKey = () => 'animal-coat-1';
  const gloss = new THREE.MeshStandardMaterial({ vertexColors: true, roughness: 0.2, metalness: 0 });
  _mats = [coat, gloss];
  return _mats;
}

// geometry builder -------------------------------------------------
const V3 = (a) => (a.isVector3 ? a.clone() : new THREE.Vector3(a[0], a[1], a[2] || 0));
function mono(vals) {                    // monotone cubic interpolation over station index (no overshoot)
  const n = vals.length;
  if (n === 1) return () => vals[0];
  const d = [], m = new Array(n);
  for (let k = 0; k < n - 1; k++) d.push(vals[k + 1] - vals[k]);
  m[0] = d[0]; m[n - 1] = d[n - 2];
  for (let k = 1; k < n - 1; k++) m[k] = d[k - 1] * d[k] <= 0 ? 0 : (d[k - 1] + d[k]) / 2;
  for (let k = 0; k < n - 1; k++) {
    if (d[k] === 0) { m[k] = m[k + 1] = 0; continue; }
    const a = m[k] / d[k], b = m[k + 1] / d[k], s = a * a + b * b;
    if (s > 9) { const t = 3 / Math.sqrt(s); m[k] = t * a * d[k]; m[k + 1] = t * b * d[k]; }
  }
  return (q) => {
    const i = Math.min(Math.max(Math.floor(q), 0), n - 2), t = q - i, t2 = t * t, t3 = t2 * t;
    return (2 * t3 - 3 * t2 + 1) * vals[i] + (t3 - 2 * t2 + t) * m[i] + (-2 * t3 + 3 * t2) * vals[i + 1] + (t3 - t2) * m[i + 1];
  };
}
function catmull(vals) {                 // Catmull-Rom through values (for centre lines)
  const n = vals.length;
  return (q) => {
    if (n === 1) return vals[0];
    const i = Math.min(Math.max(Math.floor(q), 0), n - 2), t = q - i;
    const p1 = vals[i], p2 = vals[i + 1], p0 = i > 0 ? vals[i - 1] : 2 * p1 - p2, p3 = i + 2 < n ? vals[i + 2] : 2 * p2 - p1;
    const t2 = t * t, t3 = t2 * t;
    return 0.5 * (2 * p1 + (-p0 + p2) * t + (2 * p0 - 5 * p1 + 4 * p2 - p3) * t2 + (-p0 + 3 * p1 - 3 * p2 + p3) * t3);
  };
}
function vertexNormals(pos, idx) {
  const n = new Float32Array(pos.length);
  for (let i = 0; i < idx.length; i += 3) {
    const a = idx[i] * 3, b = idx[i + 1] * 3, c = idx[i + 2] * 3;
    const ux = pos[b] - pos[a], uy = pos[b + 1] - pos[a + 1], uz = pos[b + 2] - pos[a + 2];
    const vx = pos[c] - pos[a], vy = pos[c + 1] - pos[a + 1], vz = pos[c + 2] - pos[a + 2];
    const nx = uy * vz - uz * vy, ny = uz * vx - ux * vz, nz = ux * vy - uy * vx;
    for (const k of [a, b, c]) { n[k] += nx; n[k + 1] += ny; n[k + 2] += nz; }
  }
  for (let i = 0; i < n.length; i += 3) { const l = Math.hypot(n[i], n[i + 1], n[i + 2]) || 1; n[i] /= l; n[i + 1] /= l; n[i + 2] /= l; }
  return n;
}
const GEO = new Map();                   // species -> shared skinned geometry
const _hitRay = new THREE.Ray(), _hitM = new THREE.Matrix4(), _hitP = new THREE.Vector3();
/** Cheap picking for skinned animals: test the (generous) rest-pose bounding box instead of every skinned triangle. */
function boxRaycast(raycaster, hits) {
  _hitM.copy(this.matrixWorld).invert();
  _hitRay.copy(raycaster.ray).applyMatrix4(_hitM);
  if (!_hitRay.intersectBox(this.userData.hitBox, _hitP)) return;
  _hitP.applyMatrix4(this.matrixWorld);
  const d = raycaster.ray.origin.distanceTo(_hitP);
  if (d >= raycaster.near && d <= raycaster.far) hits.push({ distance: d, point: _hitP.clone(), object: this });
}

/**
 * Collects bones and skinned geometry for one animal. All coordinates are given in the rest (bind) pose in
 * model space; bones have identity rest rotations, so animating is just setting rotations relative to rest.
 */
class Body {
  constructor(key) {
    this.key = key;
    this.skip = GEO.has(key);             // geometry already built for this species: only rebuild the rig
    this.bones = []; this.bn = {};
    this.P = []; this.Nm = []; this.Cl = []; this.PA = []; this.PB = []; this.SI = []; this.SW = []; this.I = [[], []];
  }
  bone(name, parent, p) {
    const b = new THREE.Bone();
    b.name = name;
    const w = V3(p);
    b.userData.rest = w;
    if (parent) { const pb = this.bn[parent]; b.position.copy(w).sub(pb.userData.rest); pb.add(b); } else b.position.copy(w);
    b.userData.i = this.bones.length;
    this.bones.push(b);
    this.bn[name] = b;
    return b;
  }
  W(spec) {
    if (Array.isArray(spec)) return spec;
    if (typeof spec === 'string') spec = { [spec]: 1 };
    const arr = [];
    for (const k in spec) { if (spec[k] > 1e-4) { if (!this.bn[k]) throw new Error('unknown bone ' + k); arr.push([this.bn[k].userData.i, spec[k]]); } }
    return normW(arr);
  }
  _vert(q, o, wts) {
    let c = typeof o.paint === 'function' ? o.paint(q) : o.paint || '#888';
    c = C(c);
    let k = 1;
    if (o.ao) k = 1 - o.ao * sstep(0.1, -0.9, q.c);          // darken the underside (fake occlusion)
    this.P.push(q.p.x, q.p.y, q.p.z);
    this.Nm.push(q.n.x, q.n.y, q.n.z);
    this.Cl.push(c.r * k, c.g * k, c.b * k);
    const pt = o.pat;
    if (pt) {
      const ink = C(pt.ink);
      this.PA.push(pt.mode, pt.mask ? pt.mask(q) : 1, pt.coord ? pt.coord(q) : q.p.x, pt.scale);
      this.PB.push(ink.r, ink.g, ink.b, pt.prm ?? 0.5);
    } else { this.PA.push(0, 0, 0, 0); this.PB.push(0, 0, 0, 0); }
    for (let i = 0; i < 4; i++) { const e = wts[i]; this.SI.push(e ? e[0] : 0); this.SW.push(e ? e[1] : 0); }
  }
  /**
   * Smooth lofted tube. Stations: { p: [x,y,z], r: [side, dorsal, ventral] | r, b: bone weights }.
   * o: around, step, ref (initial "dorsal" direction), axis (fixed sweep direction), sq: [topExp, botExp],
   * cap0/cap1 (end-cap length in radii, 0 = flat), paint(q), pat, ao, disp(q) radial displacement, mat, part.
   */
  loft(st, o = {}) {
    if (this.skip) return;
    const n = st.length;
    const pts = st.map((s) => V3(s.p));
    const R = st.map((s) => { const r = typeof s.r === 'number' ? [s.r] : s.r; return [r[0], r[1] ?? r[0], r[2] ?? r[1] ?? r[0]]; });
    const WT = st.map((s) => this.W(s.b || o.b));
    const fx = catmull(pts.map((p) => p.x)), fy = catmull(pts.map((p) => p.y)), fz = catmull(pts.map((p) => p.z));
    const rw = mono(R.map((r) => r[0])), ru = mono(R.map((r) => r[1])), rd = mono(R.map((r) => r[2]));
    const around = o.around || 12;
    const eT = 2 / (o.sq ? o.sq[0] : 2), eB = 2 / (o.sq ? (o.sq[1] ?? o.sq[0]) : 2), eS = 2 / (o.sqs || 2);
    let mean = 0;
    R.forEach((r) => (mean += Math.min(r[0], (r[1] + r[2]) / 2)));
    mean /= n;
    const step = o.step || Math.max(1e-3, mean * 0.55);
    // ring parameters along the stations
    const qs = [];
    for (let i = 0; i < n - 1; i++) {
      const L = pts[i].distanceTo(pts[i + 1]);
      const k = Math.max(o.minSeg || 1, Math.round(L / step));
      for (let j = 0; j < k; j++) qs.push(i + j / k);
    }
    qs.push(n - 1);
    const at = (q) => new THREE.Vector3(fx(q), fy(q), fz(q));
    const rings = qs.map((q) => {
      const P = at(q);
      const T = o.axis ? V3(o.axis).normalize() : at(Math.min(n - 1, q + 0.02)).sub(at(Math.max(0, q - 0.02))).normalize();
      return { q, P, T, r: [Math.max(1e-4, rw(q)), Math.max(1e-4, ru(q)), Math.max(1e-4, rd(q))], s: 1 };
    });
    // end caps: shrinking rings + a pole vertex
    const capRings = (end, len) => {
      const out = [], m = o.capRings || 3, r0 = end.r, e = len * (r0[0] + (r0[1] + r0[2]) / 2) / 2, sgn = end === rings[0] ? -1 : 1;
      for (let k = 1; k <= m; k++) {
        const th = (k / m) * Math.PI / 2;
        out.push({ q: end.q, P: end.P.clone().addScaledVector(end.T, sgn * e * Math.sin(th)), T: end.T, r: r0, s: Math.max(0, Math.cos(th)) });
      }
      return out;
    };
    const c0 = o.cap0 ?? 1, c1 = o.cap1 ?? 1;
    const head = c0 > 0 ? capRings(rings[0], c0).reverse() : [{ ...rings[0], s: 0 }];
    const tail = c1 > 0 ? capRings(rings[rings.length - 1], c1) : [{ ...rings[rings.length - 1], s: 0 }];
    const all = [...head, ...rings, ...tail];
    // parallel-transported frames
    let N = V3(o.ref || [0, 1, 0]);
    const T0 = rings[0].T;
    N.addScaledVector(T0, -N.dot(T0));
    if (N.lengthSq() < 1e-6) N.set(1, 0, 0).addScaledVector(T0, -T0.x);
    N.normalize();
    const pos = [], meta = [];
    const nr = all.length;
    all.forEach((rg, i) => {
      const T = rg.T;
      N.addScaledVector(T, -N.dot(T)).normalize();
      const B = new THREE.Vector3().crossVectors(T, N);
      const [w, u, d] = rg.r, s = rg.s;
      const isPole = s === 0;
      for (let j = 0; j < (isPole ? 1 : around); j++) {
        const a = (j / around) * TAU, ca = Math.cos(a), sa = Math.sin(a);
        const vv = isPole ? 0 : (ca >= 0 ? spow(ca, eT) * u : spow(ca, eB) * d) * s;
        const zz = isPole ? 0 : spow(sa, eS) * w * s;
        const p = rg.P.clone().addScaledVector(N, vv).addScaledVector(B, zz);
        meta.push({ q: rg.q, a, c: isPole ? 0 : ca, s: isPole ? 0 : sa, ring: i, P: rg.P, pole: isPole });
        if (o.disp && !isPole) {
          const dq = { p, a, c: ca, s: sa, u: rg.q / (n - 1), part: o.part };
          const r = o.disp(dq);
          if (r) p.add(new THREE.Vector3().subVectors(p, rg.P).normalize().multiplyScalar(r));
        }
        pos.push(p);
      }
    });
    // indices
    const idx = [], start = [];
    let k = 0;
    all.forEach((rg) => { start.push(k); k += rg.s === 0 ? 1 : around; });
    for (let i = 0; i < nr - 1; i++) {
      const A = start[i], Bn = start[i + 1], pa = all[i].s === 0, pb = all[i + 1].s === 0;
      for (let j = 0; j < around; j++) {
        const j2 = (j + 1) % around;
        if (pa && pb) continue;
        if (pa) idx.push(A, Bn + j2, Bn + j);
        else if (pb) idx.push(A + j, A + j2, Bn);
        else idx.push(A + j, A + j2, Bn + j, A + j2, Bn + j2, Bn + j);
      }
    }
    const flat = new Float32Array(pos.length * 3);
    pos.forEach((p, i) => flat.set([p.x, p.y, p.z], i * 3));
    const nrm = vertexNormals(flat, idx);
    const base = this.P.length / 3;
    const q = { part: o.part, side: o.side ?? 0, n: new THREE.Vector3() };
    pos.forEach((p, i) => {
      const m = meta[i];
      q.p = p; q.u = m.q / (n - 1); q.a = m.a; q.c = m.c; q.s = m.s; q.st = m.q; q.pole = m.pole;
      q.n.set(nrm[i * 3], nrm[i * 3 + 1], nrm[i * 3 + 2]);
      if (m.pole) q.c = q.n.y;
      const i0 = Math.min(Math.floor(m.q), n - 2), f = m.q - i0;
      const wts = n === 1 ? WT[0] : blendW(WT[i0], WT[Math.min(n - 1, i0 + 1)], f);
      this._vert(q, o, wts);
    });
    const I = this.I[o.mat || 0];
    idx.forEach((v) => I.push(base + v));
  }
  /** Any BufferGeometry already in rest-pose model space, rigidly bound to `b`. */
  part(geo, b, o = {}) {
    if (this.skip) { geo.dispose(); return; }
    if (o.m) geo.applyMatrix4(o.m);
    if (geo.index === null) { const ix = []; for (let i = 0; i < geo.attributes.position.count; i++) ix.push(i); geo.setIndex(ix); }
    if (!geo.attributes.normal) geo.computeVertexNormals();
    const pos = geo.attributes.position, nor = geo.attributes.normal, wts = this.W(b);
    const base = this.P.length / 3;
    const q = { part: o.part, side: o.side ?? 0, p: new THREE.Vector3(), n: new THREE.Vector3(), u: 0, a: 0, s: 0 };
    for (let i = 0; i < pos.count; i++) {
      q.p.fromBufferAttribute(pos, i); q.n.fromBufferAttribute(nor, i); q.c = q.n.y; q.s = q.n.z;
      this._vert(q, o, wts);
    }
    const I = this.I[o.mat || 0];
    for (let i = 0; i < geo.index.count; i++) I.push(base + geo.index.getX(i));
    geo.dispose();
  }
  /** Ellipsoid at `c` with radii `r`, optional Euler rotation `rot`. */
  ell(c, r, b, paint, o = {}) {
    if (this.skip) return;
    const g = new THREE.SphereGeometry(1, o.seg || 12, Math.max(6, Math.round((o.seg || 12) * 0.7)));
    g.scale(r[0], r[1], r[2]);
    if (o.rot) g.applyMatrix4(new THREE.Matrix4().makeRotationFromEuler(new THREE.Euler(...o.rot, o.order || 'XYZ')));
    g.translate(...(c.isVector3 ? c.toArray() : c));
    this.part(g, b, { ...o, paint });
  }
  /** Glossy eye (dark sphere with a hint of iris colour). */
  eye(c, r, b, iris = '#1a120b') {
    this.ell(c, [r, r, r], b, (q) => (q.n.y > 0.55 ? mul(iris, 0.6) : iris), { mat: 1, seg: 10 });
  }
  finish() {
    let geo = GEO.get(this.key);
    if (!geo) {
      geo = new THREE.BufferGeometry();
      geo.setAttribute('position', new THREE.Float32BufferAttribute(this.P, 3));
      geo.setAttribute('normal', new THREE.Float32BufferAttribute(this.Nm, 3));
      geo.setAttribute('color', new THREE.Float32BufferAttribute(this.Cl, 3));
      geo.setAttribute('aPatA', new THREE.Float32BufferAttribute(this.PA, 4));
      geo.setAttribute('aPatB', new THREE.Float32BufferAttribute(this.PB, 4));
      geo.setAttribute('skinIndex', new THREE.Uint16BufferAttribute(this.SI, 4));
      geo.setAttribute('skinWeight', new THREE.Float32BufferAttribute(this.SW, 4));
      const [i0, i1] = this.I;
      geo.setIndex(i0.concat(i1));
      geo.addGroup(0, i0.length, 0);
      if (i1.length) geo.addGroup(i0.length, i1.length, 1);
      geo.computeBoundingBox();
      geo.computeBoundingSphere();
      geo.userData.hitBox = geo.boundingBox.clone().expandByScalar(0.04 * geo.boundingSphere.radius);
      GEO.set(this.key, geo);
    }
    const mesh = new THREE.SkinnedMesh(geo, animalMaterials());
    mesh.frustumCulled = false;
    mesh.userData.hitBox = geo.userData.hitBox;
    mesh.raycast = boxRaycast;
    const root = new THREE.Group();
    root.add(mesh);
    root.add(this.bones[0]);
    root.updateMatrixWorld(true);
    mesh.bind(new THREE.Skeleton(this.bones));
    return { root, mesh, B: this.bn };
  }
}
function normW(arr) {
  arr.sort((a, b) => b[1] - a[1]);
  const top = arr.slice(0, 4), s = top.reduce((a, e) => a + e[1], 0) || 1;
  return top.map(([i, w]) => [i, w / s]);
}
function blendW(a, b, t) {
  if (a === b) return a;
  const m = new Map();
  a.forEach(([i, w]) => m.set(i, (m.get(i) || 0) + w * (1 - t)));
  b.forEach(([i, w]) => m.set(i, (m.get(i) || 0) + w * t));
  return normW([...m.entries()].filter((e) => e[1] > 1e-4));
}

// rig helpers ----------------------------------------------------------
/** 2-D helpers in a leg's sagittal plane. Segment angle phi: 0 = pointing straight down, + = lower end forward. */
const angOf = (dx, dy) => Math.atan2(dx, -dy);
/** Two-bone IK from (ax,ay) towards (tx,ty). bend = ±1 picks the side of the middle joint. Returns [phi0, phi1]. */
function ik2(ax, ay, tx, ty, l0, l1, bend) {
  let dx = tx - ax, dy = ty - ay, d = Math.hypot(dx, dy) || 1e-6;
  const dmax = (l0 + l1) * 0.999, dmin = Math.abs(l0 - l1) * 1.001 + 1e-5;
  const dc = clamp(d, dmin, dmax);
  dx *= dc / d; dy *= dc / d;
  const base = Math.atan2(dy, dx), A = Math.acos(clamp((l0 * l0 + dc * dc - l1 * l1) / (2 * l0 * dc), -1, 1)) * bend;
  const kx = Math.cos(base + A) * l0, ky = Math.sin(base + A) * l0;
  return [angOf(kx, ky), angOf(dx - kx, dy - ky)];
}


// ================================================================ quadruped rig
/** A 2-D frame in the sagittal plane: origin (x,y) and an axis angle in degrees (0 = +X, negative = pointing down). */
function frame2(x, y, angDeg) {
  const a = angDeg * D, d = new THREE.Vector3(Math.cos(a), Math.sin(a), 0), u = new THREE.Vector3(-Math.sin(a), Math.cos(a), 0), o = new THREE.Vector3(x, y, 0);
  return {
    o, d, u,
    pt: (px, py = 0, pz = 0) => o.clone().addScaledVector(d, px).addScaledVector(u, py).setZ(pz),
    local: (p) => { const dx = p.x - x, dy = p.y - y; return [dx * d.x + dy * d.y, dx * u.x + dy * u.y]; },
  };
}
const rot2 = (x, y, a) => [x * Math.cos(a) - y * Math.sin(a), x * Math.sin(a) + y * Math.cos(a)];
/** Footfall timing (fraction of the cycle at which each foot touches down) for symmetrical gaits. */
const GAITS = {
  walk: { lf: 0.25, rf: 0.75, lh: 0, rh: 0.5 },
  trot: { lf: 0, rf: 0.5, lh: 0.5, rh: 0 },
  pace: { lf: 0.06, rf: 0.56, lh: 0, rh: 0.5 },
};

function buildLeg(b, name, parent, spec, side, o) {
  const z = spec.z * side, J = spec.j, n = J.length - 1;
  const bones = [];
  for (let i = 0; i < n; i++) bones.push(b.bone(name + i, i ? name + (i - 1) : parent, [J[i][0], J[i][1], z + (spec.dz ? spec.dz[i] * side : 0)]));
  const len = [], pr = [];
  for (let i = 0; i < n; i++) { const dx = J[i + 1][0] - J[i][0], dy = J[i + 1][1] - J[i][1]; len.push(Math.hypot(dx, dy)); pr.push(angOf(dx, dy)); }
  const rr = (r) => [r[1], r[0], r[2] ?? r[0]];
  const st = [];
  if (spec.top) st.push({ p: [spec.top[0], spec.top[1], z], r: rr(spec.top.slice(2)), b: { [parent]: 1 } });
  for (let i = 0; i <= n; i++) {
    const zz = z + (spec.dz && i < n ? spec.dz[i] * side : spec.dz ? spec.dz[n - 1] * side : 0);
    const wts = i === 0 ? { [parent]: spec.top ? 0.3 : 0.15, [name + 0]: spec.top ? 0.7 : 0.85 } : i === n ? { [name + (n - 1)]: 1 } : { [name + (i - 1)]: 0.5, [name + i]: 0.5 };
    st.push({ p: [J[i][0], J[i][1], zz], r: rr(spec.r[i]), b: wts });
    const m = spec.mid && spec.mid[i];
    if (i < n && m) {
      const t = m[3] ?? 0.45;
      st.push({ p: [lerp(J[i][0], J[i + 1][0], t), lerp(J[i][1], J[i + 1][1], t), zz], r: rr(m), b: { [name + i]: 1 } });
    }
  }
  b.loft(st, { ref: [1, 0, 0], around: spec.around || 10, part: o.part, side, paint: o.paint, pat: o.pat, cap0: 0.7, cap1: spec.cap1 ?? 0.6, step: spec.step });
  if (spec.hoof) {
    const h = spec.hoof, g = new THREE.CylinderGeometry(h.r0, h.r1, h.h, 12, 1);
    const [x, y] = J[n];
    g.translate(x + (h.dx || 0), y - h.h / 2 + (h.dy || 0), z);
    b.part(g, name + (n - 1), { paint: h.color, part: 'hoof' });
  }
  const bend = Math.sign((J[2][0] - J[0][0]) * (J[1][1] - J[0][1]) - (J[2][1] - J[0][1]) * (J[1][0] - J[0][0])) || 1;
  return {
    bones, n, len, pr, bend, z, gx: J[n][0], gy: J[n][1], j0: J[0],
    a0: angOf(J[n][0] - J[0][0], J[n][1] - J[0][1]),
    toeOff: spec.toeOff ?? 0.5, flex: spec.flex ?? 0.9, cflex: spec.cflex ?? 0, follow: spec.follow ?? 0.7, sk: spec.strideK ?? 1, lk: spec.liftK ?? 1,
  };
}
/** Pose one leg: foot follows the gait cycle (stance on the ground, arcing swing), joints solved by IK. */
function poseLeg(L, j0x, j0y, pd, s, w, g) {
  const sl = fract(s + L.ph), duty = g.duty;
  let dx, dy = 0, fl, cf = 0;
  if (sl < duty) { const q = sl / duty; dx = 0.5 - q; fl = sstep(0.6, 1, q) * L.toeOff; }
  else {
    const q = (sl - duty) / (1 - duty);
    dx = -0.5 + (1 - Math.cos(Math.PI * q)) / 2;
    dy = Math.sin(Math.PI * Math.pow(q, 0.85));
    fl = L.toeOff * (1 - sstep(0, 0.6, q)) + Math.sin(Math.PI * Math.min(1, q * 1.15)) * L.flex;
    cf = Math.sin(Math.PI * Math.min(1, q * 1.3));
  }
  const gx = L.gx + dx * g.stride * L.sk * w + (L.shift || 0), gy = L.gy + dy * g.lift * L.lk * w + (L.raise || 0);
  const n = L.n, phi = L._phi || (L._phi = new Array(n));
  phi[n - 1] = L.pr[n - 1] - fl * w;
  let tx = gx - Math.sin(phi[n - 1]) * L.len[n - 1], ty = gy + Math.cos(phi[n - 1]) * L.len[n - 1];
  if (n === 4) {
    const al = angOf(gx - j0x, gy - j0y);
    phi[2] = L.pr[2] + (al - L.a0) * L.follow - cf * L.cflex * w;
    tx -= Math.sin(phi[2]) * L.len[2]; ty += Math.cos(phi[2]) * L.len[2];
  }
  const [p0, p1] = ik2(j0x, j0y, tx, ty, L.len[0], L.len[1], L.bend);
  phi[0] = p0; phi[1] = p1;
  let prev = pd;
  for (let i = 0; i < n; i++) { const dl = phi[i] - L.pr[i]; L.bones[i].rotation.z = dl - prev; prev = dl; }
}

/**
 * Generic quadruped: torso + neck + head lofts, IK legs, tail, ears, plus species extras.
 * S.torso: [[x, top, bottom, halfWidth, widestAt(0..1)]] rear→front (side-view silhouette, exact).
 * S.neck: [[x, y, halfWidth, dorsalR, ventralR]] base (inside chest) → poll.  S.head: { ang, st: [[x, top, bot, w, k]] }.
 * S.legF/legH: { j: joints top→foot [[x,y]], r: radii per joint [front-back, side, back?], mid, top, z, hoof }.
 */
function buildQuad(S) {
  const b = new Body(S.key);
  const [hx, hy] = S.hip, [sx, sy] = S.sh;
  b.bone('root', null, [0, 0, 0]);
  const bodyRest = new THREE.Vector3((hx + sx) / 2, (hy + sy) / 2, 0);
  b.bone('body', 'root', bodyRest);
  b.bone('pelvis', 'body', [hx, hy, 0]);
  b.bone('chest', 'body', [sx, sy, 0]);
  const nk = S.neck, nn = nk.length, poll = nk[nn - 1];
  const HF = frame2(poll[0] + (S.head.off?.[0] || 0), poll[1] + (S.head.off?.[1] || 0), S.head.ang);
  const paint = (q) => { q.hl = HF.local(q.p); return S.paint(q); };
  const pat = S.pat || (() => null);
  const spineW = (x) => { const t = sstep(hx, sx, x); return { pelvis: 1 - t, chest: t }; };
  // torso
  const tx0 = S.torso[0][0], tx1 = S.torso[S.torso.length - 1][0];
  b.loft(S.torso.map(([x, top, bot, w, k = 0.5]) => { const c = bot + (top - bot) * k; return { p: [x, c, 0], r: [w, top - c, c - bot], b: spineW(x) }; }),
    { axis: [1, 0, 0], around: S.torsoAround || 18, sq: S.torsoSq || [2.2, 2], cap0: S.rumpCap ?? 0.8, cap1: S.chestCap ?? 0.8, part: 'torso', paint, pat: pat('torso'), ao: S.ao ?? 0.22, step: (tx1 - tx0) / (S.torsoRings || 26), disp: S.torsoDisp });
  // neck
  for (let i = 0; i < nn - 1; i++) b.bone('n' + i, i ? 'n' + (i - 1) : 'chest', [nk[i][0], nk[i][1], 0]);
  b.bone('head', 'n' + (nn - 2), HF.o);
  b.loft(nk.map(([x, y, w, up, dn], i) => ({ p: [x, y, 0], r: [w, up, dn ?? up], b: i === 0 ? { chest: 0.5, n0: 0.5 } : i === nn - 1 ? 'head' : 'n' + i })),
    { ref: [0, 1, 0], around: S.neckAround || 14, part: 'neck', paint, pat: pat('neck'), cap0: 0.6, cap1: 0.6, ao: S.ao ?? 0.22, disp: S.neckDisp, step: S.neckStep });
  // head
  const H = S.head;
  b.loft(H.st.map(([x, top, bot, w, k = 0.5]) => { const c = bot + (top - bot) * k; return { p: HF.pt(x, c), r: [w, top - c, c - bot], b: 'head' }; }),
    { axis: HF.d, ref: HF.u, around: H.around || 14, sq: H.sq || [2, 2], part: 'head', paint, pat: pat('head'), cap0: H.cap0 ?? 0.8, cap1: H.cap1 ?? 0.6, ao: 0.12, step: H.step });
  if (H.eye) { const [ex, ey, ez, er, iris] = H.eye; [1, -1].forEach((s) => b.eye(HF.pt(ex, ey, ez * s), er, 'head', iris)); }
  // legs
  const legs = [];
  const gk = GAITS[S.gait.kind || 'walk'];
  [['lf', S.legF, 'chest', 1], ['rf', S.legF, 'chest', -1], ['lh', S.legH, 'pelvis', 1], ['rh', S.legH, 'pelvis', -1]].forEach(([nm, spec, parent, side]) => {
    const L = buildLeg(b, nm, parent, spec, side, { paint, pat: pat(parent === 'chest' ? 'legF' : 'legH'), part: parent === 'chest' ? 'legF' : 'legH' });
    L.ph = 1 - gk[nm]; L.front = parent === 'chest'; L.parent = parent;
    legs.push(L);
  });
  // tail
  const tails = [];
  if (S.tail) {
    const tp = S.tail.pts;
    tp.slice(0, -1).forEach((p, i) => tails.push(b.bone('t' + i, i ? 't' + (i - 1) : 'pelvis', [p[0], p[1], 0])));
    b.loft(tp.map(([x, y, r, r2], i) => ({ p: [x, y, 0], r: [r, r2 ?? r, r2 ?? r], b: 't' + Math.min(i, tp.length - 2) })),
      { ref: [1, 0, 0], around: S.tail.around || 8, part: 'tail', paint, pat: pat('tail'), cap0: 0.5, cap1: S.tail.cap ?? 1, disp: S.tail.disp });
  }
  // ears
  const ears = [];
  if (S.ears) {
    const E = S.ears;
    [1, -1].forEach((s) => {
      const base = HF.pt(E.at[0], E.at[1], E.at[2] * s);
      const nm = s > 0 ? 'earL' : 'earR';
      b.bone(nm, 'head', base);
      const dir = new THREE.Vector3(...E.dir).normalize(); dir.z *= s;
      const dirW = HF.d.clone().multiplyScalar(dir.x).addScaledVector(HF.u, dir.y).setZ(dir.z).normalize();
      const prof = E.prof || [[0, 0.55], [0.35, 1], [0.75, 0.75], [1, 0.12]];
      const flat = new THREE.Vector3(...(E.face || [0, 0, 1])); flat.z *= s;
      b.loft(prof.map(([t, wk]) => ({ p: base.clone().addScaledVector(dirW, t * E.len), r: [E.w * wk, E.th, E.th], b: nm })),
        { ref: HF.d.clone().negate(), around: 10, part: 'ear', side: s, paint, cap0: 0.4, cap1: E.cap ?? 0.6, sqs: E.sqs || 2 });
      ears.push({ bone: b.bn[nm], s });
    });
  }
  if (S.extras) S.extras(b, { HF, paint, pat, legs });
  const R = b.finish();
  const B = R.B;
  const g = S.gait, ofs = nextOffset();
  const pOff = B.pelvis.position.clone(), cOff = B.chest.position.clone();
  const neck = [];
  for (let i = 0; i < nn - 1; i++) neck.push(B['n' + i]);
  const st = { w: 0, s: 0, tau: 0, idle: 0 };
  return {
    root: R.root,
    update(t) {
      const tau = t + ofs;
      const w = S.walkFn ? S.walkFn(tau) : walkEnvelope(tau, g.per || 17, g.walk || 10);
      const s = fract(tau * g.hz);
      st.w = w; st.s = s; st.tau = tau; st.idle = 1 - w;
      // body: bob twice per stride, slight pitch and sway
      const bob = (g.bob || 0) * w * Math.cos(TAU * 2 * (s + (g.bobPh || 0)));
      const pitch = (g.pitch || 0) * w * Math.sin(TAU * 2 * (s + (g.bobPh || 0))) + (S.idlePitch ? S.idlePitch(st) : 0);
      B.body.position.set(bodyRest.x, bodyRest.y + bob + (S.crouch ? S.crouch(st) : 0), 0);
      B.body.rotation.set((g.roll || 0) * w * Math.sin(TAU * s), 0, pitch);
      const pr = (g.spine || 0) * w * Math.sin(TAU * 2 * s), cr = -pr;
      B.pelvis.rotation.z = pr; B.chest.rotation.z = cr;
      for (const L of legs) {
        const off = L.parent === 'chest' ? cOff : pOff, pa = L.parent === 'chest' ? cr : pr;
        const [lx, ly] = rot2(L.j0[0] - off.x - bodyRest.x, L.j0[1] - off.y - bodyRest.y, pa);
        const [wx, wy] = rot2(off.x + lx, off.y + ly, pitch);
        poseLeg(L, B.body.position.x + wx, B.body.position.y + wy, pitch + pa, s, w, g);
      }
      // neck & head: nod with the stride while walking; idle behaviour otherwise
      const nod = (g.nod || 0) * w * Math.sin(TAU * 2 * (s + 0.1));
      const look = Math.sin(tau * 0.31) * 0.5 + Math.sin(tau * 0.77) * 0.25;
      const head = S.headFn ? S.headFn(st) : { down: 0, yaw: look * (0.25 + 0.5 * st.idle), tilt: 0 };
      neck.forEach((nb, i) => {
        nb.rotation.z = nod / neck.length - (head.down || 0) * (S.grazeW ? S.grazeW[i] : 1 / neck.length);
        nb.rotation.y = (head.yaw || 0) / (neck.length + 1);
      });
      B.head.rotation.set(0, (head.yaw || 0) / (neck.length + 1), (head.tilt || 0) + (head.lift || 0));
      // tail swish, ear flicks
      tails.forEach((tb, i) => {
        const k = (i + 1) / tails.length;
        tb.rotation.y = (S.tail.swish ?? 0.25) * k * (Math.sin(tau * 2.1 - i * 0.7) * (0.4 + st.idle * 0.6) + twitch(tau, 0.13, 0.3, 0.1) * 1.5);
        tb.rotation.z = (S.tail.lift || 0) * w * Math.sin(TAU * 2 * s - i * 0.5) * k + (S.tail.curl ? S.tail.curl(st, i) : 0);
      });
      ears.forEach(({ bone, s: sd }) => {
        const f = twitch(tau, 0.19, sd > 0 ? 0.1 : 0.55, 0.05);
        bone.rotation.set(sd * f * 0.5, 0, -f * 0.35 + Math.sin(tau * 0.5 + sd) * 0.06);
      });
      if (S.anim) S.anim(B, st, R);
    },
  };
}
/** Shared idle head behaviour for grazers: lower the head to graze for a while, lift it, look around. */
function grazeHead(st, amp = 1.1, lift = 0.6, yawAmp = 0.6) {
  const x = fract(st.tau / 17), graze = st.idle * sstep(0.66, 0.72, x) * (1 - sstep(0.9, 0.96, x));
  const chew = graze * Math.sin(st.tau * 9) * 0.03;
  return { down: graze * amp, lift: graze * lift, yaw: (1 - graze) * (Math.sin(st.tau * 0.37) * 0.5 + Math.sin(st.tau * 0.9) * 0.2) * yawAmp * (0.3 + 0.7 * st.idle), tilt: chew };
}
/** Idle head behaviour for predators/browsers: look around, occasionally sniff the ground. */
function lookHead(st, yawAmp = 0.7, sniff = 0.5) {
  const x = fract(st.tau / 13), down = st.idle * sstep(0.7, 0.76, x) * (1 - sstep(0.86, 0.92, x));
  return { down: down * sniff, lift: down * sniff * 0.3, yaw: (Math.sin(st.tau * 0.41) * 0.6 + Math.sin(st.tau * 1.03) * 0.25) * yawAmp * (0.25 + 0.75 * st.idle) * (1 - down), tilt: Math.sin(st.tau * 0.6) * 0.04 };
}


// ================================================================ quadruped species
/** Crest/mane strip along the top of a neck spec. */
function neckCrest(b, neck, o) {
  const pts = neck.map((p) => new THREE.Vector2(p[0], p[1]));
  const st = [];
  const i0 = o.from ?? 0;
  for (let i = i0; i < neck.length; i++) {
    const a = pts[Math.max(0, i - 1)], c = pts[Math.min(neck.length - 1, i + 1)];
    const t = new THREE.Vector2().subVectors(c, a).normalize(), up = new THREE.Vector2(-t.y, t.x);
    const r = neck[i][3] * (o.inset ?? 0.85);
    const k = (i - i0) / (neck.length - 1 - i0);
    st.push({ p: [pts[i].x + up.x * r, pts[i].y + up.y * r, 0], r: [o.w, o.h * (o.prof ? o.prof(k) : 1), o.w], b: i === 0 ? 'chest' : i === neck.length - 1 ? 'head' : 'n' + i });
  }
  b.loft(st, { around: 8, part: 'mane', paint: o.paint, pat: o.pat, cap0: 0.6, cap1: 0.8, disp: o.disp });
}

function zebra() {
  const ink = '#141312';
  const neck = [[0.38, 1.08, 0.16, 0.2, 0.22], [0.58, 1.34, 0.12, 0.14, 0.16], [0.74, 1.55, 0.09, 0.1, 0.11], [0.84, 1.69, 0.08, 0.085, 0.09]];
  const nd = new THREE.Vector2(0.84 - 0.38, 1.69 - 1.08).normalize();
  const S = {
    key: 'zebra',
    hip: [-0.46, 1.0], sh: [0.42, 0.98],
    torso: [
      [-0.76, 1.16, 0.97, 0.11], [-0.68, 1.27, 0.83, 0.2], [-0.52, 1.31, 0.75, 0.25], [-0.3, 1.29, 0.69, 0.27],
      [-0.05, 1.25, 0.65, 0.28, 0.45], [0.2, 1.27, 0.65, 0.27, 0.45], [0.4, 1.33, 0.69, 0.24], [0.56, 1.26, 0.77, 0.2], [0.66, 1.11, 0.87, 0.14],
    ],
    neck,
    head: { ang: -58, st: [[-0.06, 0.07, -0.1, 0.07], [0.04, 0.09, -0.13, 0.085], [0.16, 0.08, -0.1, 0.08], [0.3, 0.06, -0.065, 0.062], [0.42, 0.055, -0.058, 0.055], [0.5, 0.05, -0.06, 0.06], [0.55, 0.035, -0.04, 0.045]], eye: [0.1, 0.045, 0.072, 0.02] },
    legF: { z: 0.15, j: [[0.5, 0.98], [0.4, 0.72], [0.43, 0.38], [0.44, 0.13], [0.48, 0.065]], r: [[0.13, 0.1], [0.08, 0.07], [0.045, 0.042], [0.04, 0.036], [0.038, 0.036]], mid: [[0.12, 0.09], [0.065, 0.055, null, 0.3], [0.032, 0.03], null], top: [0.47, 1.1, 0.12, 0.09], hoof: { h: 0.065, r0: 0.036, r1: 0.046, color: '#1d1b1a', dx: 0.008 }, cflex: 1.4, flex: 1.0, follow: 0.7 },
    legH: { z: 0.15, j: [[-0.48, 1.02], [-0.32, 0.74], [-0.5, 0.44], [-0.46, 0.13], [-0.42, 0.065]], r: [[0.17, 0.12], [0.1, 0.08], [0.05, 0.04], [0.04, 0.036], [0.038, 0.036]], mid: [[0.16, 0.11], [0.07, 0.055, null, 0.35], [0.034, 0.03], null], top: [-0.52, 1.18, 0.16, 0.12], hoof: { h: 0.065, r0: 0.036, r1: 0.046, color: '#1d1b1a', dx: 0.008 }, cflex: 0.4, flex: 1.0, follow: 0.9 },
    tail: { pts: [[-0.74, 1.2, 0.04], [-0.8, 1.08, 0.03], [-0.82, 0.92, 0.024], [-0.82, 0.76, 0.022], [-0.81, 0.64, 0.04], [-0.8, 0.56, 0.03]], swish: 0.5 },
    ears: { at: [0.0, 0.075, 0.05], dir: [-0.2, 1, 0.5], len: 0.17, w: 0.05, th: 0.014, prof: [[0, 0.6], [0.35, 1], [0.75, 0.8], [1, 0.2]] },
    gait: { kind: 'walk', hz: 0.95, stride: 0.55, lift: 0.1, duty: 0.62, bob: 0.012, pitch: 0.015, nod: 0.08 },
    headFn: (st) => grazeHead(st, 1.5, 0.9),
    grazeW: [0.75, 0.2, 0.05],
    paint(q) {
      if (q.part === 'head' && q.hl[0] > 0.4) return mix('#f0eee8', '#2a2523', sstep(0.4, 0.47, q.hl[0]));
      if (q.part === 'tail' && q.u > 0.65) return ink;
      if (q.part === 'ear') return q.u > 0.75 ? ink : q.c < -0.3 ? '#e8e2da' : '#f2f0ea';
      return '#f3f1eb';
    },
    pat(part) {
      const base = { mode: PAT.zebra, ink, prm: 0.48 };
      if (part === 'torso') return { ...base, scale: 8.5, coord: (q) => { const f = sstep(-0.2, -0.5, q.p.x); return lerp(q.p.x, -Math.hypot(q.p.x + 0.12, q.p.y - 0.45) * 1.05 + 0.35, f); } };
      if (part === 'legF' || part === 'legH') return { ...base, scale: 15, coord: (q) => q.p.y, mask: (q) => sstep(0.1, 0.22, q.p.y) };
      if (part === 'neck' || part === 'mane') return { ...base, scale: 9.5, coord: (q) => q.p.x * nd.x + q.p.y * nd.y };
      if (part === 'head') return { ...base, scale: 20, prm: 0.42, coord: (q) => q.hl[0] + Math.abs(q.p.z) * 0.6, mask: (q) => 1 - sstep(0.3, 0.4, q.hl[0]) };
      if (part === 'tail') return { ...base, scale: 18, coord: (q) => q.p.y, mask: (q) => 1 - sstep(0.4, 0.6, q.u) };
      return null;
    },
    extras(b, { paint, pat }) {
      neckCrest(b, neck, { w: 0.02, h: 0.075, paint: () => '#f3f1eb', pat: { mode: PAT.zebra, ink, prm: 0.5, scale: 9.5, coord: (q) => q.p.x * nd.x + q.p.y * nd.y }, prof: (k) => 0.6 + 0.4 * Math.sin(Math.PI * Math.min(1, k * 1.1)) });
    },
  };
  return buildQuad(S);
}

// ---------------------------------------------------------------- carnivores (digitigrade)
/** Digitigrade legs from shoulder/hip joints. t = thickness factor, pw = paw radius, k = canid (slimmer, straighter). */
function digiLegs(sx, sy, hx, hy, t, pw, o = {}) {
  const c = o.canid ? 1 : 0;
  return {
    legF: {
      z: o.zf ?? 0.12, j: [[sx, sy], [sx - (0.11 - 0.03 * c) * sy, 0.6 * sy], [sx - (0.06 - 0.04 * c) * sy, 0.17 * sy], [sx - 0.02 * sy, pw * 1.4], [sx + 0.1 * sy, pw]],
      r: [[0.13 * t, 0.1 * t], [0.075 * t, 0.065 * t], [0.043 * t, 0.044 * t], [pw * 1.15, pw * 1.5], [pw, pw * 1.4]],
      mid: [[0.12 * t, 0.09 * t], [0.064 * t, 0.055 * t, null, 0.3], [0.038 * t, 0.042 * t], null],
      top: [sx + 0.02, sy + 0.14 * t, 0.12 * t, 0.095 * t], toeOff: 0.7, flex: 1.0, cflex: 0.9, follow: 0.6,
    },
    legH: {
      z: o.zh ?? 0.12, j: [[hx, hy], [hx + 0.2 * hy, 0.66 * hy], [hx - (0.07 - 0.02 * c) * hy, 0.3 * hy], [hx - 0.01 * hy, pw * 1.4], [hx + 0.1 * hy, pw]],
      r: [[0.17 * t, 0.12 * t], [0.095 * t, 0.075 * t], [0.04 * t, 0.04 * t], [pw * 1.15, pw * 1.45], [pw, pw * 1.35]],
      mid: [[0.16 * t, 0.11 * t], [0.074 * t, 0.058 * t, null, 0.35], [0.036 * t, 0.04 * t], null],
      top: [hx - 0.03, hy + 0.15 * t, 0.15 * t, 0.115 * t], toeOff: 0.7, flex: 0.9, cflex: 0.2, follow: 0.85,
    },
  };
}
/** Head stations scaled from a unit template (x along the head, top/bottom/half-width). ml stretches the muzzle. */
function headSt(tpl, s, ml = 1) {
  return tpl.map(([x, top, bot, w, k]) => [x > 0.45 ? 0.45 * s + (x - 0.45) * s * ml : x * s, top * s, bot * s, w * s, k]);
}
const CAT_HEAD = [[-0.22, 0.22, -0.24, 0.26], [0.0, 0.34, -0.31, 0.36], [0.26, 0.31, -0.29, 0.38], [0.5, 0.2, -0.24, 0.26], [0.72, 0.13, -0.22, 0.21], [0.88, 0.09, -0.17, 0.16], [0.96, 0.03, -0.1, 0.09]];
const DOG_HEAD = [[-0.2, 0.2, -0.2, 0.22], [0.0, 0.29, -0.23, 0.27], [0.24, 0.26, -0.21, 0.26], [0.44, 0.15, -0.17, 0.16], [0.68, 0.1, -0.13, 0.11], [0.88, 0.08, -0.1, 0.08], [0.99, 0.04, -0.05, 0.05]];
const pawPaint = (q, fur, pad) => (q.p.y < 0.012 ? pad : fur);

function bigCat(o) {
  const sy = o.sy, hy = o.hy ?? sy * 1.04, sx = o.sx, hx = o.hx;
  const legs = digiLegs(sx, sy, hx, hy, o.t ?? 1, o.pw ?? 0.035, o);
  const hs = o.hs, neck = o.neck;
  const S = {
    key: o.key, hip: [hx + 0.02, hy], sh: [sx - 0.02, sy],
    torso: o.torso, torsoSq: [2.3, 2.0], neck,
    head: { ang: o.ang ?? -14, st: headSt(o.headTpl || CAT_HEAD, hs, o.ml ?? 1), eye: [0.42 * hs, 0.17 * hs, 0.235 * hs, 0.042 * hs, o.iris || '#8a6a1c'], cap1: 0.45 },
    ...legs,
    tail: o.tail,
    ears: o.ears || { at: [0.06 * hs, 0.3 * hs, 0.22 * hs], dir: [-0.25, 1, 0.45], len: 0.17 * hs, w: 0.11 * hs, th: 0.025 * hs, prof: [[0, 0.8], [0.4, 1], [0.8, 0.75], [1, 0.25]], cap: 0.9 },
    gait: { kind: 'walk', hz: o.hz ?? 0.8, stride: o.stride ?? 0.5 * sy, lift: 0.1 * sy, duty: 0.6, bob: 0.008 * sy, pitch: 0.01, nod: 0.05, spine: 0.03 },
    headFn: (st) => lookHead(st, 0.8, 0.45),
    grazeW: [0.4, 0.4, 0.2],
    paint: o.paint, pat: o.pat, extras: o.extras, anim: o.anim,
  };
  return buildQuad(S);
}
/** Cat tail: hangs low then curves up at the tip; `curl` keeps the J-shape alive. */
function catTail(x, y, len, r, o = {}) {
  const pts = [];
  const n = o.n || 7;
  for (let i = 0; i < n; i++) {
    const u = i / (n - 1), a = (o.a0 ?? -35) * D + u * u * (o.bend ?? 120) * D * (o.up ?? 1);
    if (i === 0) pts.push([x, y, r]);
    else { const p = pts[i - 1]; pts.push([p[0] - Math.cos(a) * len / (n - 1), p[1] + Math.sin(a) * len / (n - 1), r * (1 - u * (o.taper ?? 0.35))]); }
  }
  if (o.tuft) { const p = pts[n - 1]; pts.push([p[0] - 0.02, p[1] + 0.03, o.tuft]); }
  return { pts, swish: o.swish ?? 0.35, lift: 0.08, around: o.around || 9, cap: 0.9, curl: (st, i) => (i > 2 ? Math.sin(st.tau * 0.8 + i * 0.4) * 0.08 : 0) };
}
const furCounter = (q, top, bottom, lo = -0.55, hi = 0.15) => mix(bottom, top, sstep(lo, hi, q.n.y));

function lion() {
  const hs = 0.4;
  const neck = [[0.36, 0.86, 0.16, 0.19, 0.22], [0.56, 0.96, 0.15, 0.16, 0.18], [0.72, 1.02, 0.14, 0.14, 0.15]];
  return bigCat({
    key: 'lion', sx: 0.44, sy: 0.8, hx: -0.46, hy: 0.84, hs, neck, ang: -16, t: 1.05, pw: 0.038,
    torso: [[-0.7, 0.96, 0.74, 0.12], [-0.62, 1.03, 0.62, 0.2], [-0.44, 1.04, 0.56, 0.23], [-0.15, 1.0, 0.54, 0.24], [0.15, 1.02, 0.47, 0.25], [0.4, 1.06, 0.46, 0.24], [0.56, 1.0, 0.56, 0.2], [0.66, 0.88, 0.67, 0.13]],
    tail: catTail(-0.68, 0.98, 0.95, 0.035, { tuft: 0.055, a0: -55, bend: 70 }),
    paint(q) {
      if (q.part === 'tail') return q.u > 0.86 ? '#2b1a0f' : furCounter(q, '#c08a4a', '#dcb985');
      if (q.part === 'mane') return mix('#4a2a12', '#a8702f', sstep(0.2, 0.95, q.u) * 0.7 + fbm(q.p.x * 9, q.p.y * 9, q.p.z * 9) * 0.5 - 0.15);
      if (q.part === 'head') {
        const [x, y] = q.hl;
        if (x > 0.35 && y > -0.005 && Math.abs(q.p.z) < 0.05) return '#3a2420';
        if (x > 0.28 && y < -0.02) return '#efe2c6';
        return furCounter(q, '#c99350', '#e6cf9f');
      }
      if (q.part === 'ear') return q.c < -0.2 ? '#e7d2b0' : '#5a3a20';
      if (q.part === 'legF' || q.part === 'legH') return mix('#c08a4a', '#e2c79a', sstep(-0.3, -0.9, q.n.z * Math.sign(q.p.z)) * 0.8 + sstep(0.3, 0.05, q.p.y) * 0.3);
      return furCounter(q, '#c08a4a', '#e4cc9c');
    },
    extras(b, { HF, paint }) {
      // shaggy mane: a lumpy loft over the neck and the back of the head, longer under the throat
      const mp = [[0.3, 0.98, 0.2, 0.18, 0.26, 'chest'], [0.46, 1.02, 0.25, 0.22, 0.32, 'n0'], [0.62, 1.04, 0.28, 0.25, 0.32, 'n1'], [0.76, 1.04, 0.27, 0.26, 0.3, 'head'], [0.84, 1.03, 0.2, 0.2, 0.22, 'head']];
      b.loft(mp.map(([x, y, w, u, d, bn]) => ({ p: [x, y, 0], r: [w, u, d], b: bn })), {
        around: 18, part: 'mane', paint, cap0: 0.6, cap1: 0.35, step: 0.035,
        disp: (q) => (fbm(q.p.x * 14, q.p.y * 14, q.p.z * 14) - 0.5) * 0.09 + (q.c < -0.3 ? 0.02 : 0),
      });
    },
  });
}

// ---------------------------------------------------------------- materials & primitives
const matCache = new Map();
function mat(color, opts = {}) {
  const key = color + JSON.stringify(opts, (k, v) => (v && v.isTexture ? v.uuid : v));
  if (!matCache.has(key)) {
    matCache.set(key, new THREE.MeshStandardMaterial({ color, roughness: 0.85, metalness: 0.02, flatShading: true, ...opts }));
  }
  return matCache.get(key);
}
const vcMat = (opts = {}) => mat('#ffffff', { vertexColors: true, ...opts });

/** Ellipsoid. `paint` may be a colour, a material, or fn(x,y,z on unit sphere) -> colour (vertex-coloured). */
function ellipsoid(rx, ry, rz, paint, segs = 14, extra = {}) {
  const g = new THREE.SphereGeometry(1, segs, Math.max(6, Math.round(segs * 0.7)), extra.phiStart, extra.phiLength, extra.thetaStart, extra.thetaLength);
  let m;
  if (typeof paint === 'function') {
    const pos = g.attributes.position, col = new Float32Array(pos.count * 3), c = new THREE.Color();
    for (let i = 0; i < pos.count; i++) {
      c.set(paint(pos.getX(i), pos.getY(i), pos.getZ(i)));
      col.set([c.r, c.g, c.b], i * 3);
    }
    g.setAttribute('color', new THREE.BufferAttribute(col, 3));
    m = vcMat(extra.map ? { map: extra.map } : {});
  } else m = paint && paint.isMaterial ? paint : mat(paint, extra.map ? { map: extra.map } : {});
  g.scale(rx, ry, rz);
  const mesh = new THREE.Mesh(g, m);
  return mesh;
}
/** Cylinder hanging down from its pivot at the origin (length along -Y). */
function limb(len, rTop, rBot, color, segs = 7) {
  const g = new THREE.CylinderGeometry(rTop, rBot, len, segs, 1);
  g.translate(0, -len / 2, 0);
  return new THREE.Mesh(g, color.isMaterial ? color : mat(color));
}
/** Cylinder pointing up from the origin. */
function stalk(len, rBot, rTop, color, segs = 7) {
  const g = new THREE.CylinderGeometry(rTop, rBot, len, segs, 1);
  g.translate(0, len / 2, 0);
  return new THREE.Mesh(g, color.isMaterial ? color : mat(color));
}
function cone(r, h, color, segs = 6) {
  const g = new THREE.ConeGeometry(r, h, segs);
  g.translate(0, h / 2, 0);
  return new THREE.Mesh(g, mat(color));
}
const grp = (...kids) => { const g = new THREE.Group(); kids.forEach(k => k && g.add(k)); return g; };
const at = (obj, x = 0, y = 0, z = 0) => { obj.position.set(x, y, z); return obj; };
const rot = (obj, x = 0, y = 0, z = 0) => { obj.rotation.set(x, y, z); return obj; };
function eyes(parent, x, y, z, r = 0.035, color = '#0b0b0b') {
  const g = new THREE.SphereGeometry(r, 8, 6);
  [1, -1].forEach(s => { const e = new THREE.Mesh(g, mat(color, { roughness: 0.3, flatShading: false })); e.position.set(x, y, z * s); parent.add(e); });
}

// ---------------------------------------------------------------- pattern textures
const texCache = new Map();
function patternTex(type, bg, fg, seed = 1) {
  const key = [type, bg, fg, seed].join('|');
  if (texCache.has(key)) return texCache.get(key);
  const W = 512, H = 256, cv = document.createElement('canvas');
  cv.width = W; cv.height = H;
  const x = cv.getContext('2d');
  let s = seed * 9301 + 49297;
  const rnd = () => ((s = (s * 9301 + 49297) % 233280) / 233280);
  x.fillStyle = bg; x.fillRect(0, 0, W, H);
  x.fillStyle = fg; x.strokeStyle = fg;
  if (type === 'stripes' || type === 'rear-stripes' || type === 'tiger') {
    const n = type === 'tiger' ? 26 : type === 'rear-stripes' ? 34 : 40;
    for (let i = 0; i < n; i++) {
      const u = (i + 0.5) / n;
      if (type === 'rear-stripes' && u > 0.22 && u < 0.78) continue;
      const cx = u * W, w = (type === 'tiger' ? 5 : 7) + rnd() * 5;
      x.beginPath();
      x.moveTo(cx - w, type === 'tiger' ? H * 0.08 : 0);
      for (let y = 0; y <= H; y += 16) x.lineTo(cx + Math.sin(y / 30 + i) * 6 - w / 2, y);
      for (let y = H; y >= 0; y -= 16) x.lineTo(cx + Math.sin(y / 30 + i) * 6 + w / 2, y * (type === 'tiger' ? 0.75 : 1));
      x.closePath(); x.fill();
    }
  } else if (type === 'spots') {
    for (let i = 0; i < 520; i++) { x.beginPath(); x.arc(rnd() * W, rnd() * H, 2.5 + rnd() * 3, 0, 7); x.fill(); }
  } else if (type === 'rosettes') {
    x.lineWidth = 3.2;
    for (let i = 0; i < 210; i++) {
      const cx = rnd() * W, cy = rnd() * H, r = 5 + rnd() * 5;
      for (let k = 0; k < 4; k++) { x.beginPath(); x.arc(cx, cy, r, k * 1.57 + 0.25, k * 1.57 + 1.2); x.stroke(); }
    }
  } else if (type === 'patches') {
    for (let gy = 0; gy < H; gy += 26) for (let gx = 0; gx < W; gx += 26) {
      const w = 18 + rnd() * 4, h = 18 + rnd() * 4, ox = gx + rnd() * 5, oy = gy + rnd() * 5;
      x.beginPath();
      x.moveTo(ox + 4, oy); x.lineTo(ox + w, oy + rnd() * 3); x.lineTo(ox + w - rnd() * 3, oy + h); x.lineTo(ox + rnd() * 3, oy + h - 2); x.closePath(); x.fill();
    }
  } else if (type === 'scutes') {
    x.lineWidth = 3;
    for (let gy = 0; gy < H; gy += 40) for (let gx = (gy / 40) % 2 ? 0 : 22; gx < W; gx += 44) {
      x.beginPath();
      for (let k = 0; k < 6; k++) x.lineTo(gx + Math.cos(k * 1.047) * 20, gy + Math.sin(k * 1.047) * 18);
      x.closePath(); x.stroke();
    }
  } else if (type === 'scales') {
    x.globalAlpha = 0.55;
    for (let i = 0; i < 900; i++) { x.beginPath(); x.arc(rnd() * W, rnd() * H, 2 + rnd() * 2, 0, 7); x.fill(); }
  } else if (type === 'bands') {
    for (let i = 0; i < 22; i++) { const cx = (i + 0.5) / 22 * W; x.fillRect(cx - 4, 0, 8, H); }
  }
  const t = new THREE.CanvasTexture(cv);
  t.colorSpace = THREE.SRGBColorSpace;
  t.wrapS = THREE.RepeatWrapping;
  texCache.set(key, t);
  return t;
}
const bellyPaint = (top, belly, split = -0.25) => (x, y) => (y < split ? belly : top);

// ---------------------------------------------------------------- quadruped
function quadruped(p) {
  const root = new THREE.Group(), torso = new THREE.Group();
  root.add(torso);
  const L = p.bodyLen, H = p.bodyH, W = p.bodyW, legLen = p.legLen;
  const bodyY = legLen + H * 0.5;
  const legColor = p.legColor || p.color;
  const map = p.pattern ? patternTex(p.pattern.type, p.pattern.bg || p.color, p.pattern.fg, p.pattern.seed) : null;
  const body = ellipsoid(L, H, W, p.belly ? bellyPaint(map ? '#ffffff' : p.color, p.belly, -0.35) : (map ? '#ffffff' : p.color), 16, { map });
  at(body, 0, bodyY, 0);
  torso.add(body);
  if (p.chest) torso.add(at(ellipsoid(L * 0.55, H * 1.02, W * 1.02, p.chest, 14), L * 0.38, bodyY + H * 0.03, 0));
  if (p.band) torso.add(at(ellipsoid(L * 0.28, H * 1.04, W * 1.05, p.band, 14), L * 0.2, bodyY, 0));
  if (p.saddle) torso.add(at(ellipsoid(L * 0.55, H * 0.55, W * 0.9, p.saddle, 12), -L * 0.15, bodyY + H * 0.5, 0));
  if (p.hump) torso.add(at(ellipsoid(p.hump.rx, p.hump.ry, p.hump.rz || W * 0.7, p.hump.color || p.color, 12), p.hump.x * L, bodyY + H * 0.7, 0));
  if (p.skirt) {
    for (let i = 0; i < 9; i++) {
      const u = -0.8 + i * 0.2;
      [1, -1].forEach(s => torso.add(at(ellipsoid(L * 0.16, H * 0.55, W * 0.18, p.skirt, 8), u * L, bodyY - H * 0.55, s * W * 0.82)));
    }
  }

  // legs
  const legs = [];
  const hipX = L * (p.hipX || 0.6), hipZ = W * 0.55;
  [[hipX, hipZ, 0], [hipX, -hipZ, Math.PI], [-hipX, hipZ, Math.PI], [-hipX, -hipZ, 0]].forEach(([x, z, ph], i) => {
    const front = i < 2;
    const thighLen = legLen * 0.52 + H * 0.25, shinLen = legLen * 0.52;
    const lr = p.legR * (front && p.frontLegScale ? p.frontLegScale : 1);
    const hip = at(new THREE.Group(), x, bodyY - H * 0.25, z);
    const thigh = limb(thighLen, lr * 1.35, lr, legColor);
    hip.add(thigh);
    const knee = at(new THREE.Group(), 0, -thighLen, 0);
    knee.add(limb(shinLen, lr, lr * (p.paw ? 1 : 0.85), p.lowerLegColor || legColor));
    if (p.hoof) knee.add(at(limb(lr * 0.9, lr * 0.95, lr * 1.05, p.hoof), 0, -shinLen + lr * 0.4, 0));
    if (p.paw) knee.add(at(ellipsoid(lr * 1.5, lr * 0.7, lr * 1.2, p.lowerLegColor || legColor, 8), lr * 0.4, -shinLen, 0));
    hip.add(knee);
    torso.add(hip);
    legs.push({ hip, knee, ph, front });
  });

  // neck + head
  const neckPivot = at(new THREE.Group(), L * 0.78, bodyY + H * (p.neckBase ?? 0.3), 0);
  const alpha = (90 - p.neckAngle) * D;
  neckPivot.rotation.z = -alpha;
  torso.add(neckPivot);
  const nl = p.neckLen;
  if (nl > 0) {
    const neckMat = p.neckPattern && map ? mat('#ffffff', { map }) : (p.neckColor || p.color);
    neckPivot.add(stalk(nl, p.neckR * 1.25, p.neckR, neckMat, 9));
  }
  const head = at(new THREE.Group(), 0, nl, 0);
  head.rotation.z = alpha - (p.headTilt || 15) * D;
  neckPivot.add(head);
  const [hx, hy, hz] = p.head;
  head.add(ellipsoid(hx, hy, hz, p.headColor || p.color, 12));
  if (p.snout) {
    const [sx, sy, sz] = p.snout;
    head.add(at(ellipsoid(sx, sy, sz, p.snoutColor || p.headColor || p.color, 10), hx * 0.75, -hy * 0.2, 0));
    head.add(at(ellipsoid(sz * 0.45, sy * 0.4, sz * 0.6, p.nose || '#1a1a1a', 8), hx * 0.75 + sx * 0.92, -hy * 0.1, 0));
  }
  eyes(head, hx * 0.45, hy * 0.35, hz * 0.78, Math.max(0.025, hy * 0.13));
  if (p.eyePatch) [1, -1].forEach(s => head.add(at(ellipsoid(hx * 0.22, hy * 0.28, hz * 0.12, p.eyePatch, 8), hx * 0.42, hy * 0.25, s * hz * 0.82)));
  if (p.mask) [1, -1].forEach(s => head.add(at(ellipsoid(hx * 0.5, hy * 0.12, hz * 0.06, p.mask, 6), hx * 0.6, 0, s * hz * 0.82)));

  // ears
  const ears = [];
  if (p.ears) {
    const e = p.ears;
    [1, -1].forEach(s => {
      const ep = at(new THREE.Group(), -hx * 0.25, hy * 0.7, s * hz * 0.55);
      let ear;
      if (e.shape === 'pointy') ear = rot(cone(e.size * 0.45, e.size, e.color || p.headColor || p.color, 4), 0, 0, s * 0 + 0.15);
      else if (e.shape === 'round') ear = at(ellipsoid(e.size * 0.35, e.size * 0.5, e.size * 0.5, e.color || p.headColor || p.color, 8), 0, e.size * 0.25, 0);
      else if (e.shape === 'elephant') {
        ep.position.set(-hx * 0.45, hy * 0.15, s * hz * 0.8);
        ear = at(ellipsoid(e.size * 0.55, e.size, e.size * 0.08, e.color || p.color, 12), -e.size * 0.35, -e.size * 0.3, s * e.size * 0.05);
      } else if (e.shape === 'long') ear = rot(at(ellipsoid(e.size * 0.18, e.size * 0.6, e.size * 0.1, e.color || p.color, 8), 0, e.size * 0.5, 0), s * 0.35, 0, -0.2);
      if (e.shape === 'pointy') ep.rotation.x = s * -0.35;
      ep.add(ear);
      head.add(ep);
      ears.push({ ep, s });
    });
  }

  // horns & antlers
  if (p.horns === 'rhino') {
    head.add(rot(at(cone(hy * 0.32, hy * 1.5, '#9a8f7e', 6), hx * 1.4, hy * 0.05, 0), 0, 0, -0.35));
    head.add(rot(at(cone(hy * 0.25, hy * 0.7, '#9a8f7e', 6), hx * 0.95, hy * 0.35, 0), 0, 0, -0.2));
  } else if (p.horns === 'bison') {
    [1, -1].forEach(s => head.add(rot(at(cone(hy * 0.13, hy * 0.6, '#2b2723', 6), -hx * 0.1, hy * 0.55, s * hz * 0.8), s * -1.0, 0, 0)));
  } else if (p.horns === 'ossicones') {
    [1, -1].forEach(s => { const o = at(stalk(hy * 0.6, hy * 0.12, hy * 0.1, '#6b4b2a'), -hx * 0.3, hy * 0.75, s * hz * 0.35); o.add(at(ellipsoid(hy * 0.16, hy * 0.16, hy * 0.16, '#2a2018', 6), 0, hy * 0.6, 0)); head.add(o); });
  } else if (p.horns === 'moose') {
    [1, -1].forEach(s => {
      const a = at(new THREE.Group(), -hx * 0.2, hy * 0.75, s * hz * 0.5);
      a.add(at(rot(stalk(hy * 0.4, hy * 0.08, hy * 0.08, '#c9b48a'), s * -0.9, 0, 0), 0, 0, 0));
      const palm = at(ellipsoid(hy * 0.55, hy * 0.12, hy * 0.85, '#d2bf95', 8), 0, hy * 0.45, s * hy * 0.75);
      palm.rotation.x = s * 0.35;
      a.add(palm);
      for (let k = 0; k < 4; k++) a.add(rot(at(cone(hy * 0.06, hy * 0.35, '#d2bf95', 4), -hy * 0.3 + k * hy * 0.22, hy * 0.6, s * hy * (1.1 + k * 0.1)), s * -0.5, 0, 0));
      head.add(a);
    });
  }
  if (p.mane) {
    head.add(at(ellipsoid(hx * 0.9, hy * 1.65, hz * 1.7, p.mane, 12), -hx * 0.55, -hy * 0.05, 0));
    neckPivot.add(at(ellipsoid(p.neckR * 1.9, nl * 0.75 + 0.05, p.neckR * 1.9, p.mane, 10), 0, nl * 0.45, 0));
  }
  if (p.beard) head.add(at(ellipsoid(hx * 0.4, hy * 0.6, hz * 0.6, p.beard, 8), hx * 0.2, -hy * 0.9, 0));

  // trunk & tusks
  const trunkSegs = [];
  if (p.trunk) {
    let parent = head;
    const n = 9, segLen = p.trunk.len / n;
    const base = at(new THREE.Group(), hx * 0.95, -hy * 0.15, 0);
    base.rotation.z = -0.25;
    head.add(base);
    parent = base;
    for (let i = 0; i < n; i++) {
      const r = p.trunk.r * (1 - i / n * 0.55);
      const seg = new THREE.Group();
      seg.add(limb(segLen * 1.12, r, r * 0.92, p.headColor || p.color));
      if (i > 0) seg.position.y = -segLen;
      parent.add(seg);
      trunkSegs.push(seg);
      parent = seg;
    }
  }
  if (p.tusks) {
    [1, -1].forEach(s => {
      const R = p.tusks.len, g = new THREE.TorusGeometry(R, p.tusks.r, 6, 14, p.tusks.arc || 1.4);
      const t = new THREE.Mesh(g, mat('#f3ecd8'));
      t.position.set(hx * 0.9 - R * 0.05, -hy * 0.45 - R, s * hz * 0.45);
      t.rotation.set(s * 0.25, 0, Math.PI * 0.5 + 0.15);
      t.rotation.order = 'YXZ';
      t.rotation.y = s * 0.2;
      head.add(t);
    });
  }

  // tail
  let tail = null;
  if (p.tail) {
    const t = p.tail;
    tail = at(new THREE.Group(), -L * 0.95, bodyY + H * 0.3, 0);
    tail.rotation.z = (t.angle ?? 35) * D;
    const tMat = t.pattern && map ? mat('#ffffff', { map }) : (t.color || p.color);
    if (t.bushy) tail.add(at(ellipsoid(t.r * 2.2, t.len * 0.55, t.r * 2.2, tMat, 10), 0, -t.len * 0.5, 0));
    else tail.add(limb(t.len, t.r, t.r * 0.6, tMat));
    if (t.tuft) tail.add(at(ellipsoid(t.r * 2, t.r * 3.2, t.r * 2, t.tuft, 8), 0, -t.len, 0));
    if (t.tip) tail.add(at(ellipsoid(t.r * 2.3, t.len * 0.15, t.r * 2.3, t.tip, 8), 0, -t.len * 0.95, 0));
    torso.add(tail);
  }

  const speed = p.speed || 5, stride = (p.stride || 24) * D;
  return {
    root,
    update(t) {
      const ph = t * speed;
      legs.forEach(l => {
        const a = Math.sin(ph + l.ph);
        l.hip.rotation.z = stride * a;
        l.knee.rotation.z = (l.front ? -1 : 1) * Math.max(0, Math.cos(ph + l.ph)) * stride * 1.4 * (l.front ? 1 : -1);
      });
      torso.position.y = Math.abs(Math.sin(ph)) * p.legLen * 0.04;
      neckPivot.rotation.z = -alpha + Math.sin(ph * 2) * 0.03 + Math.sin(t * 0.7) * 0.05;
      head.rotation.y = Math.sin(t * 0.5) * 0.12;
      if (tail) tail.rotation.x = Math.sin(t * 3) * 0.3;
      ears.forEach(e => {
        if (p.ears.shape === 'elephant') e.ep.rotation.y = e.s * (0.25 + Math.sin(t * 1.6) * 0.2);
        else e.ep.rotation.z = Math.sin(t * 2 + e.s) * 0.08;
      });
      trunkSegs.forEach((s, i) => { s.rotation.z = (i ? 1 : 0) * (0.12 + Math.sin(t * 1.4 - i * 0.5) * 0.12); s.rotation.x = Math.sin(t * 0.9 - i * 0.3) * 0.05; });
    },
  };
}

// ---------------------------------------------------------------- deformable tube bodies (fish, whales, snakes)
/**
 * A smooth body swept along a centre-line that can change every frame.
 * center(u, t) -> [x, y, z] for u in [0 (head) .. 1 (tail)], radius(u) -> [up, side],
 * paint(x = 1-2u, up, side) -> colour.  attach(u) returns a group that follows the body frame
 * (local +X = forward, +Y = dorsal, +Z = side).
 */
function tubeBody({ rings = 44, around = 14, center, radius, paint, map }) {
  const verts = rings * around + 2;
  const pos = new Float32Array(verts * 3), col = new Float32Array(verts * 3), uv = new Float32Array(verts * 2);
  const idx = [], c = new THREE.Color();
  for (let i = 0; i < rings; i++) {
    const u = i / (rings - 1);
    for (let j = 0; j < around; j++) {
      const a = (j / around) * Math.PI * 2, k = i * around + j;
      c.set(paint(1 - 2 * u, Math.cos(a), Math.sin(a)));
      col.set([c.r, c.g, c.b], k * 3);
      uv.set([j / around, u], k * 2);
      if (i < rings - 1) {
        const n = (i + 1) * around, j2 = (j + 1) % around;
        idx.push(k, i * around + j2, n + j, i * around + j2, n + j2, n + j);
      }
    }
  }
  const head = rings * around, tail = head + 1;
  c.set(paint(1, 0, 0)); col.set([c.r, c.g, c.b], head * 3);
  c.set(paint(-1, 0, 0)); col.set([c.r, c.g, c.b], tail * 3);
  for (let j = 0; j < around; j++) {
    const j2 = (j + 1) % around;
    idx.push(head, j2, j);
    idx.push(tail, (rings - 1) * around + j, (rings - 1) * around + j2);
  }
  const g = new THREE.BufferGeometry();
  g.setAttribute('position', new THREE.BufferAttribute(pos, 3));
  g.setAttribute('color', new THREE.BufferAttribute(col, 3));
  g.setAttribute('uv', new THREE.BufferAttribute(uv, 2));
  g.setIndex(idx);
  const mesh = new THREE.Mesh(g, vcMat(map ? { map, side: THREE.DoubleSide } : { side: THREE.DoubleSide }));
  mesh.frustumCulled = false;
  const root = new THREE.Group();
  root.add(mesh);
  const attachments = [];
  const C = [], T = new THREE.Vector3(), N = new THREE.Vector3(), B = new THREE.Vector3(), P = new THREE.Vector3(), m4 = new THREE.Matrix4();
  for (let i = 0; i < rings; i++) C.push(new THREE.Vector3());
  const frameAt = (u, t, out) => {
    const e = 0.004;
    const a = center(Math.max(0, u - e), t), b = center(Math.min(1, u + e), t), p = center(u, t);
    out.p.set(...p);
    out.t.set(a[0] - b[0], a[1] - b[1], a[2] - b[2]).normalize();
  };
  function update(t) {
    const f = { p: P, t: T };
    let prevN = new THREE.Vector3(0, 1, 0);
    for (let i = 0; i < rings; i++) {
      const u = i / (rings - 1);
      frameAt(u, t, f);
      N.copy(prevN).addScaledVector(T, -prevN.dot(T)).normalize();
      if (N.lengthSq() < 1e-6) N.set(0, 1, 0);
      prevN.copy(N);
      B.crossVectors(T, N);
      const [ru, rs] = radius(u);
      for (let j = 0; j < around; j++) {
        const a = (j / around) * Math.PI * 2, ca = Math.cos(a) * ru, sa = Math.sin(a) * rs, k = (i * around + j) * 3;
        pos[k] = P.x + N.x * ca + B.x * sa; pos[k + 1] = P.y + N.y * ca + B.y * sa; pos[k + 2] = P.z + N.z * ca + B.z * sa;
      }
      if (i === 0) pos.set([P.x + T.x * 0.002, P.y, P.z], head * 3);
      if (i === rings - 1) pos.set([P.x - T.x * 0.002, P.y, P.z], tail * 3);
    }
    g.attributes.position.needsUpdate = true;
    g.computeBoundingSphere();
    attachments.forEach(at => {
      frameAt(at.u, t, f);
      const n = new THREE.Vector3(0, 1, 0).addScaledVector(T, -T.y).normalize();
      const b = new THREE.Vector3().crossVectors(T, n);
      m4.makeBasis(T, n, b);
      at.g.position.copy(P);
      at.g.quaternion.setFromRotationMatrix(m4);
    });
  }
  return {
    root,
    update,
    attach(u) { const gr = new THREE.Group(); root.add(gr); attachments.push({ g: gr, u }); return gr; },
  };
}
function finMesh(shape, color) {
  return new THREE.Mesh(new THREE.ExtrudeGeometry(shape, { depth: 0.014, bevelEnabled: false }), mat(color, { side: THREE.DoubleSide }));
}
function dorsal(g, h, len, color, back = 0.5) {
  const s = new THREE.Shape();
  s.moveTo(len * 0.5, 0); s.quadraticCurveTo(len * 0.4, h * 0.7, len * (0.5 - back), h); s.quadraticCurveTo(0, h * 0.35, -len * 0.5, 0); s.lineTo(len * 0.5, 0);
  const m = finMesh(s, color);
  m.position.z = -0.007;
  g.add(m);
}
function pectoral(g, len, width, color, y, z, down = 0.5, back = 0.6) {
  [1, -1].forEach(s => {
    const pv = at(new THREE.Group(), 0, y, s * z);
    pv.add(at(ellipsoid(width * 0.5, 0.018, len * 0.5, color, 8), -width * 0.1, 0, s * len * 0.45));
    pv.rotation.set(s * down, s * back, 0);
    g.add(pv);
  });
}
function tailFin(g, F, color, vertical) {
  const s = new THREE.Shape();
  s.moveTo(0.02, 0); s.quadraticCurveTo(-F * 0.25, F * 0.45, -F * 0.6, F * 1.0); s.quadraticCurveTo(-F * 0.35, F * 0.25, -F * 0.42, 0);
  s.quadraticCurveTo(-F * 0.35, -F * 0.25, -F * 0.6, -F * (vertical ? 0.75 : 1.0)); s.quadraticCurveTo(-F * 0.25, -F * 0.45, 0.02, 0);
  const m = finMesh(s, color);
  if (vertical) m.position.z = -0.007; else { m.rotation.x = Math.PI / 2; m.position.y = 0.007; }
  g.add(m);
}
/** Fish / whale / dolphin. lateral = side-to-side swimming (sharks); otherwise up-and-down (cetaceans). */
function swimmer(p) {
  const L = p.length, H = p.height || 0.45, lateral = !!p.vertical, speed = p.speed || 2.5, amp = p.amp || 0.12;
  let time = 0;
  const wave = (u, t) => amp * L * Math.pow(u, 1.6) * Math.sin(t * speed - u * 5);
  const body = tubeBody({
    center: (u, t) => { const d = wave(u, t); return [L * (0.5 - u), H + (lateral ? 0 : d), lateral ? d : 0]; },
    radius: p.profile,
    paint: p.paint || ((x, y) => (y < (p.split ?? -0.15) ? (p.belly || p.color) : p.color)),
  });
  const eye = body.attach(0.07);
  const [r0u, r0s] = p.profile(0.07);
  eyes(eye, 0, r0u * 0.15, r0s * 0.9, Math.max(0.012, r0u * 0.12));
  tailFin(body.attach(0.985), p.finSize || 0.3, p.finColor || p.color, lateral);
  if (p.decorate) p.decorate((u) => body.attach(u), p.profile);
  return { root: body.root, update: (t) => { time = t; body.update(t); } };
}
// ---------------------------------------------------------------- birds
function flyingBird(p) {
  const root = new THREE.Group(), body = at(new THREE.Group(), 0, 0.6, 0);
  root.add(body);
  body.add(ellipsoid(0.42, 0.16, 0.16, p.color, 12));
  const head = at(new THREE.Group(), 0.42, 0.06, 0);
  head.add(ellipsoid(0.13, 0.11, 0.1, p.head || p.color, 10));
  head.add(rot(at(cone(0.045, 0.14, p.beak || '#f5c518', 5), 0.12, -0.02, 0), 0, 0, -Math.PI / 2 - 0.3));
  eyes(head, 0.06, 0.04, 0.08, 0.018, '#2a1a00');
  body.add(head);
  const tail = at(ellipsoid(0.2, 0.025, 0.12, p.tail || p.color, 8), -0.52, 0.02, 0);
  body.add(tail);
  const wings = [];
  [1, -1].forEach(s => {
    const shoulder = at(new THREE.Group(), 0.06, 0.06, s * 0.12);
    const inner = at(ellipsoid(0.17, 0.025, 0.3, p.wing || p.color, 8), 0, 0, s * 0.28);
    shoulder.add(inner);
    const elbow = at(new THREE.Group(), 0, 0, s * 0.55);
    elbow.add(at(ellipsoid(0.15, 0.02, 0.32, p.wingTip || p.wing || p.color, 8), -0.04, 0, s * 0.28));
    for (let k = 0; k < 5; k++) elbow.add(rot(at(ellipsoid(0.03, 0.01, 0.12, p.wingTip || p.wing || p.color, 5), -0.12 + k * 0.05, 0, s * (0.56 + k * 0.012)), 0, s * (0.3 - k * 0.15), 0));
    shoulder.add(elbow);
    body.add(shoulder);
    wings.push({ shoulder, elbow, s });
  });
  [1, -1].forEach(s => body.add(rot(at(limb(0.14, 0.02, 0.015, p.feet || '#f5c518'), -0.25, -0.1, s * 0.06), 0, 0, -1.3)));
  return {
    root,
    update(t) {
      const f = Math.sin(t * (p.flap || 6));
      wings.forEach(w => { w.shoulder.rotation.x = w.s * f * 0.55; w.elbow.rotation.x = w.s * Math.sin(t * (p.flap || 6) - 0.8) * 0.4; });
      body.position.y = 0.6 - f * 0.04;
      body.rotation.x = Math.sin(t * 0.6) * 0.15;
      tail.rotation.z = Math.sin(t * 1.3) * 0.05;
    },
  };
}
function walkingBird(p) {
  const root = new THREE.Group(), body = new THREE.Group();
  root.add(body);
  const legLen = p.legLen, bodyY = legLen + p.body[1] * 0.4;
  body.add(at(ellipsoid(...p.body, p.color, 14), 0, bodyY, 0));
  if (p.plume) [1, -1].forEach(s => body.add(at(ellipsoid(p.body[0] * 0.55, p.body[1] * 0.4, 0.05, p.plume, 8), -p.body[0] * 0.45, bodyY + p.body[1] * 0.2, s * p.body[2] * 0.9)));
  if (p.tuft) body.add(rot(at(ellipsoid(p.body[0] * 0.3, p.body[1] * 0.35, p.body[2] * 0.5, p.tuft, 8), -p.body[0] * 0.95, bodyY + p.body[1] * 0.5, 0), 0, 0, 0.7));
  if (p.wing) [1, -1].forEach(s => body.add(at(ellipsoid(p.body[0] * 0.45, p.body[1] * 0.4, 0.05, p.wing, 8), -p.body[0] * 0.05, bodyY + p.body[1] * 0.15, s * p.body[2] * 0.93)));
  const neck = at(new THREE.Group(), p.body[0] * 0.75, bodyY + p.body[1] * 0.3, 0);
  neck.rotation.z = -(90 - p.neckAngle) * D;
  neck.add(stalk(p.neckLen, p.neckR * 1.3, p.neckR, p.neckColor || p.color, 8));
  body.add(neck);
  const head = at(new THREE.Group(), 0, p.neckLen, 0);
  head.rotation.z = (90 - p.neckAngle) * D;
  head.add(ellipsoid(p.headR * 1.1, p.headR, p.headR * 0.9, p.headColor || p.neckColor || p.color, 10));
  const beak = p.beakShape === 'hook'
    ? grp(at(ellipsoid(p.headR * 1.0, p.headR * 0.55, p.headR * 0.5, p.beak, 8), p.headR * 1.4, -p.headR * 0.15, 0),
        rot(at(cone(p.headR * 0.35, p.headR * 0.6, p.beak, 5), p.headR * 2.2, -p.headR * 0.15, 0), 0, 0, -2.4))
    : rot(at(cone(p.headR * 0.35, p.headR * 1.6, p.beak, 5), p.headR * 0.8, -p.headR * 0.1, 0), 0, 0, -Math.PI / 2);
  head.add(beak);
  eyes(head, p.headR * 0.4, p.headR * 0.3, p.headR * 0.75, p.headR * 0.18);
  neck.add(head);
  const legs = [];
  [1, -1].forEach((s, i) => {
    const hip = at(new THREE.Group(), 0, bodyY - p.body[1] * 0.4, s * p.body[2] * 0.4);
    hip.add(limb(legLen * 0.5 + p.body[1] * 0.4, p.legR * 1.8, p.legR, p.thigh || p.legColor));
    const knee = at(new THREE.Group(), 0, -(legLen * 0.5 + p.body[1] * 0.4), 0);
    knee.add(limb(legLen * 0.52, p.legR, p.legR * 0.9, p.legColor));
    knee.add(at(ellipsoid(p.legR * 3, p.legR * 0.8, p.legR * 2, p.legColor, 6), p.legR * 1.5, -legLen * 0.52, 0));
    hip.add(knee);
    body.add(hip);
    legs.push({ hip, knee, ph: i * Math.PI });
  });
  const sp = p.speed || 5;
  return {
    root,
    update(t) {
      legs.forEach(l => { l.hip.rotation.z = Math.sin(t * sp + l.ph) * 0.45; l.knee.rotation.z = -Math.max(0, Math.cos(t * sp + l.ph)) * 0.7; });
      body.position.y = Math.abs(Math.cos(t * sp)) * legLen * 0.03;
      body.rotation.x = Math.sin(t * sp) * (p.waddle || 0.03);
      neck.rotation.z = -(90 - p.neckAngle) * D + Math.sin(t * sp * 2) * 0.06;
    },
  };
}
function penguin() {
  const root = new THREE.Group(), body = new THREE.Group();
  root.add(body);
  const paint = (x, y, z) => (x > 0.35 && y < 0.55 ? (y > 0.25 ? '#f6e8b0' : '#f4f4f0') : '#14161c');
  body.add(at(ellipsoid(0.27, 0.62, 0.25, paint, 16), 0, 0.68, 0));
  const head = at(new THREE.Group(), 0.02, 1.28, 0);
  head.add(ellipsoid(0.17, 0.16, 0.16, '#14161c', 12));
  [1, -1].forEach(s => head.add(at(ellipsoid(0.07, 0.09, 0.03, '#f5b82e', 8), -0.03, -0.06, s * 0.14)));
  head.add(rot(at(cone(0.035, 0.22, '#1e1e1e', 5), 0.13, -0.03, 0), 0, 0, -Math.PI / 2 - 0.25));
  head.add(rot(at(cone(0.02, 0.12, '#f08a24', 4), 0.15, -0.045, 0.012), 0, 0, -Math.PI / 2 - 0.25));
  eyes(head, 0.08, 0.04, 0.12, 0.02);
  body.add(head);
  const flips = [1, -1].map(s => { const f = at(new THREE.Group(), 0, 1.0, s * 0.24); f.add(at(ellipsoid(0.08, 0.32, 0.03, '#1b1e26', 8), 0, -0.3, 0)); body.add(f); return { f, s }; });
  const feet = [1, -1].map(s => { const ft = at(ellipsoid(0.1, 0.03, 0.06, '#2a2a2a', 6), 0.08, 0.03, s * 0.11); root.add(ft); return ft; });
  return {
    root,
    update(t) {
      body.rotation.x = Math.sin(t * 3.5) * 0.08;
      body.position.y = Math.abs(Math.sin(t * 3.5)) * 0.02;
      flips.forEach(({ f, s }) => (f.rotation.x = s * (0.25 + Math.sin(t * 3.5) * 0.12)));
      feet.forEach((ft, i) => (ft.position.y = 0.03 + Math.max(0, Math.sin(t * 3.5 + i * Math.PI)) * 0.03));
      head.rotation.y = Math.sin(t * 0.6) * 0.3;
    },
  };
}

// ---------------------------------------------------------------- reptiles
function lizard(p) {
  const root = new THREE.Group(), torso = new THREE.Group();
  root.add(torso);
  const by = p.legLen * 0.9 + p.bodyH;
  const map = patternTex(p.pattern || 'scales', p.color, p.dark || '#1f2a14', 3);
  const paint = bellyPaint('#ffffff', p.belly || '#d9d2a6', -0.45);
  torso.add(at(ellipsoid(p.bodyLen, p.bodyH, p.bodyW, paint, 14, { map }), 0, by, 0));
  // head
  const head = at(new THREE.Group(), p.bodyLen * 0.95, by + p.bodyH * 0.1, 0);
  head.add(at(ellipsoid(p.headLen * 0.45, p.bodyH * 0.55, p.bodyW * 0.55, paint, 10, { map }), p.headLen * 0.2, 0, 0));
  head.add(at(ellipsoid(p.headLen * 0.55, p.bodyH * 0.3, p.bodyW * (p.snoutW || 0.35), paint, 10, { map }), p.headLen * 0.75, -p.bodyH * 0.12, 0));
  eyes(head, p.headLen * 0.38, p.bodyH * 0.4, p.bodyW * 0.32, 0.03, '#c9b03a');
  let tongue = null;
  if (p.tongue) { tongue = rot(at(cone(0.012, 0.2, '#e0567a', 3), p.headLen * 1.25, -p.bodyH * 0.15, 0), 0, 0, -Math.PI / 2); head.add(tongue); }
  if (p.teeth) for (let k = 0; k < 6; k++) [1, -1].forEach(s => head.add(rot(at(cone(0.012, 0.035, '#f2f0e0', 3), p.headLen * (0.5 + k * 0.12), -p.bodyH * 0.32, s * p.bodyW * 0.3), Math.PI, 0, 0)));
  torso.add(head);
  // tail chain
  const tail = [];
  let parent = at(new THREE.Group(), -p.bodyLen * 0.9, by, 0);
  torso.add(parent);
  const n = 8, segLen = p.tailLen / n;
  for (let i = 0; i < n; i++) {
    const r = p.bodyH * 0.75 * (1 - i / n * 0.85);
    const seg = new THREE.Group();
    if (i) seg.position.x = -segLen;
    seg.add(at(ellipsoid(segLen * 0.7, r, r * (p.bodyW / p.bodyH) * 0.9, paint, 8, { map }), -segLen * 0.5, 0, 0));
    if (p.scutes) [1, -1].forEach(s => seg.add(at(cone(r * 0.18, r * 0.35, p.dark || '#2b3320', 4), -segLen * 0.5, r * 0.85, s * r * 0.25)));
    parent.add(seg);
    tail.push(seg);
    parent = seg;
  }
  if (p.scutes) for (let k = 0; k < 10; k++) [1, -1].forEach(s => torso.add(at(cone(0.02, 0.05, p.dark || '#2b3320', 4), -p.bodyLen * 0.9 + k * p.bodyLen * 0.2, by + p.bodyH * 0.9, s * p.bodyW * 0.25)));
  // sprawled legs
  const legs = [];
  [[0.6, 1, 0], [0.6, -1, Math.PI], [-0.6, 1, Math.PI], [-0.6, -1, 0]].forEach(([fx, s, ph]) => {
    const hip = at(new THREE.Group(), fx * p.bodyLen, by - p.bodyH * 0.2, s * p.bodyW * 0.8);
    const upper = rot(limb(p.legLen * 0.75, p.legR * 1.2, p.legR, p.color), s * -1.1, 0, 0);
    hip.add(upper);
    const knee = at(new THREE.Group(), 0, -Math.cos(1.1) * p.legLen * 0.75, s * Math.sin(1.1) * p.legLen * 0.75);
    knee.add(limb(p.legLen * 0.75, p.legR, p.legR * 0.8, p.color));
    knee.add(at(ellipsoid(p.legR * 1.8, p.legR * 0.5, p.legR * 1.5, p.color, 6), p.legR, -p.legLen * 0.75, 0));
    hip.add(knee);
    torso.add(hip);
    legs.push({ hip, ph });
  });
  const sp = p.speed || 3;
  return {
    root,
    update(t) {
      legs.forEach(l => (l.hip.rotation.y = Math.sin(t * sp + l.ph) * 0.45));
      torso.rotation.y = Math.sin(t * sp) * 0.08;
      tail.forEach((s, i) => (s.rotation.y = Math.sin(t * sp - i * 0.5) * 0.12));
      head.rotation.y = -Math.sin(t * sp) * 0.1;
      if (tongue) tongue.scale.set(1, Math.max(0.01, Math.sin(t * 5) > 0.6 ? 1 : 0.01), 1);
    },
  };
}
function cobra() {
  const map = patternTex('bands', '#5e5b2c', '#2c2a14', 1);
  const top = 0.5, R = 0.16, g0 = 0.06;
  const center = (u, t) => {
    if (u < 0.08) { const s = 1 - u / 0.08; return [0.05 + s * 0.14, top + Math.sin(t * 1.3) * 0.01 * s, Math.sin(t * 0.9) * 0.02 * s]; }
    if (u < 0.3) { const s = (u - 0.08) / 0.22; return [0.05, top - s * (top - g0 - R), 0]; }
    if (u < 0.42) { const a = (u - 0.3) / 0.12 * Math.PI / 2; return [0.05 - R + R * Math.cos(a), g0 + R - R * Math.sin(a), 0]; }
    const s = (u - 0.42) / 0.58;
    return [0.05 - R - s * 1.4, g0, Math.sin(s * 9 - t * 2.2) * 0.16 * Math.min(1, s * 3)];
  };
  const radius = (u) => {
    const r = 0.055 * (u < 0.04 ? 0.75 : 1) * (1 - Math.max(0, u - 0.6) * 1.9);
    const hood = u > 0.08 && u < 0.26 ? 1 + Math.sin((u - 0.08) / 0.18 * Math.PI) * 2.2 : 1;
    return [r * (hood > 1 ? 0.75 : 1), r * hood];
  };
  const body = tubeBody({ rings: 60, around: 12, center, radius, map, paint: (x, y) => (y < -0.4 ? '#e8dca0' : '#ffffff') });
  const h = body.attach(0.0);
  eyes(h, -0.03, 0.03, 0.035, 0.012);
  const tongue = rot(at(cone(0.006, 0.09, '#d0485e', 3), 0.02, -0.01, 0), 0, 0, -Math.PI / 2);
  h.add(tongue);
  return { root: body.root, update(t) { body.update(t); tongue.visible = Math.sin(t * 6) > 0.4; } };
}
function turtle() {
  const root = new THREE.Group(), body = at(new THREE.Group(), 0, 0.35, 0);
  root.add(body);
  const shellMap = patternTex('scutes', '#6b5a32', '#2e2410', 2);
  body.add(ellipsoid(0.55, 0.25, 0.45, mat('#ffffff', { map: shellMap }), 16, { thetaLength: Math.PI / 2 }));
  body.add(at(ellipsoid(0.52, 0.06, 0.42, '#d9cf9a', 12), 0, -0.01, 0));
  const head = at(new THREE.Group(), 0.58, 0.02, 0);
  head.add(ellipsoid(0.16, 0.09, 0.1, '#6e7d4a', 10));
  eyes(head, 0.08, 0.04, 0.07, 0.018);
  body.add(head);
  const flips = [];
  [[0.25, 1, 0.42, 0.11], [0.25, -1, 0.42, 0.11], [-0.38, 1, 0.2, 0.07], [-0.38, -1, 0.2, 0.07]].forEach(([x, s, len, w]) => {
    const pv = at(new THREE.Group(), x, 0, s * 0.35);
    pv.add(at(ellipsoid(w, 0.025, len * 0.5, '#7b8a55', 8), 0, 0, s * len * 0.45));
    body.add(pv);
    flips.push({ pv, s, front: x > 0 });
  });
  return {
    root,
    update(t) {
      flips.forEach(f => { f.pv.rotation.x = f.s * Math.sin(t * 1.8 + (f.front ? 0 : 1)) * (f.front ? 0.5 : 0.25); f.pv.rotation.y = f.s * Math.cos(t * 1.8) * 0.3; });
      body.position.y = 0.35 + Math.sin(t * 1.8) * 0.03;
      head.rotation.y = Math.sin(t * 0.7) * 0.2;
    },
  };
}

// ---------------------------------------------------------------- special mammals
function ape(p) {
  const root = new THREE.Group(), torso = new THREE.Group();
  root.add(torso);
  const hipY = p.legLen, shY = hipY + 0.55;
  const body = at(ellipsoid(0.5, 0.33, 0.36, p.color, 14), 0.05, hipY + 0.28, 0);
  body.rotation.z = 0.55;
  torso.add(body);
  if (p.back) { const b = at(ellipsoid(0.34, 0.2, 0.3, p.back, 12), -0.08, hipY + 0.36, 0); b.rotation.z = 0.5; torso.add(b); }
  if (p.belly) torso.add(rot(at(ellipsoid(0.36, 0.22, 0.26, p.belly, 10), 0.12, hipY + 0.16, 0), 0, 0, 0.55));
  const head = at(new THREE.Group(), 0.58, shY + 0.12, 0);
  head.add(ellipsoid(0.17, 0.18, 0.17, p.color, 12));
  if (p.crest) head.add(at(ellipsoid(0.13, 0.12, 0.09, p.color, 8), -0.06, 0.15, 0));
  head.add(at(ellipsoid(0.1, 0.11, 0.13, p.face, 10), 0.11, -0.03, 0));
  head.add(at(ellipsoid(0.05, 0.03, 0.12, p.face, 6), 0.14, 0.07, 0));
  if (p.cheeks) [1, -1].forEach(s => head.add(at(ellipsoid(0.04, 0.15, 0.1, p.face, 8), 0.06, -0.01, s * 0.18)));
  eyes(head, 0.15, 0.03, 0.055, 0.02);
  torso.add(head);
  const limbs = [];
  const mk = (x, y, z, len, r, ph, arm) => {
    const pv = at(new THREE.Group(), x, y, z);
    pv.add(limb(len * 0.52, r * 1.25, r, p.color));
    const el = at(new THREE.Group(), 0, -len * 0.52, 0);
    el.add(limb(len * 0.5, r, r * 0.85, p.color));
    el.add(at(ellipsoid(r * 1.3, r * 0.7, r * 1.2, arm ? p.face : p.color, 6), arm ? 0 : r * 0.6, -len * 0.5, 0));
    pv.add(el);
    torso.add(pv);
    limbs.push({ pv, el, ph, arm });
  };
  mk(0.4, shY, 0.3, shY, p.armR || 0.08, 0, true);
  mk(0.4, shY, -0.3, shY, p.armR || 0.08, Math.PI, true);
  mk(-0.3, hipY + 0.05, 0.2, hipY + 0.05, 0.085, Math.PI, false);
  mk(-0.3, hipY + 0.05, -0.2, hipY + 0.05, 0.085, 0, false);
  return {
    root,
    update(t) {
      limbs.forEach(l => { l.pv.rotation.z = Math.sin(t * 2.4 + l.ph) * 0.28; l.el.rotation.z = (l.arm ? 0.6 : -1) * Math.max(0, Math.cos(t * 2.4 + l.ph)) * 0.35; });
      torso.position.y = Math.abs(Math.sin(t * 2.4)) * 0.02;
      head.rotation.y = Math.sin(t * 0.5) * 0.25;
    },
  };
}
function kangaroo() {
  const root = new THREE.Group(), all = new THREE.Group();
  root.add(all);
  const C = '#b5683a', L = '#e3c3a0';
  const body = at(ellipsoid(0.3, 0.48, 0.27, bellyPaint(C, L, -0.1), 14), 0, 0.85, 0);
  body.rotation.z = -0.55;
  all.add(body);
  const head = at(new THREE.Group(), 0.42, 1.38, 0);
  head.add(ellipsoid(0.15, 0.13, 0.12, C, 10));
  head.add(at(ellipsoid(0.13, 0.08, 0.08, C, 8), 0.15, -0.04, 0));
  head.add(at(ellipsoid(0.03, 0.03, 0.04, '#1b1b1b', 6), 0.27, -0.02, 0));
  eyes(head, 0.08, 0.05, 0.09, 0.02);
  [1, -1].forEach(s => head.add(rot(at(ellipsoid(0.04, 0.14, 0.07, C, 8), -0.05, 0.18, s * 0.07), s * 0.25, 0, -0.2)));
  all.add(head);
  [1, -1].forEach(s => { const a = at(new THREE.Group(), 0.33, 1.05, s * 0.15); a.add(rot(limb(0.3, 0.04, 0.03, C), 0, 0, 0.6)); all.add(a); });
  const legs = [1, -1].map(s => {
    const hip = at(new THREE.Group(), -0.1, 0.62, s * 0.2);
    hip.add(at(ellipsoid(0.25, 0.14, 0.12, C, 10), 0.08, -0.1, 0));
    const knee = at(new THREE.Group(), 0.2, -0.2, 0);
    knee.add(rot(limb(0.42, 0.06, 0.04, C), 0, 0, -0.9));
    const foot = at(new THREE.Group(), -0.33, -0.27, 0);
    foot.add(at(ellipsoid(0.24, 0.035, 0.06, '#7a4425', 8), 0.18, -0.03, 0));
    knee.add(foot);
    hip.add(knee);
    all.add(hip);
    return { hip, knee, foot };
  });
  const tail = at(new THREE.Group(), -0.32, 0.55, 0);
  tail.add(rot(at(ellipsoid(0.6, 0.07, 0.07, C, 10), -0.55, 0, 0), 0, 0, 0.45));
  all.add(tail);
  return {
    root,
    update(t) {
      const ph = (t * 1.6) % 1, air = Math.sin(ph * Math.PI);
      all.position.y = air * 0.35;
      all.rotation.z = -air * 0.12;
      legs.forEach(l => { l.knee.rotation.z = air * 0.5; l.foot.rotation.z = -air * 0.6; });
      tail.rotation.z = air * 0.3;
    },
  };
}
function koala() {
  const root = new THREE.Group(), G = '#8e8e8e', W = '#d8d8d6';
  const branch = rot(at(stalk(2.2, 0.09, 0.07, '#6b4f33', 8), 0.12, -0.2, 0), 0, 0, -0.08);
  root.add(branch);
  const body = at(new THREE.Group(), -0.12, 0.65, 0);
  body.add(ellipsoid(0.28, 0.38, 0.27, bellyPaint(G, W, -0.3), 14));
  root.add(body);
  const head = at(new THREE.Group(), -0.05, 1.12, 0);
  head.add(ellipsoid(0.24, 0.2, 0.25, G, 12));
  head.add(at(ellipsoid(0.07, 0.1, 0.07, '#222', 8), 0.22, -0.02, 0));
  eyes(head, 0.18, 0.06, 0.1, 0.022);
  [1, -1].forEach(s => { head.add(at(ellipsoid(0.06, 0.13, 0.13, G, 10), -0.03, 0.14, s * 0.24)); head.add(at(ellipsoid(0.05, 0.1, 0.1, W, 8), 0.01, 0.14, s * 0.25)); });
  root.add(head);
  [1, -1].forEach(s => { const a = at(new THREE.Group(), 0.0, 0.85, s * 0.2); a.add(rot(limb(0.32, 0.06, 0.05, G), s * 0.4, 0, 1.3)); root.add(a); });
  [1, -1].forEach(s => { const a = at(new THREE.Group(), -0.05, 0.42, s * 0.2); a.add(rot(limb(0.28, 0.07, 0.05, G), s * 0.3, 0, 1.4)); root.add(a); });
  return {
    root,
    update(t) {
      const b = 1 + Math.sin(t * 1.5) * 0.02;
      body.scale.set(b, b, b);
      head.rotation.y = Math.sin(t * 0.4) * 0.6;
      head.rotation.z = Math.sin(t * 0.3) * 0.08;
    },
  };
}
function butterfly() {
  const root = new THREE.Group(), body = at(new THREE.Group(), 0, 0.5, 0);
  root.add(body);
  body.add(ellipsoid(0.28, 0.035, 0.035, '#1b1b1b', 8));
  body.add(at(ellipsoid(0.05, 0.05, 0.05, '#1b1b1b', 8), 0.28, 0.01, 0));
  [1, -1].forEach(s => body.add(rot(at(stalk(0.22, 0.004, 0.004, '#1b1b1b', 4), 0.3, 0.03, s * 0.02), s * 0.3, 0, -0.6)));
  const cv = document.createElement('canvas'); cv.width = cv.height = 256;
  const x = cv.getContext('2d');
  x.fillStyle = '#121212'; x.fillRect(0, 0, 256, 256);
  x.fillStyle = '#f28c18';
  const cells = [[40, 40, 70, 60], [120, 30, 80, 70], [60, 120, 70, 60], [140, 115, 70, 70], [70, 190, 60, 40], [150, 195, 50, 35]];
  cells.forEach(([a, b, w, h]) => { x.beginPath(); x.ellipse(a + w / 2, b + h / 2, w / 2, h / 2, 0.3, 0, 7); x.fill(); });
  x.fillStyle = '#fff';
  for (let i = 0; i < 26; i++) { const a = i / 26 * Math.PI * 1.6 - 0.3; x.beginPath(); x.arc(128 + Math.cos(a) * 112, 128 + Math.sin(a) * 112, 3.5, 0, 7); x.fill(); }
  const tex = new THREE.CanvasTexture(cv);
  tex.colorSpace = THREE.SRGBColorSpace;
  const wm = new THREE.MeshStandardMaterial({ map: tex, side: THREE.DoubleSide, roughness: 0.7 });
  const wings = [1, -1].map(s => {
    const pv = new THREE.Group();
    const fore = new THREE.Mesh(new THREE.CircleGeometry(0.42, 18), wm);
    fore.scale.set(0.75, 1, 1); fore.rotation.x = -Math.PI / 2; fore.position.set(0.06, 0, s * 0.36);
    const hind = new THREE.Mesh(new THREE.CircleGeometry(0.3, 16), wm);
    hind.scale.set(0.9, 1, 1); hind.rotation.x = -Math.PI / 2; hind.position.set(-0.2, -0.005, s * 0.25);
    pv.add(fore, hind);
    body.add(pv);
    return { pv, s };
  });
  return {
    root,
    update(t) {
      const f = Math.sin(t * 9);
      wings.forEach(w => (w.pv.rotation.x = w.s * (0.25 + f * 0.85)));
      body.position.y = 0.5 + Math.sin(t * 2) * 0.08;
      body.rotation.z = Math.sin(t * 1.2) * 0.15;
    },
  };
}


// ---------------------------------------------------------------- species catalogue
const Q = (o) => () => quadruped(o);
const PROC = {
  'african-elephant': Q({ color: '#8a8580', bodyLen: 1.05, bodyH: 0.62, bodyW: 0.58, legLen: 0.95, legR: 0.17, hoof: '#5d5853', neckLen: 0.1, neckR: 0.35, neckAngle: 25, head: [0.38, 0.42, 0.36], headTilt: 10, ears: { shape: 'elephant', size: 0.62 }, trunk: { len: 1.2, r: 0.11 }, tusks: { len: 0.32, r: 0.04 }, tail: { len: 0.55, r: 0.03, angle: 15, tuft: '#333' }, speed: 3, stride: 18 }),
  'woolly-mammoth': Q({ color: '#6b4426', bodyLen: 1.05, bodyH: 0.7, bodyW: 0.6, legLen: 0.92, legR: 0.18, hoof: '#3b2716', neckLen: 0.1, neckR: 0.35, neckAngle: 30, head: [0.36, 0.5, 0.34], headTilt: 5, hump: { rx: 0.5, ry: 0.4, x: 0.35, color: '#5a371d' }, skirt: '#4b2c15', ears: { shape: 'round', size: 0.2 }, trunk: { len: 1.15, r: 0.1 }, tusks: { len: 0.6, r: 0.05, arc: 2.6 }, tail: { len: 0.3, r: 0.03, angle: 15, tuft: '#2a170a' }, speed: 3, stride: 18 }),
  lion: () => lion(),
  tiger: Q({ color: '#e07b28', belly: '#f4ece0', pattern: { type: 'tiger', bg: '#e07b28', fg: '#1a120c' }, bodyLen: 0.85, bodyH: 0.32, bodyW: 0.28, legLen: 0.52, legR: 0.08, paw: true, neckLen: 0.2, neckR: 0.16, neckAngle: 30, head: [0.22, 0.2, 0.2], headColor: '#e07b28', snout: [0.12, 0.1, 0.13], snoutColor: '#f4ece0', nose: '#d77a7a', ears: { shape: 'round', size: 0.09 }, tail: { len: 0.75, r: 0.035, angle: 45, tip: '#1a120c' }, speed: 4.5 }),
  giraffe: Q({ color: '#d9a75c', pattern: { type: 'patches', bg: '#f1dfb8', fg: '#a8642b' }, neckPattern: true, bodyLen: 0.68, bodyH: 0.42, bodyW: 0.3, legLen: 1.6, legR: 0.06, hoof: '#3a2a1a', lowerLegColor: '#efe1c3', neckLen: 1.65, neckR: 0.11, neckAngle: 62, neckBase: 0.4, head: [0.22, 0.13, 0.12], headColor: '#c99449', snout: [0.13, 0.08, 0.08], snoutColor: '#b3824a', ears: { shape: 'pointy', size: 0.12 }, horns: 'ossicones', tail: { len: 0.7, r: 0.025, angle: 15, tuft: '#1e1408' }, speed: 3.2, stride: 20 }),
  zebra: () => zebra(),
  cheetah: Q({ color: '#e1b25d', belly: '#f4e6c4', pattern: { type: 'spots', bg: '#e1b25d', fg: '#1c140c' }, bodyLen: 0.75, bodyH: 0.24, bodyW: 0.2, legLen: 0.62, legR: 0.05, paw: true, neckLen: 0.22, neckR: 0.11, neckAngle: 30, head: [0.16, 0.14, 0.14], snout: [0.08, 0.07, 0.08], nose: '#2a1a12', mask: '#1b120c', ears: { shape: 'round', size: 0.07 }, tail: { len: 0.75, r: 0.025, angle: 60, pattern: true, tip: '#f4f0e8' }, speed: 7, stride: 32 }),
  jaguar: Q({ color: '#d79a3e', belly: '#f2e2c0', pattern: { type: 'rosettes', bg: '#d79a3e', fg: '#20140a' }, bodyLen: 0.75, bodyH: 0.3, bodyW: 0.27, legLen: 0.42, legR: 0.08, paw: true, neckLen: 0.18, neckR: 0.16, neckAngle: 28, head: [0.21, 0.19, 0.19], snout: [0.11, 0.1, 0.12], snoutColor: '#e9d0a2', nose: '#3a2418', ears: { shape: 'round', size: 0.08 }, tail: { len: 0.55, r: 0.035, angle: 40, pattern: true }, speed: 4.5 }),
  'snow-leopard': Q({ color: '#cfcac0', belly: '#efece6', pattern: { type: 'rosettes', bg: '#cfcac0', fg: '#3a3a3a' }, bodyLen: 0.68, bodyH: 0.27, bodyW: 0.24, legLen: 0.42, legR: 0.075, paw: true, neckLen: 0.16, neckR: 0.14, neckAngle: 28, head: [0.18, 0.17, 0.17], snout: [0.09, 0.08, 0.1], nose: '#c98b8b', ears: { shape: 'round', size: 0.07 }, tail: { len: 0.95, r: 0.07, angle: 70, bushy: true, pattern: true }, speed: 4.2 }),
  gorilla: () => ape({ color: '#232323', back: '#8d8d8d', face: '#151515', legLen: 0.42, crest: true, armR: 0.1 }),
  orangutan: () => ape({ color: '#a8461c', face: '#5a3a2a', legLen: 0.36, cheeks: true, armR: 0.07 }),
  'giant-panda': Q({ color: '#f2f0ea', legColor: '#141414', band: '#141414', bodyLen: 0.62, bodyH: 0.4, bodyW: 0.38, legLen: 0.32, legR: 0.11, paw: true, neckLen: 0.12, neckR: 0.22, neckAngle: 30, head: [0.27, 0.25, 0.27], snout: [0.12, 0.1, 0.12], snoutColor: '#f2f0ea', nose: '#141414', eyePatch: '#141414', ears: { shape: 'round', size: 0.12, color: '#141414' }, tail: { len: 0.12, r: 0.05, angle: 40 }, speed: 3, stride: 20 }),
  'polar-bear': Q({ color: '#f1ede2', bodyLen: 0.85, bodyH: 0.4, bodyW: 0.36, legLen: 0.42, legR: 0.12, paw: true, neckLen: 0.35, neckR: 0.18, neckAngle: 22, head: [0.25, 0.17, 0.17], snout: [0.14, 0.1, 0.1], nose: '#111', ears: { shape: 'round', size: 0.07 }, tail: { len: 0.1, r: 0.04, angle: 40 }, speed: 3.2, stride: 20 }),
  'brown-bear': Q({ color: '#6b4428', bodyLen: 0.82, bodyH: 0.42, bodyW: 0.38, legLen: 0.42, legR: 0.12, paw: true, hump: { rx: 0.3, ry: 0.22, x: 0.4 }, neckLen: 0.18, neckR: 0.2, neckAngle: 20, head: [0.25, 0.21, 0.21], snout: [0.13, 0.1, 0.1], snoutColor: '#8a6142', nose: '#111', ears: { shape: 'round', size: 0.09 }, tail: { len: 0.1, r: 0.04, angle: 40 }, speed: 3.2, stride: 20 }),
  'gray-wolf': Q({ color: '#8d8a83', belly: '#d8d4cc', saddle: '#5b5751', bodyLen: 0.62, bodyH: 0.24, bodyW: 0.2, legLen: 0.52, legR: 0.05, paw: true, neckLen: 0.24, neckR: 0.12, neckAngle: 35, head: [0.17, 0.14, 0.13], snout: [0.15, 0.07, 0.07], snoutColor: '#c9c4bb', nose: '#111', ears: { shape: 'pointy', size: 0.12 }, tail: { len: 0.45, r: 0.06, angle: 35, bushy: true, tip: '#333' }, speed: 5.5 }),
  thylacine: Q({ color: '#b98c55', belly: '#dcc39b', pattern: { type: 'rear-stripes', bg: '#b98c55', fg: '#3a2414' }, bodyLen: 0.65, bodyH: 0.22, bodyW: 0.18, legLen: 0.42, legR: 0.05, paw: true, neckLen: 0.2, neckR: 0.1, neckAngle: 30, head: [0.17, 0.12, 0.11], snout: [0.16, 0.07, 0.07], snoutColor: '#c9a479', nose: '#1a1a1a', ears: { shape: 'round', size: 0.08 }, tail: { len: 0.6, r: 0.05, angle: 55 }, speed: 4.5 }),
  'american-bison': Q({ color: '#5a3c22', chest: '#3a2412', bodyLen: 0.85, bodyH: 0.45, bodyW: 0.36, legLen: 0.5, legR: 0.08, hoof: '#1a1a1a', lowerLegColor: '#3a2412', hump: { rx: 0.45, ry: 0.35, x: 0.45, color: '#3e2714' }, neckLen: 0.12, neckR: 0.26, neckAngle: 0, neckBase: 0.05, head: [0.26, 0.24, 0.22], headTilt: 25, headColor: '#2d1c0e', beard: '#2a1a0c', snout: [0.1, 0.11, 0.12], snoutColor: '#2d1c0e', horns: 'bison', ears: { shape: 'round', size: 0.06 }, tail: { len: 0.38, r: 0.025, angle: 15, tuft: '#1a1008' }, speed: 3.5 }),
  hippopotamus: Q({ color: '#7d6a6a', belly: '#c9a2a0', bodyLen: 0.95, bodyH: 0.5, bodyW: 0.48, legLen: 0.3, legR: 0.13, hoof: '#4a3c3c', neckLen: 0.1, neckR: 0.32, neckAngle: 10, head: [0.32, 0.25, 0.28], headTilt: 5, snout: [0.24, 0.2, 0.28], snoutColor: '#8c7272', nose: '#5a4040', ears: { shape: 'round', size: 0.06 }, tail: { len: 0.18, r: 0.03, angle: 30 }, speed: 2.8, stride: 18 }),
  'white-rhino': Q({ color: '#9b9690', bodyLen: 0.95, bodyH: 0.46, bodyW: 0.42, legLen: 0.42, legR: 0.13, hoof: '#5a5650', hump: { rx: 0.3, ry: 0.22, x: 0.45 }, neckLen: 0.12, neckR: 0.28, neckAngle: 0, neckBase: 0.1, head: [0.38, 0.22, 0.2], headTilt: 30, snout: [0.14, 0.13, 0.17], ears: { shape: 'pointy', size: 0.14 }, horns: 'rhino', tail: { len: 0.3, r: 0.025, angle: 15, tuft: '#333' }, speed: 3, stride: 18 }),
  dromedary: Q({ color: '#c69c6d', bodyLen: 0.7, bodyH: 0.38, bodyW: 0.3, legLen: 1.05, legR: 0.06, hoof: '#8a6a48', hump: { rx: 0.38, ry: 0.42, x: -0.05 }, neckLen: 0.75, neckR: 0.1, neckAngle: 30, head: [0.24, 0.13, 0.12], headTilt: -25, snout: [0.12, 0.1, 0.09], snoutColor: '#b38a5f', ears: { shape: 'round', size: 0.05 }, tail: { len: 0.45, r: 0.025, angle: 15, tuft: '#5a4028' }, speed: 3.2, stride: 22 }),
  moose: Q({ color: '#3e2b1e', lowerLegColor: '#9a8a78', bodyLen: 0.85, bodyH: 0.42, bodyW: 0.32, legLen: 1.05, legR: 0.07, hoof: '#1a1a1a', hump: { rx: 0.3, ry: 0.25, x: 0.5 }, neckLen: 0.35, neckR: 0.17, neckAngle: 30, head: [0.36, 0.17, 0.15], headTilt: 25, snout: [0.14, 0.13, 0.13], snoutColor: '#4b3424', beard: '#2a1c12', ears: { shape: 'long', size: 0.24 }, horns: 'moose', tail: { len: 0.12, r: 0.03, angle: 30 }, speed: 3.2, stride: 22 }),
  'emperor-penguin': () => penguin(),
  'bald-eagle': () => flyingBird({ color: '#4a3020', head: '#f8f8f4', tail: '#f8f8f4', wing: '#3c2617', wingTip: '#2a1a10', beak: '#f2b81c', feet: '#f2b81c', flap: 3.2 }),
  ostrich: () => walkingBird({ color: '#1d1b1b', body: [0.5, 0.36, 0.35], plume: '#f2efe8', tuft: '#f2efe8', legLen: 1.05, legR: 0.045, legColor: '#d59b8e', thigh: '#e3b3a6', neckLen: 0.95, neckR: 0.05, neckAngle: 78, neckColor: '#d9a89b', headR: 0.09, headColor: '#c99a8e', beak: '#d8b48a', speed: 6 }),
  dodo: () => walkingBird({ color: '#8b8b86', body: [0.42, 0.38, 0.36], tuft: '#f0ede4', wing: '#6e6e6a', legLen: 0.32, legR: 0.045, legColor: '#e6c444', neckLen: 0.3, neckR: 0.1, neckAngle: 60, neckColor: '#8b8b86', headR: 0.14, headColor: '#9a9a96', beak: '#c9c26a', beakShape: 'hook', speed: 4.5, waddle: 0.08 }),
  'blue-whale': () => swimmer({ length: 3.2, color: '#5d7894', belly: '#a9bccd', profile: u => { const v = 0.3 * Math.sin(Math.PI * Math.min(1, 0.1 + u * 0.92)) + 0.025; return [v, v * 1.08]; }, finSize: 0.5, speed: 1.4, amp: 0.07, height: 0.45, decorate: (A, pr) => { dorsal(A(0.74), 0.07, 0.16, '#5d7894', 0.6); pectoral(A(0.24), 0.4, 0.12, '#5d7894', -pr(0.24)[0] * 0.55, pr(0.24)[1] * 0.75); } }),
  'humpback-whale': () => swimmer({ length: 2.6, color: '#2f3238', belly: '#e8e8e8', split: -0.45, profile: u => { const v = 0.34 * Math.sin(Math.PI * Math.min(1, 0.14 + u * 0.9)) + 0.025; return [v, v * 1.08]; }, finSize: 0.55, finColor: '#2f3238', speed: 1.4, amp: 0.08, height: 0.5, decorate: (A, pr) => { dorsal(A(0.7), 0.07, 0.16, '#2f3238', 0.6); pectoral(A(0.26), 0.95, 0.16, '#e4e4e4', -pr(0.26)[0] * 0.5, pr(0.26)[1] * 0.75, 0.6, 0.8); } }),
  orca: () => swimmer({ length: 2.0, color: '#121318', profile: u => { const v = 0.26 * Math.sin(Math.PI * Math.min(1, 0.13 + u * 0.9)) + 0.025; return [v, v * 1.08]; }, paint: (x, y, z) => (y < -0.35 || (x > 0.55 && x < 0.8 && y > 0.05 && y < 0.6 && Math.abs(z) > 0.55) ? '#f4f4f4' : '#121318'), finSize: 0.38, finColor: '#121318', speed: 2, amp: 0.09, height: 0.45, decorate: (A, pr) => { dorsal(A(0.42), 0.48, 0.24, '#121318', 0.35); pectoral(A(0.24), 0.32, 0.17, '#121318', -pr(0.24)[0] * 0.6, pr(0.24)[1] * 0.7); } }),
  'bottlenose-dolphin': () => swimmer({ length: 1.6, color: '#7f8b97', belly: '#dfe4e8', profile: u => { const v = u < 0.1 ? 0.035 + u * 0.6 : 0.19 * Math.sin(Math.PI * Math.min(1, 0.1 + u * 0.92)) + 0.02; return [v, v]; }, finSize: 0.3, speed: 2.6, amp: 0.1, height: 0.4, decorate: (A, pr) => { dorsal(A(0.46), 0.2, 0.22, '#6f7a86', 0.75); pectoral(A(0.27), 0.22, 0.08, '#6f7a86', -pr(0.27)[0] * 0.6, pr(0.27)[1] * 0.7); } }),
  'great-white-shark': () => swimmer({ length: 2.0, color: '#6d7681', belly: '#f1f1f1', split: -0.15, vertical: true, profile: u => { const v = 0.25 * Math.sin(Math.PI * Math.min(1, 0.12 + u * 0.9)) + 0.02; return [v, v * 0.85]; }, finSize: 0.46, speed: 2.2, amp: 0.1, height: 0.5, decorate: (A, pr) => { dorsal(A(0.38), 0.36, 0.3, '#5d6670', 0.75); pectoral(A(0.3), 0.48, 0.15, '#5d6670', -pr(0.3)[0] * 0.5, pr(0.3)[1] * 0.7, 0.45, 0.8); const g = A(0.2); for (let k = 0; k < 4; k++) [1, -1].forEach(s => g.add(at(ellipsoid(0.006, 0.07, 0.006, '#3a3f45', 4), -k * 0.04, 0, s * pr(0.2)[1] * 0.98))); } }),
  'green-sea-turtle': () => turtle(),
  'saltwater-crocodile': () => lizard({ color: '#4b5232', dark: '#232816', belly: '#cfc79a', pattern: 'scales', bodyLen: 0.75, bodyH: 0.16, bodyW: 0.28, headLen: 0.75, snoutW: 0.3, tailLen: 1.6, legLen: 0.2, legR: 0.05, scutes: true, teeth: true, speed: 2 }),
  'komodo-dragon': () => lizard({ color: '#6c6450', dark: '#3b3528', belly: '#a89f84', pattern: 'scales', bodyLen: 0.65, bodyH: 0.17, bodyW: 0.25, headLen: 0.4, snoutW: 0.4, tailLen: 1.2, legLen: 0.26, legR: 0.06, tongue: true, speed: 2.4 }),
  'king-cobra': () => cobra(),
  'monarch-butterfly': () => butterfly(),
  'red-kangaroo': () => kangaroo(),
  koala: () => koala(),
};

const GLTF = {
  'wild-horse': { file: 'Horse.glb' },
  flamingo: { file: 'Flamingo.glb', flying: true },
  'scarlet-macaw': { file: 'Parrot.glb', flying: true },
  'white-stork': { file: 'Stork.glb', flying: true },
  'red-fox': { file: 'Fox.glb', clip: 'Walk', skinned: true },
};
export const MARINE = new Set(['blue-whale', 'humpback-whale', 'orca', 'bottlenose-dolphin', 'great-white-shark', 'green-sea-turtle']);
export const FLYING = new Set(['bald-eagle', 'flamingo', 'scarlet-macaw', 'white-stork', 'monarch-butterfly']);

const gltfLoader = new GLTFLoader();
const gltfCache = new Map();
function loadGltf(file) {
  if (!gltfCache.has(file)) gltfCache.set(file, gltfLoader.loadAsync(MODEL_DIR + file));
  return gltfCache.get(file);
}

/** Normalise so the model's longest horizontal size is 1, centred, feet on y = 0, facing +X. */
function normalise(obj) {
  const wrap = new THREE.Group();
  wrap.add(obj);
  obj.updateMatrixWorld(true);
  const box = new THREE.Box3().setFromObject(obj, true), size = box.getSize(new THREE.Vector3()), c = box.getCenter(new THREE.Vector3());
  const s = 1 / Math.max(size.x, size.z, size.y * 0.8);
  obj.position.set(-c.x, -box.min.y, -c.z);
  const outer = new THREE.Group();
  wrap.scale.setScalar(s);
  outer.add(wrap);
  outer.userData.size = size.clone().multiplyScalar(s);
  return outer;
}

/** Build a fresh animated instance of an animal. Returns { object, update(t, dt), dispose() }. */
export async function createAnimal(id) {
  if (GLTF[id]) {
    const spec = GLTF[id];
    const gltf = await loadGltf(spec.file);
    const scene = spec.skinned ? SkeletonUtils.clone(gltf.scene) : gltf.scene.clone(true);
    scene.traverse(o => { if (o.isMesh) { o.frustumCulled = false; if (o.material) o.material.side = THREE.FrontSide; } });
    // These models face +Z; rotate to face +X.
    const inner = new THREE.Group();
    inner.add(scene);
    inner.rotation.y = Math.PI / 2;
    const object = normalise(inner);
    const mixer = new THREE.AnimationMixer(scene);
    const clip = (spec.clip && gltf.animations.find(a => a.name === spec.clip)) || gltf.animations[0];
    if (clip) mixer.clipAction(clip).play();
    return { object, update: (t, dt) => mixer.update(dt), flying: !!spec.flying };
  }
  const factory = PROC[id] || PROC['gray-wolf'];
  const built = factory();
  built.update(0);
  const object = normalise(built.root);
  return { object, update: (t) => built.update(t), flying: FLYING.has(id), marine: MARINE.has(id) };
}

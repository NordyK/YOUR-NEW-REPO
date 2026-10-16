// Animated 3D animal miniatures for the globe: glTF models where available, procedural models otherwise.
// Convention for every model: head points +X, up is +Y, feet rest on y = 0.
//
// Procedural animals are built entirely in code. Each one is a single SkinnedMesh (one shared geometry per
// species, so many instances are cheap): smooth lofted forms (superellipse rings swept along spines and limbs)
// bound to a hand-built bone rig. Coats use vertex colours (countershading, masks, socks, baked occlusion) plus
// resolution-independent shader patterns evaluated on the rest pose (stripes, spots, rosettes, giraffe network,
// scutes, mottling, and signed-distance markings such as an orca's eye patch), so markings stay crisp up close
// and fade cleanly to their average colour when tiny.
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
const PAT = { zebra: 1, tiger: 2, spots: 3, rosettes: 4, openRosettes: 5, network: 6, cells: 7, mottle: 8, mark: 9 };
/** Signed distance to an ellipse (approximate; negative inside) — for crisp PAT.mark markings. */
const sdEll = (x, y, cx, cy, rx, ry) => (Math.hypot((x - cx) / rx, (y - cy) / ry) - 1) * Math.min(rx, ry);
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
  if (mode > 8.5) {                     // 9 crisp marking: ink where the interpolated signed distance is negative
    float d = vPatA.z, aa = max(fwidth(d), 1e-5) * 0.8;
    return mix(base, vPatB.rgb, (1.0 - smoothstep(-aa, aa, d)) * mask);
  }
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
    vec3 w = ahW(p + (vec3(ahN(p * 2.2), ahN(p * 2.2 + 5.0), ahN(p * 2.2 + 9.0)) - 0.5) * 0.35);
    float r = prm * (0.75 + 0.45 * w.z);
    float ring = ahLess(abs(w.x - r), prm * (0.17 + 0.12 * ahN(p * 5.0)) * (0.4 + 0.6 * mask)) * step(0.36, ahN(p * 4.3 + w.z * 9.0));
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
    const WT = st.map((s) => this.W(s.b || o.b || this.bones[0].name));
    const fx = catmull(pts.map((p) => p.x)), fy = catmull(pts.map((p) => p.y)), fz = catmull(pts.map((p) => p.z));
    const rw = mono(R.map((r) => r[0])), ru = mono(R.map((r) => r[1])), rd = mono(R.map((r) => r[2]));
    const around = o.around || 12;
    const eT = 2 / (o.sq ? o.sq[0] : 2), eB = 2 / (o.sq ? (o.sq[1] ?? o.sq[0]) : 2), eS = 2 / (o.sqs || 2);
    let mean = 0;
    R.forEach((r) => (mean += Math.min(r[0], (r[1] + r[2]) / 2)));
    mean /= n;
    const step = o.step || Math.max(1e-3, mean * 0.55);
    // ring parameters along the stations
    const qs = [], two = n === 2 && !o.step;   // tiny two-station parts (teeth, tines, toes): a single segment
    for (let i = 0; i < n - 1; i++) {
      const L = pts[i].distanceTo(pts[i + 1]);
      const k = two ? 1 : Math.max(o.minSeg || 1, Math.round(L / step));
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
      const out = [], m = o.capRings || (two ? 2 : 3), r0 = end.r, e = len * (r0[0] + (r0[1] + r0[2]) / 2) / 2, sgn = end === rings[0] ? -1 : 1;
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
      const wts = o.wfn ? this.W(o.wfn(m.P)) : n === 1 ? WT[0] : blendW(WT[i0], WT[Math.min(n - 1, i0 + 1)], f);
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
    const wts = i === 0 ? { [parent]: spec.top ? 0.45 : 0.2, [name + 0]: spec.top ? 0.55 : 0.8 } : i === n ? { [name + (n - 1)]: 1 } : { [name + (i - 1)]: 0.5, [name + i]: 0.5 };
    st.push({ p: [J[i][0], J[i][1], zz], r: rr(spec.r[i]), b: wts });
    const m = spec.mid && spec.mid[i];
    if (i < n && m) {
      const t = m[3] ?? 0.45;
      st.push({ p: [lerp(J[i][0], J[i + 1][0], t), lerp(J[i][1], J[i + 1][1], t), zz], r: rr(m), b: { [name + i]: 1 } });
    }
  }
  b.loft(st, { ref: [1, 0, 0], around: spec.around || 10, part: o.part, side, paint: o.paint, pat: o.pat, cap0: spec.cap0 ?? 0.6, cap1: spec.cap1 ?? 0.6, step: spec.step });
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
    update(t) { this.pose(t + ofs); },
    /** Pose at absolute animation time `tau` (tau = 0 is a neutral standing pose). */
    pose(tau) {
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
      [-0.05, 1.25, 0.65, 0.28, 0.45], [0.2, 1.27, 0.65, 0.27, 0.45], [0.4, 1.33, 0.69, 0.25], [0.56, 1.26, 0.77, 0.22], [0.66, 1.11, 0.87, 0.16],
    ],
    neck,
    head: { ang: -58, st: [[-0.06, 0.07, -0.1, 0.07], [0.04, 0.09, -0.13, 0.085], [0.16, 0.08, -0.1, 0.08], [0.3, 0.06, -0.065, 0.062], [0.42, 0.055, -0.058, 0.055], [0.5, 0.05, -0.06, 0.06], [0.55, 0.035, -0.04, 0.045]], eye: [0.1, 0.045, 0.072, 0.02] },
    legF: { z: 0.13, j: [[0.5, 0.98], [0.4, 0.72], [0.43, 0.38], [0.44, 0.13], [0.48, 0.065]], r: [[0.11, 0.09], [0.08, 0.07], [0.045, 0.042], [0.04, 0.036], [0.038, 0.036]], mid: [[0.1, 0.085], [0.065, 0.055, null, 0.3], [0.032, 0.03], null], top: [0.47, 1.1, 0.1, 0.085], hoof: { h: 0.065, r0: 0.036, r1: 0.046, color: '#1d1b1a', dx: 0.008 }, cflex: 1.4, flex: 1.0, follow: 0.7 },
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
      const body = (p) => { const f = sstep(-0.2, -0.5, p.x); return lerp(p.x, -Math.hypot(p.x + 0.12, p.y - 0.45) * 1.05 + 0.35, f); };
      if (part === 'torso') return { ...base, scale: 8.5, coord: (q) => body(q.p) };
      if (part === 'legF' || part === 'legH') return { ...base, scale: 8.5, coord: (q) => lerp(q.p.y * 1.75, body(q.p), sstep(0.62, 0.9, q.p.y)), mask: (q) => sstep(0.1, 0.22, q.p.y) };
      if (part === 'neck' || part === 'mane') return { ...base, scale: 9.5, coord: (q) => q.p.x * nd.x + q.p.y * nd.y };
      if (part === 'head') return { ...base, scale: 20, prm: 0.42, coord: (q) => q.hl[0] + Math.abs(q.p.z) * 0.6, mask: (q) => 1 - sstep(0.3, 0.4, q.hl[0]) };
      if (part === 'tail') return { ...base, scale: 11, coord: (q) => q.p.y, mask: (q) => sstep(0.1, 0.25, q.u) * (1 - sstep(0.45, 0.6, q.u)) };
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
      z: o.zf ?? 0.12, j: [[sx, sy], [sx - (0.11 - 0.03 * c) * sy, 0.6 * sy], [sx - (0.06 - 0.04 * c) * sy, 0.17 * sy], [sx - 0.02 * sy, pw * 1.35], [sx + 0.06 * sy, pw]],
      r: [[0.13 * t, 0.1 * t], [0.075 * t, 0.065 * t], [0.043 * t, 0.044 * t], [pw * 1.25, pw * 1.5], [pw * 1.05, pw * 1.45]],
      mid: [[0.12 * t, 0.09 * t], [0.064 * t, 0.055 * t, null, 0.3], [0.038 * t, 0.042 * t], null],
      top: [sx + 0.0, sy + 0.08 * t, 0.11 * t, 0.085 * t], toeOff: 0.7, flex: 1.0, cflex: 0.9, follow: 0.6,
    },
    legH: {
      z: o.zh ?? 0.12, j: [[hx, hy], [hx + 0.2 * hy, 0.66 * hy], [hx - (0.07 - 0.02 * c) * hy, 0.3 * hy], [hx - 0.01 * hy, pw * 1.35], [hx + 0.065 * hy, pw]],
      r: [[0.16 * t, 0.11 * t], [0.095 * t, 0.075 * t], [0.04 * t, 0.04 * t], [pw * 1.2, pw * 1.45], [pw, pw * 1.4]],
      mid: [[0.16 * t, 0.11 * t], [0.074 * t, 0.058 * t, null, 0.35], [0.036 * t, 0.04 * t], null],
      top: [hx - 0.02, hy + 0.08 * t, 0.14 * t, 0.1 * t], toeOff: 0.7, flex: 0.9, cflex: 0.2, follow: 0.85,
    },
  };
}
/** Head stations scaled from a unit template (x along the head, top/bottom/half-width). ml stretches the muzzle. */
function headSt(tpl, s, ml = 1) {
  return tpl.map(([x, top, bot, w, k]) => [x > 0.45 ? 0.45 * s + (x - 0.45) * s * ml : x * s, top * s, bot * s, w * s, k]);
}
const CAT_HEAD = [[-0.2, 0.22, -0.22, 0.26], [0.0, 0.36, -0.3, 0.38], [0.22, 0.38, -0.32, 0.42], [0.42, 0.3, -0.3, 0.36], [0.6, 0.2, -0.27, 0.27], [0.74, 0.14, -0.24, 0.22], [0.83, 0.08, -0.15, 0.15]];
const DOG_HEAD = [[-0.2, 0.2, -0.2, 0.22], [0.0, 0.3, -0.24, 0.28], [0.24, 0.27, -0.23, 0.27], [0.44, 0.16, -0.2, 0.17], [0.66, 0.12, -0.16, 0.125], [0.86, 0.1, -0.12, 0.095], [0.97, 0.05, -0.07, 0.06]];

function bigCat(o) {
  const sy = o.sy, hy = o.hy ?? sy * 1.04, sx = o.sx, hx = o.hx;
  const legs = digiLegs(sx, sy, hx, hy, o.t ?? 1, o.pw ?? 0.035, o);
  const hs = o.hs, neck = o.neck;
  const S = {
    key: o.key, hip: [hx + 0.02, hy], sh: [sx - 0.02, sy],
    torso: o.torso, torsoSq: [2.3, 2.0], neck,
    head: { ang: o.ang ?? -14, st: headSt(o.headTpl || CAT_HEAD, hs, o.ml ?? 1), eye: [0.4 * hs, 0.17 * hs, 0.28 * hs, 0.045 * hs, o.iris || '#8a6a1c'], cap1: 0.4 },
    ...legs,
    tail: o.tail,
    ears: o.ears || { at: [0.1 * hs, 0.32 * hs, 0.24 * hs], dir: [-0.2, 1, 0.5], len: 0.24 * hs, w: 0.14 * hs, th: 0.03 * hs, prof: [[0, 0.85], [0.4, 1], [0.8, 0.7], [1, 0.2]], cap: 0.9 },
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
  const hs = 0.45;
  const neck = [[0.36, 0.86, 0.16, 0.19, 0.22], [0.56, 0.96, 0.15, 0.16, 0.18], [0.72, 1.02, 0.14, 0.14, 0.15]];
  return bigCat({
    key: 'lion', sx: 0.44, sy: 0.8, hx: -0.46, hy: 0.84, hs, neck, ang: -12, t: 1.25, pw: 0.042,
    torso: [[-0.7, 0.96, 0.74, 0.12], [-0.62, 1.03, 0.62, 0.2], [-0.44, 1.04, 0.56, 0.23], [-0.15, 1.0, 0.54, 0.24], [0.15, 1.02, 0.47, 0.25], [0.4, 1.06, 0.46, 0.24], [0.56, 1.0, 0.56, 0.2], [0.66, 0.88, 0.67, 0.13]],
    tail: catTail(-0.68, 0.98, 0.9, 0.035, { tuft: 0.055, a0: -72, bend: 75 }),
    paint(q) {
      if (q.part === 'tail') return q.u > 0.86 ? '#2b1a0f' : furCounter(q, '#c08a4a', '#dcb985');
      if (q.part === 'mane') return mix('#4a2a12', '#a8702f', sstep(0.2, 0.95, q.u) * 0.7 + fbm(q.p.x * 9, q.p.y * 9, q.p.z * 9) * 0.5 - 0.15);
      if (q.part === 'head') {
        const [x, y] = q.hl;
        if (x > 0.35 && y > -0.005 && Math.abs(q.p.z) < 0.05) return '#3a2420';
        if (x > 0.25 && y < -0.01) return '#efe2c6';
        return furCounter(q, '#c99350', '#e6cf9f');
      }
      if (q.part === 'ear') return q.c < -0.2 ? '#e7d2b0' : '#5a3a20';
      if (q.part === 'legF' || q.part === 'legH') return mix('#c08a4a', '#e2c79a', sstep(-0.3, -0.9, q.n.z * Math.sign(q.p.z)) * 0.8 + sstep(0.3, 0.05, q.p.y) * 0.3);
      return furCounter(q, '#c08a4a', '#e4cc9c');
    },
    extras(b, { HF, paint }) {
      // shaggy mane: a lumpy loft over the neck and the back of the head, longer under the throat
      const mp = [[[0.3, 0.98], 0.19, 0.17, 0.24, 'chest'], [[0.46, 1.01], 0.24, 0.21, 0.3, 'n0'], [[0.6, 1.03], 0.27, 0.24, 0.31, 'n1'], [HF.pt(-0.04, 0.0), 0.27, 0.25, 0.3, 'head'], [HF.pt(0.07, -0.01), 0.25, 0.23, 0.27, 'head']];
      b.loft(mp.map(([p, w, u, d, bn]) => ({ p: p.isVector3 ? p : [p[0], p[1], 0], r: [w, u, d], b: bn })), {
        around: 20, part: 'mane', paint, cap0: 0.6, cap1: 0.12, step: 0.035,
        disp: (q) => (fbm(q.p.x * 14, q.p.y * 14, q.p.z * 14) - 0.5) * 0.09 + (q.c < -0.3 ? 0.02 : 0),
      });
    },
  });
}

/** Shared cat colouring: countershaded coat, pale muzzle/chin/inner legs, dark nose, ear backs. */
function catPaint(o) {
  return (q) => {
    const hs = o.hs;
    if (q.part === 'head') {
      const [x, y] = q.hl;
      if (x > 0.45 * hs + 0.33 * hs * (o.ml ?? 1) && y > -0.02 * hs && Math.abs(q.p.z) < 0.1 * hs) return o.nose || '#3b2622';
      if (o.tear && Math.abs(q.p.z) > 0.12 * hs) { const t = clamp((x - 0.38 * hs) / (0.3 * hs)), ly = lerp(0.12 * hs, -0.12 * hs, t); if (Math.abs(y - ly) < 0.022 * hs && x > 0.36 * hs && x < 0.72 * hs) return '#1c140e'; }
      if (x > 0.55 * hs && y < 0.02 * hs) return o.muzzle || o.belly;
      if (o.face && x > 0.18 * hs && x < 0.55 * hs && y > -0.05 * hs && y < 0.14 * hs && Math.abs(q.p.z) > 0.2 * hs) return o.face;
      return furCounter(q, o.top, o.belly, -0.8, -0.2);
    }
    if (q.part === 'ear') return q.c < -0.2 ? (o.earIn || o.belly) : (o.earBack || o.top);
    if (q.part === 'tail') return o.tailTip && q.u > o.tailTip[0] ? o.tailTip[1] : furCounter(q, o.top, o.belly, -0.9, -0.2);
    if (q.part === 'legF' || q.part === 'legH') {
      if (q.p.y < 0.008) return '#3a2d26';
      const inner = sstep(-0.2, -0.8, q.n.z * Math.sign(q.p.z));
      return mix(o.leg || o.top, o.belly, inner * (o.innerK ?? 0.6) + sstep(0.45, 0.0, q.n.y + 0.4) * 0.15);
    }
    return furCounter(q, o.top, o.belly, -0.65, 0.05);
  };
}

function tiger() {
  const hs = 0.43;
  const o = { hs, top: '#dc732b', belly: '#f4ede2', muzzle: '#f7f2ea', face: '#f5efe6', nose: '#c98a7c', earBack: '#1b1512', earIn: '#f1e8dc', tailTip: [0.9, '#16110e'] };
  const ink = '#16100c';
  return bigCat({
    key: 'tiger', sx: 0.5, sy: 0.76, hx: -0.55, hy: 0.8, hs, ang: -12, t: 1.3, pw: 0.045,
    neck: [[0.42, 0.84, 0.16, 0.18, 0.21], [0.62, 0.93, 0.15, 0.15, 0.17], [0.78, 0.98, 0.14, 0.13, 0.14]],
    torso: [[-0.8, 0.93, 0.71, 0.12], [-0.72, 1.01, 0.59, 0.21], [-0.52, 1.02, 0.54, 0.24], [-0.2, 0.98, 0.51, 0.25], [0.15, 1.0, 0.46, 0.26], [0.45, 1.04, 0.45, 0.25], [0.62, 0.97, 0.55, 0.2], [0.72, 0.85, 0.66, 0.13]],
    tail: catTail(-0.78, 0.95, 0.92, 0.042, { a0: -68, bend: 80, taper: 0.25 }),
    paint: catPaint(o),
    pat(part) {
      const t = { mode: PAT.tiger, ink, prm: 0.42 };
      if (part === 'torso' || part === 'neck') return { ...t, scale: 7, coord: (q) => q.p.x + q.p.y * 0.25, mask: (q) => sstep(-0.85, 0.1, q.n.y) };
      if (part === 'legF' || part === 'legH') return { ...t, scale: 9, prm: 0.32, coord: (q) => q.p.y, mask: (q) => sstep(0.12, 0.45, q.p.y) * (0.5 + 0.5 * sstep(-0.3, 0.4, q.n.z * Math.sign(q.p.z))) };
      if (part === 'head') return { ...t, scale: 26, prm: 0.32, coord: (q) => q.hl[0] * 0.7 + Math.abs(q.p.z) * 0.8, mask: (q) => (q.hl[0] < 0.5 * hs ? sstep(-0.05, 0.1, q.hl[1] / hs) : 0) };
      if (part === 'tail') return { mode: PAT.zebra, ink, prm: 0.42, scale: 1, coord: (q) => q.u * 8, mask: (q) => sstep(0.1, 0.25, q.u) };
      return null;
    },
    extras(b, { HF, paint }) {
      // cheek ruffs
      [1, -1].forEach((s) => b.ell(HF.pt(0.16 * hs, -0.12 * hs, 0.3 * hs * s), [0.2 * hs, 0.2 * hs, 0.14 * hs], 'head', '#f4eee4', { rot: [0, 0, 0.6], part: 'ruff' }));
    },
  });
}

function cheetah() {
  const hs = 0.3;
  const o = { hs, top: '#e1ad5f', belly: '#f5ead3', muzzle: '#f4e9d6', nose: '#2a1d18', earBack: '#2e2218', earIn: '#efe2c9', tear: true, tailTip: [0.95, '#f6f1e8'] };
  const ink = '#1c130c';
  return bigCat({
    key: 'cheetah', sx: 0.42, sy: 0.76, hx: -0.42, hy: 0.8, hs, ang: -12, t: 0.82, pw: 0.032, hz: 0.85,
    neck: [[0.36, 0.86, 0.11, 0.13, 0.15], [0.54, 0.97, 0.1, 0.105, 0.115], [0.69, 1.02, 0.09, 0.09, 0.095]],
    torso: [[-0.62, 0.92, 0.72, 0.1], [-0.55, 0.98, 0.62, 0.16], [-0.4, 0.98, 0.6, 0.17], [-0.15, 0.95, 0.62, 0.155], [0.1, 0.97, 0.52, 0.17], [0.35, 1.02, 0.45, 0.19], [0.52, 0.97, 0.53, 0.16], [0.6, 0.87, 0.65, 0.1]],
    tail: catTail(-0.6, 0.93, 0.8, 0.032, { a0: -74, bend: 105, taper: 0.15 }),
    ears: { at: [0.04 * hs, 0.28 * hs, 0.22 * hs], dir: [-0.3, 1, 0.5], len: 0.14 * hs, w: 0.12 * hs, th: 0.025 * hs, prof: [[0, 0.8], [0.4, 1], [0.8, 0.8], [1, 0.3]], cap: 1 },
    paint: catPaint(o),
    pat(part) {
      const sp = { mode: PAT.spots, ink, prm: 0.3 };
      if (part === 'torso' || part === 'neck') return { ...sp, scale: 20, mask: (q) => 0.55 + 0.45 * sstep(-0.7, 0.0, q.n.y) };
      if (part === 'legF' || part === 'legH') return { ...sp, scale: 26, mask: (q) => sstep(0.06, 0.3, q.p.y) * 0.9 };
      if (part === 'head') return { ...sp, scale: 55, prm: 0.26, mask: (q) => (q.hl[0] < 0.4 * hs && q.hl[1] > 0 ? 1 : 0) };
      if (part === 'tail') return { mode: PAT.zebra, ink, prm: 0.45, scale: 1, coord: (q) => q.u * 10, mask: (q) => sstep(0.55, 0.65, q.u) * (1 - sstep(0.93, 0.95, q.u)) };
      return null;
    },
  });
}

function jaguar() {
  const hs = 0.42;
  const o = { hs, top: '#d6973a', belly: '#f3e4c4', muzzle: '#f1e2c6', nose: '#4a2c22', earBack: '#2a1c12', earIn: '#ecdcc0' };
  const ink = '#20140b';
  return bigCat({
    key: 'jaguar', sx: 0.42, sy: 0.6, hx: -0.45, hy: 0.64, hs, ml: 0.9, ang: -14, t: 1.35, pw: 0.046, hz: 0.75,
    neck: [[0.36, 0.7, 0.15, 0.17, 0.19], [0.54, 0.78, 0.145, 0.14, 0.16], [0.68, 0.82, 0.135, 0.12, 0.13]],
    torso: [[-0.66, 0.79, 0.57, 0.13], [-0.58, 0.86, 0.46, 0.21], [-0.4, 0.87, 0.41, 0.24], [-0.1, 0.84, 0.39, 0.25], [0.2, 0.86, 0.37, 0.26], [0.42, 0.89, 0.37, 0.24], [0.56, 0.84, 0.45, 0.19], [0.64, 0.74, 0.55, 0.12]],
    tail: catTail(-0.64, 0.8, 0.6, 0.042, { a0: -68, bend: 90, taper: 0.3 }),
    paint: catPaint(o),
    pat(part) {
      if (part === 'torso') return { mode: PAT.rosettes, ink, prm: 0.34, scale: 9.5, mask: (q) => 0.35 + 0.65 * sstep(-0.8, -0.1, q.n.y) };
      if (part === 'neck') return { mode: PAT.rosettes, ink, prm: 0.3, scale: 12 };
      if (part === 'legF' || part === 'legH') return { mode: PAT.spots, ink, prm: 0.28, scale: 22, mask: (q) => sstep(0.05, 0.2, q.p.y) };
      if (part === 'head') return { mode: PAT.spots, ink, prm: 0.24, scale: 40, mask: (q) => (q.hl[0] < 0.5 * hs ? 1 : 0) };
      if (part === 'tail') return { mode: PAT.spots, ink, prm: 0.3, scale: 18 };
      return null;
    },
  });
}

function snowLeopard() {
  const hs = 0.34;
  const o = { hs, ml: 0.8, top: '#cbc5b8', belly: '#f2efe9', muzzle: '#eeebe5', nose: '#b98a84', earBack: '#3a3836', earIn: '#ebe7e0', iris: '#9fae9c' };
  const ink = '#3e3c3a';
  return bigCat({
    key: 'snow-leopard', sx: 0.4, sy: 0.58, hx: -0.42, hy: 0.62, hs, ml: 0.8, ang: -12, t: 1.25, pw: 0.05, hz: 0.75, iris: '#a7b39a',
    neck: [[0.34, 0.69, 0.14, 0.16, 0.18], [0.5, 0.76, 0.13, 0.135, 0.15], [0.63, 0.8, 0.12, 0.115, 0.125]],
    torso: [[-0.6, 0.76, 0.56, 0.12], [-0.52, 0.83, 0.46, 0.19], [-0.35, 0.84, 0.42, 0.22], [-0.05, 0.81, 0.41, 0.23], [0.2, 0.83, 0.39, 0.23], [0.4, 0.86, 0.4, 0.22], [0.54, 0.81, 0.46, 0.18], [0.62, 0.72, 0.55, 0.11]],
    tail: { ...catTail(-0.58, 0.79, 0.9, 0.075, { a0: -58, bend: 90, taper: 0.05, around: 11 }), disp: (q) => (fbm(q.p.x * 20, q.p.y * 20, q.p.z * 20) - 0.5) * 0.02 },
    paint: catPaint(o),
    pat(part) {
      if (part === 'torso' || part === 'neck') return { mode: PAT.openRosettes, ink, prm: 0.36, scale: 7.5, mask: (q) => 0.3 + 0.7 * sstep(-0.8, -0.1, q.n.y) };
      if (part === 'legF' || part === 'legH') return { mode: PAT.spots, ink, prm: 0.26, scale: 20, mask: (q) => sstep(0.08, 0.3, q.p.y) };
      if (part === 'head') return { mode: PAT.spots, ink, prm: 0.22, scale: 45, mask: (q) => (q.hl[0] < 0.45 * hs && q.hl[1] > -0.05 ? 1 : 0) };
      if (part === 'tail') return { mode: PAT.openRosettes, ink, prm: 0.4, scale: 9, mask: () => 0.9 };
      return null;
    },
  });
}

function canid(o) {
  const legs = digiLegs(o.sx, o.sy, o.hx, o.hy, o.t, o.pw, { canid: true, zf: o.zf ?? 0.1, zh: o.zh ?? 0.1 });
  return buildQuad({
    key: o.key, hip: [o.hx + 0.02, o.hy], sh: [o.sx - 0.02, o.sy],
    torso: o.torso, torsoSq: [2.2, 1.9], neck: o.neck,
    head: { ang: o.ang ?? -16, st: headSt(DOG_HEAD, o.hs, o.ml ?? 1), eye: [0.3 * o.hs, 0.16 * o.hs, 0.2 * o.hs, 0.04 * o.hs, o.iris || '#8a6420'], cap1: 0.5 },
    ...legs, tail: o.tail, ears: o.ears,
    gait: o.gait, headFn: (st) => lookHead(st, 0.7, 0.6), grazeW: [0.45, 0.35, 0.2],
    paint: o.paint, pat: o.pat, extras: o.extras, neckDisp: o.neckDisp, torsoDisp: o.torsoDisp,
  });
}
function grayWolf() {
  const hs = 0.3;
  const fur = (k) => (q) => (fbm(q.p.x * 22, q.p.y * 22, q.p.z * 22) - 0.5) * k;
  return canid({
    key: 'gray-wolf', sx: 0.4, sy: 0.66, hx: -0.42, hy: 0.68, t: 0.95, pw: 0.036, hs, ml: 0.95, ang: -20,
    torso: [[-0.6, 0.84, 0.62, 0.1], [-0.52, 0.9, 0.53, 0.16], [-0.35, 0.9, 0.5, 0.18], [-0.1, 0.88, 0.5, 0.19], [0.15, 0.92, 0.44, 0.2], [0.38, 0.97, 0.42, 0.2], [0.52, 0.92, 0.5, 0.17], [0.6, 0.82, 0.6, 0.11]],
    neck: [[0.34, 0.8, 0.15, 0.17, 0.2], [0.52, 0.91, 0.14, 0.15, 0.17], [0.66, 0.97, 0.12, 0.12, 0.13]],
    neckDisp: fur(0.03), torsoDisp: fur(0.012),
    ears: { at: [0.04 * hs, 0.26 * hs, 0.17 * hs], dir: [-0.15, 1, 0.3], len: 0.36 * hs, w: 0.17 * hs, th: 0.04 * hs, prof: [[0, 1], [0.4, 0.85], [0.8, 0.4], [1, 0.05]] },
    tail: { pts: [[-0.58, 0.86, 0.04], [-0.66, 0.78, 0.065], [-0.72, 0.66, 0.075], [-0.75, 0.54, 0.07], [-0.76, 0.44, 0.055], [-0.76, 0.37, 0.02]], swish: 0.3, around: 10, disp: fur(0.03) },
    gait: { kind: 'trot', hz: 1.25, stride: 0.42, lift: 0.08, duty: 0.5, bob: -0.01, pitch: 0.01, nod: 0.03 },
    paint(q) {
      const saddle = sstep(0.2, 0.75, q.n.y) * sstep(-0.55, -0.2, q.p.x) * sstep(0.45, 0.2, q.p.x);
      if (q.part === 'head') {
        const [x, y] = q.hl;
        if (x > 0.45 * hs + 0.5 * hs * 0.95 && y > -0.01) return '#1a1817';
        if (y < -0.02 * hs && x > 0.2 * hs) return '#ece6dc';
        if (x > 0.45 * hs && y < 0.06 * hs && Math.abs(q.p.z) > 0.06 * hs) return '#e0d8cc';
        return mix(furCounter(q, '#7b766e', '#d4ccbf', -0.7, 0.1), '#5a554f', sstep(0.6, 0.9, q.n.y) * 0.6);
      }
      if (q.part === 'ear') return q.c < -0.2 ? '#d9d0c4' : '#6f6a62';
      if (q.part === 'tail') return q.u > 0.82 ? '#262321' : mix('#8a857c', '#5b5650', sstep(0.2, 0.9, q.n.y));
      if (q.part === 'legF' || q.part === 'legH') return q.p.y < 0.008 ? '#2b2724' : mix('#8f877b', '#d9cfbf', sstep(0.6, 0.2, q.p.y) * 0.6 + sstep(-0.2, -0.8, q.n.z * Math.sign(q.p.z)) * 0.3);
      const griz = fbm(q.p.x * 30, q.p.y * 30, q.p.z * 30);
      return mix(mix(furCounter(q, '#8a8378', '#e0d8ca', -0.6, 0.05), '#a39a8c', griz * 0.4), '#3b3834', saddle * 0.85);
    },
  });
}
function thylacine() {
  const hs = 0.29;
  const ink = '#36241a';
  return canid({
    key: 'thylacine', sx: 0.36, sy: 0.52, hx: -0.44, hy: 0.56, t: 0.9, pw: 0.032, hs: 0.31, ml: 1.1, ang: -14,
    torso: [[-0.62, 0.69, 0.49, 0.11], [-0.54, 0.74, 0.41, 0.16], [-0.38, 0.74, 0.39, 0.17], [-0.1, 0.72, 0.39, 0.17], [0.15, 0.74, 0.37, 0.18], [0.36, 0.77, 0.37, 0.17], [0.48, 0.73, 0.43, 0.14], [0.56, 0.65, 0.51, 0.09]],
    neck: [[0.32, 0.65, 0.12, 0.13, 0.15], [0.48, 0.73, 0.11, 0.11, 0.12], [0.6, 0.77, 0.1, 0.1, 0.11]],
    ears: { at: [0.04 * hs, 0.26 * hs, 0.17 * hs], dir: [-0.1, 1, 0.2], len: 0.3 * hs, w: 0.15 * hs, th: 0.035 * hs, prof: [[0, 0.7], [0.4, 1], [0.8, 0.9], [1, 0.45]], cap: 1 },
    tail: { pts: [[-0.6, 0.66, 0.075], [-0.72, 0.6, 0.058], [-0.85, 0.53, 0.042], [-0.97, 0.46, 0.028], [-1.06, 0.41, 0.016]], swish: 0.12, around: 10 },
    gait: { kind: 'walk', hz: 0.9, stride: 0.34, lift: 0.07, duty: 0.6, bob: 0.006, pitch: 0.01, nod: 0.04 },
    paint(q) {
      if (q.part === 'head') {
        const [x, y] = q.hl;
        if (x > 0.45 * hs + 0.5 * hs * 1.1 && y > -0.01) return '#1c1714';
        if (y < -0.03 * hs && x > 0.25 * hs) return '#dcc9a8';
        if (x > 0.15 * hs && x < 0.38 * hs && y > 0.02 * hs && y < 0.14 * hs && Math.abs(q.p.z) > 0.15 * hs) return '#e6d7bc';
        return furCounter(q, '#a9814f', '#d9c39e', -0.7, 0.1);
      }
      if (q.part === 'ear') return q.c < -0.2 ? '#d8c4a2' : '#8a6a44';
      if (q.part === 'legF' || q.part === 'legH') return q.p.y < 0.008 ? '#2b2420' : mix('#ad8554', '#d8c4a0', sstep(-0.2, -0.8, q.n.z * Math.sign(q.p.z)) * 0.7);
      if (q.part === 'tail') return mix('#a07a4c', '#7a5a36', sstep(0.2, 0.9, q.u));
      return furCounter(q, '#b28958', '#dcc6a1', -0.6, 0.05);
    },
    pat(part) {
      const st = { mode: PAT.zebra, ink, prm: 0.34, scale: 11 };
      if (part === 'torso') return { ...st, coord: (q) => q.p.x - q.p.y * 0.15, mask: (q) => sstep(0.05, -0.15, q.p.x) * sstep(-0.55, 0.1, q.n.y) };
      if (part === 'legH') return { ...st, coord: (q) => q.p.x - q.p.y * 0.15, mask: (q) => sstep(0.42, 0.62, q.p.y) * 0.8 };
      if (part === 'tail') return { ...st, coord: (q) => q.p.x - q.p.y * 0.15, mask: (q) => 1 - sstep(0.05, 0.2, q.u) };
      return null;
    },
  });
}

// ---------------------------------------------------------------- hoofed animals
/** Unguligrade leg spec from joint positions and a thickness scale. */
function hoofLeg(j, t, o = {}) {
  const front = j[1][0] < j[0][0];
  const r = front
    ? [[0.13 * t, 0.1 * t], [0.08 * t, 0.07 * t], [0.046 * t, 0.042 * t], [0.042 * t, 0.037 * t], [0.04 * t, 0.037 * t]]
    : [[0.16 * t, 0.115 * t], [0.1 * t, 0.08 * t], [0.05 * t, 0.042 * t], [0.042 * t, 0.037 * t], [0.04 * t, 0.037 * t]];
  const mid = front
    ? [[0.12 * t, 0.09 * t], [0.065 * t, 0.055 * t, null, 0.3], [0.033 * t, 0.031 * t], null]
    : [[0.15 * t, 0.105 * t], [0.072 * t, 0.055 * t, null, 0.35], [0.035 * t, 0.031 * t], null];
  const h = o.hoofH ?? j[4][1];
  return {
    z: o.z, j, r: o.r || r, mid: o.mid || mid, top: o.top,
    hoof: o.hoof === false ? null : { h, r0: 0.038 * t, r1: 0.05 * t, color: o.hoofColor || '#2a2420', dx: 0.008 * t },
    cflex: front ? 1.3 : 0.4, flex: 1.0, follow: front ? 0.7 : 0.9, toeOff: 0.5,
  };
}

function giraffe() {
  const neck = [[0.4, 2.5, 0.3, 0.36, 0.4], [0.7, 3.0, 0.2, 0.23, 0.25], [0.98, 3.48, 0.145, 0.16, 0.17], [1.22, 3.92, 0.115, 0.12, 0.13], [1.38, 4.2, 0.095, 0.1, 0.1]];
  const hs = 1;
  const cream = '#efe1c2';
  const patch = (q) => mix('#9c5524', '#b8743a', sstep(1.2, 2.6, q.p.y));
  return buildQuad({
    key: 'giraffe', hip: [-0.66, 2.2], sh: [0.5, 2.3],
    torso: [[-0.98, 2.36, 2.02, 0.17], [-0.88, 2.5, 1.86, 0.29], [-0.6, 2.56, 1.73, 0.36], [-0.25, 2.68, 1.68, 0.38], [0.15, 2.88, 1.7, 0.38], [0.45, 3.02, 1.78, 0.36], [0.65, 2.92, 1.92, 0.3], [0.8, 2.6, 2.06, 0.2]],
    neck, neckStep: 0.09,
    head: { ang: -38, st: [[-0.08, 0.1, -0.12, 0.1], [0.05, 0.14, -0.16, 0.12], [0.22, 0.12, -0.13, 0.11], [0.38, 0.08, -0.1, 0.085], [0.52, 0.07, -0.085, 0.08], [0.6, 0.05, -0.06, 0.066], [0.64, 0.02, -0.03, 0.04]], eye: [0.12, 0.07, 0.105, 0.035] },
    legF: { ...hoofLeg([[0.58, 2.15], [0.44, 1.72], [0.52, 0.86], [0.54, 0.2], [0.6, 0.12]], 1.75, { z: 0.2, top: [0.55, 2.45, 0.22, 0.16] }), cflex: 0.6, flex: 0.7 },
    legH: hoofLeg([[-0.72, 2.12], [-0.46, 1.62], [-0.76, 0.95], [-0.7, 0.2], [-0.64, 0.12]], 1.75, { z: 0.2, top: [-0.74, 2.38, 0.26, 0.18] }),
    tail: { pts: [[-0.96, 2.3, 0.05], [-1.02, 2.1, 0.035], [-1.04, 1.85, 0.03], [-1.04, 1.6, 0.028], [-1.03, 1.42, 0.06], [-1.02, 1.3, 0.04]], swish: 0.5 },
    ears: { at: [0.02, 0.1, 0.09], dir: [-0.4, 0.4, 1], len: 0.17, w: 0.05, th: 0.014, prof: [[0, 0.5], [0.4, 1], [0.8, 0.7], [1, 0.15]] },
    gait: { kind: 'pace', hz: 0.55, stride: 1.1, lift: 0.18, duty: 0.66, bob: 0.02, pitch: 0.012, nod: 0.12, roll: 0.03 },
    headFn: (st) => { const h = lookHead(st, 0.6, 0.0); const x = fract(st.tau / 19); const br = st.idle * sstep(0.3, 0.38, x) * (1 - sstep(0.62, 0.7, x)); return { ...h, down: -br * 0.25, lift: br * 0.5 + Math.sin(st.tau * 6) * 0.02 * br }; },
    grazeW: [0.2, 0.3, 0.3, 0.2],
    paint(q) {
      if (q.part === 'head') {
        const [x, y] = q.hl;
        if (x > 0.48) return mix('#d9c09a', '#5a4434', sstep(0.6, 0.66, x) * 0.6);
        return mix(cream, '#c99a62', sstep(-0.05, 0.08, y) * 0.5);
      }
      if (q.part === 'ear') return q.c < -0.2 ? '#f2e6cf' : '#b8875a';
      if (q.part === 'tail') return q.u > 0.75 ? '#1f1712' : '#c9a06a';
      if (q.part === 'mane') return '#8a4c22';
      if (q.part === 'legF' || q.part === 'legH') return q.p.y < 1.0 ? cream : mix(cream, patch(q), sstep(1.0, 1.5, q.p.y));
      return patch(q);
    },
    pat(part) {
      const n = { mode: PAT.network, ink: cream, prm: 0.11 };
      if (part === 'torso') return { ...n, scale: 2.3 };
      if (part === 'neck') return { ...n, scale: 3.0, mask: (q) => sstep(-0.3, 0.2, q.n.x * -1 + q.n.y) * 0.25 + 0.75 };
      if (part === 'legF' || part === 'legH') return { ...n, scale: 3.4, mask: (q) => sstep(0.95, 1.6, q.p.y) };
      if (part === 'head') return { ...n, scale: 7, prm: 0.14, mask: (q) => (q.hl[0] < 0.42 ? 0.65 : 0) };
      return null;
    },
    extras(b, { HF, paint }) {
      neckCrest(b, neck, { w: 0.03, h: 0.07, inset: 0.9, paint: () => '#7c4420', prof: (k) => 1 - k * 0.4 });
      [1, -1].forEach((s) => {
        const base = HF.pt(-0.02, 0.12, 0.05 * s), tip = HF.pt(-0.08, 0.3, 0.08 * s);
        b.loft([{ p: base, r: 0.035 }, { p: base.clone().lerp(tip, 0.6), r: 0.028 }, { p: tip, r: 0.045 }], { part: 'horn', paint: (q) => (q.u > 0.75 ? '#1f1712' : '#b98b5a'), b: 'head', around: 8, cap1: 1 });
      });
    },
  });
}

function dromedary() {
  const sand = '#c89c69';
  return buildQuad({
    key: 'dromedary', hip: [-0.55, 1.42], sh: [0.45, 1.4],
    torso: [[-0.86, 1.75, 1.46, 0.16], [-0.76, 1.86, 1.31, 0.26], [-0.5, 1.92, 1.18, 0.32], [-0.2, 1.96, 1.12, 0.34], [0.15, 1.96, 1.12, 0.33], [0.4, 1.9, 1.2, 0.3], [0.58, 1.76, 1.32, 0.24], [0.68, 1.56, 1.42, 0.16]],
    neck: [[0.5, 1.58, 0.2, 0.24, 0.26], [0.8, 1.52, 0.13, 0.14, 0.17], [1.02, 1.64, 0.1, 0.11, 0.12], [1.15, 1.86, 0.085, 0.09, 0.1], [1.2, 2.04, 0.08, 0.085, 0.09]],
    head: { ang: -8, st: [[-0.06, 0.1, -0.1, 0.085], [0.06, 0.12, -0.12, 0.1], [0.2, 0.09, -0.1, 0.08], [0.34, 0.07, -0.09, 0.065], [0.45, 0.065, -0.095, 0.064], [0.51, 0.03, -0.05, 0.045]], eye: [0.1, 0.06, 0.085, 0.024] },
    legF: { z: 0.17, j: [[0.48, 1.38], [0.38, 1.08], [0.44, 0.6], [0.45, 0.12], [0.53, 0.05]], r: [[0.16, 0.12], [0.1, 0.09], [0.07, 0.065], [0.055, 0.055], [0.055, 0.08]], mid: [[0.14, 0.11], [0.075, 0.065, null, 0.3], [0.045, 0.042], null], top: [0.5, 1.58, 0.15, 0.11], cflex: 1.0, flex: 0.5, follow: 0.7 },
    legH: { z: 0.17, j: [[-0.6, 1.42], [-0.44, 1.04], [-0.64, 0.62], [-0.6, 0.12], [-0.52, 0.05]], r: [[0.18, 0.13], [0.11, 0.09], [0.065, 0.06], [0.055, 0.055], [0.055, 0.08]], mid: [[0.17, 0.12], [0.08, 0.065, null, 0.35], [0.047, 0.043], null], top: [-0.62, 1.62, 0.17, 0.12], cflex: 0.3, flex: 0.5, follow: 0.9 },
    tail: { pts: [[-0.85, 1.72, 0.035], [-0.9, 1.58, 0.028], [-0.92, 1.4, 0.024], [-0.92, 1.24, 0.03], [-0.91, 1.14, 0.02]], swish: 0.5 },
    ears: { at: [0.0, 0.1, 0.07], dir: [-0.6, 0.6, 0.5], len: 0.08, w: 0.035, th: 0.012, prof: [[0, 0.7], [0.5, 1], [1, 0.5]], cap: 1 },
    gait: { kind: 'pace', hz: 0.62, stride: 0.85, lift: 0.14, duty: 0.64, bob: 0.015, pitch: 0.01, nod: 0.1, roll: 0.05 },
    headFn: (st) => grazeHead(st, 0.9, 0.7, 0.5), grazeW: [0.6, 0.25, 0.1, 0.05],
    paint(q) {
      if (q.part === 'head') return q.hl[0] > 0.4 && q.hl[1] < -0.03 ? '#a57d55' : furCounter(q, '#c39463', '#d9bd96', -0.8, 0);
      if (q.part === 'tail') return q.u > 0.7 ? '#5a4128' : sand;
      if (q.part === 'hump') return mix(sand, '#b88d5d', sstep(1.95, 2.35, q.p.y) * 0.5);
      if (q.part === 'legF' || q.part === 'legH') { if (q.p.y < 0.03) return '#6a5038'; const knee = Math.abs(q.p.y - (q.part === 'legF' ? 0.6 : 0.62)) < 0.04 ? 0.75 : 1; return mul(mix(sand, '#d7b98f', sstep(0.9, 0.3, q.p.y) * 0.5), knee); }
      return furCounter(q, sand, '#dcc19a', -0.8, 0.0);
    },
    extras(b, { paint }) {
      const sw = (x) => { const t = sstep(-0.53, 0.43, x); return { pelvis: 1 - t, chest: t }; };
      b.loft([[-0.6, 1.9, 0.24], [-0.42, 2.14, 0.29], [-0.2, 2.32, 0.31], [0.02, 2.34, 0.31], [0.22, 2.18, 0.28], [0.4, 1.96, 0.22]].map(([x, top, w]) => ({ p: [x, 1.75, 0], r: [w, top - 1.75, 0.1], b: sw(x) })),
        { axis: [1, 0, 0], around: 16, sq: [2.7, 2], part: 'hump', paint, cap0: 0.7, cap1: 0.7 });
    },
  });
}

function moose() {
  const dark = '#3a2a1e';
  return buildQuad({
    key: 'moose', hip: [-0.58, 1.55], sh: [0.48, 1.55],
    torso: [[-0.82, 1.76, 1.42, 0.15], [-0.72, 1.84, 1.24, 0.26], [-0.46, 1.87, 1.13, 0.32], [-0.1, 1.9, 1.08, 0.34], [0.25, 2.02, 1.1, 0.34], [0.5, 2.14, 1.2, 0.3], [0.66, 2.0, 1.36, 0.24], [0.76, 1.68, 1.46, 0.16]],
    neck: [[0.5, 1.7, 0.24, 0.3, 0.3], [0.76, 1.84, 0.19, 0.22, 0.23], [0.96, 1.9, 0.16, 0.17, 0.18]],
    head: { ang: -40, st: [[-0.08, 0.13, -0.15, 0.12], [0.06, 0.15, -0.17, 0.13], [0.25, 0.12, -0.14, 0.11], [0.45, 0.1, -0.12, 0.095], [0.6, 0.12, -0.14, 0.11], [0.72, 0.1, -0.13, 0.1], [0.78, 0.05, -0.07, 0.07]], eye: [0.14, 0.07, 0.105, 0.026] },
    legF: hoofLeg([[0.54, 1.5], [0.42, 1.18], [0.48, 0.62], [0.5, 0.16], [0.55, 0.1]], 1.4, { z: 0.18, top: [0.52, 1.72, 0.16, 0.12], hoofColor: '#1b1714' }),
    legH: hoofLeg([[-0.62, 1.55], [-0.44, 1.12], [-0.7, 0.68], [-0.64, 0.16], [-0.58, 0.1]], 1.4, { z: 0.18, top: [-0.64, 1.76, 0.18, 0.13], hoofColor: '#1b1714' }),
    tail: { pts: [[-0.82, 1.74, 0.04], [-0.86, 1.66, 0.035], [-0.88, 1.58, 0.02]], swish: 0.3 },
    ears: { at: [0.0, 0.13, 0.08], dir: [-0.5, 0.6, 0.7], len: 0.22, w: 0.06, th: 0.015, prof: [[0, 0.5], [0.4, 1], [0.8, 0.8], [1, 0.25]] },
    gait: { kind: 'walk', hz: 0.68, stride: 0.85, lift: 0.18, duty: 0.63, bob: 0.015, pitch: 0.01, nod: 0.07 },
    headFn: (st) => grazeHead(st, 1.0, 0.5, 0.5), grazeW: [0.7, 0.3],
    paint(q) {
      if (q.part === 'head') return q.hl[0] > 0.52 ? '#2a1e16' : furCounter(q, '#4a3626', '#5d4634', -0.8, 0);
      if (q.part === 'ear') return q.c < -0.2 ? '#6a5442' : '#3e2c20';
      if (q.part === 'antler') return mix('#d6c39a', '#a99068', sstep(0.2, 0.8, q.u) * 0.5);
      if (q.part === 'bell') return '#2d2018';
      if (q.part === 'legF' || q.part === 'legH') return mix('#a49580', dark, sstep(0.75, 1.05, q.p.y));
      return mix(furCounter(q, '#45311f', '#2e2016', -0.8, 0.2), '#5e4630', sstep(1.9, 2.1, q.p.y) * 0.3);
    },
    extras(b, { HF, paint }) {
      // bell (dewlap) and palmate antlers
      b.loft([{ p: HF.pt(0.08, -0.14, 0), r: [0.03, 0.05, 0.05] }, { p: HF.pt(0.02, -0.32, 0), r: [0.025, 0.035, 0.035] }, { p: HF.pt(-0.02, -0.42, 0), r: [0.02, 0.025, 0.025] }], { b: 'head', part: 'bell', paint, around: 8, ref: HF.d });
      [1, -1].forEach((s) => {
        const base = HF.pt(0.0, 0.14, 0.08 * s);
        const p1 = base.clone().add(new THREE.Vector3(-0.02, 0.05, 0.16 * s));
        b.loft([{ p: base, r: 0.045 }, { p: p1, r: 0.04 }], { b: 'head', part: 'antler', paint, around: 8 });
        // palm: broad thin blade spreading out and up (loft runs sideways; radii = forward/backward extent)
        const palm = [[0, 0.0, 0.06, 0.05], [0.25, 0.08, 0.16, 0.2], [0.5, 0.16, 0.2, 0.26], [0.75, 0.24, 0.17, 0.22], [1, 0.3, 0.06, 0.08]];
        const pp = palm.map(([t, up, fw, bk]) => ({ p: p1.clone().add(new THREE.Vector3(-0.04 * t, up, 0.62 * t * s)), r: [0.02, fw, bk] }));
        b.loft(pp, { b: 'head', part: 'antler', paint, ref: [1, 0, 0], around: 12, cap0: 0.3, cap1: 0.5 });
        for (let k = 0; k < 7; k++) {
          const t = 0.2 + k * 0.12, u = palm[0][1] + (0.3 * t), side = k % 2 ? 1 : -1;
          const ext = side > 0 ? 0.17 * Math.sin(Math.PI * t) + 0.04 : 0.22 * Math.sin(Math.PI * t) + 0.05;
          const root = p1.clone().add(new THREE.Vector3(-0.04 * t + side * ext * 0.85, u + 0.02, 0.62 * t * s));
          const tip = root.clone().add(new THREE.Vector3(side * 0.06, 0.13, 0.03 * s));
          b.loft([{ p: root, r: 0.024 }, { p: tip, r: 0.006 }], { b: 'head', part: 'antler', paint, around: 6, cap1: 0.5 });
        }
      });
    },
  });
}

function bison() {
  const shag = (k) => (q) => (fbm(q.p.x * 9, q.p.y * 9, q.p.z * 9) - 0.45) * k;
  const front = (x) => sstep(-0.1, 0.25, x);
  return buildQuad({
    key: 'american-bison', hip: [-0.55, 1.12], sh: [0.42, 1.12],
    torso: [[-0.8, 1.42, 1.08, 0.14], [-0.7, 1.5, 0.95, 0.24], [-0.45, 1.53, 0.88, 0.28], [-0.15, 1.62, 0.8, 0.31], [0.15, 1.84, 0.7, 0.35], [0.4, 1.94, 0.68, 0.34], [0.6, 1.82, 0.72, 0.29], [0.72, 1.52, 0.82, 0.2]],
    torsoDisp: (q) => shag(0.1)(q) * front(q.p.x) + 0.04 * front(q.p.x),
    neck: [[0.45, 1.45, 0.26, 0.3, 0.33], [0.68, 1.3, 0.25, 0.28, 0.3], [0.84, 1.14, 0.23, 0.24, 0.26]],
    neckDisp: shag(0.1),
    head: { ang: -72, st: [[-0.12, 0.21, -0.2, 0.22], [0.04, 0.25, -0.22, 0.23], [0.2, 0.21, -0.18, 0.19], [0.34, 0.16, -0.14, 0.15], [0.45, 0.13, -0.13, 0.135], [0.51, 0.07, -0.07, 0.1]], eye: [0.14, 0.14, 0.16, 0.026] },
    legF: { ...hoofLeg([[0.5, 1.1], [0.4, 0.82], [0.44, 0.44], [0.45, 0.12], [0.5, 0.07]], 1.3, { z: 0.17, top: [0.5, 1.3, 0.17, 0.13], hoofColor: '#16120f' }), mid: [[0.18, 0.15], [0.095, 0.085, null, 0.35], [0.045, 0.04], null] },
    legH: hoofLeg([[-0.58, 1.12], [-0.42, 0.8], [-0.62, 0.45], [-0.57, 0.12], [-0.52, 0.07]], 1.2, { z: 0.16, top: [-0.6, 1.3, 0.17, 0.12], hoofColor: '#16120f' }),
    tail: { pts: [[-0.8, 1.38, 0.035], [-0.85, 1.25, 0.028], [-0.87, 1.08, 0.024], [-0.87, 0.96, 0.045], [-0.86, 0.88, 0.03]], swish: 0.55 },
    ears: { at: [0.06, 0.12, 0.2], dir: [-0.2, 0.0, 1], len: 0.08, w: 0.03, th: 0.012 },
    gait: { kind: 'walk', hz: 0.8, stride: 0.52, lift: 0.1, duty: 0.63, bob: 0.012, pitch: 0.01, nod: 0.05 },
    headFn: (st) => grazeHead(st, 0.75, 0.25, 0.4), grazeW: [0.6, 0.4],
    paint(q) {
      const f = front(q.p.x);
      if (q.part === 'head') return q.hl[0] > 0.44 ? '#1c140e' : mix('#2a1c11', '#3e2a18', fbm(q.p.x * 12, q.p.y * 12, q.p.z * 12));
      if (q.part === 'horn') return q.u > 0.6 ? '#141110' : '#3a332c';
      if (q.part === 'beard') return '#24180e';
      if (q.part === 'tail') return q.u > 0.6 ? '#1f150c' : '#5a3e24';
      if (q.part === 'legF') return mix('#2a1b10', '#3e2a18', sstep(0.6, 0.9, q.p.y) * fbm(q.p.y * 9, 0, q.p.z * 9));
      if (q.part === 'legH') return mix('#2e1f13', '#5d4128', sstep(0.6, 1.0, q.p.y));
      const back = mix('#6e4d2d', '#7e5a36', fbm(q.p.x * 6, q.p.y * 6, q.p.z * 6));
      return mix(furCounter(q, back, '#4c3420', -0.8, 0), mix('#3b2716', '#5e4126', fbm(q.p.x * 10, q.p.y * 10, q.p.z * 10)), f);
    },
    extras(b, { HF, paint }) {
      [1, -1].forEach((s) => {
        const base = HF.pt(0.02, 0.17, 0.17 * s);
        b.loft([{ p: base, r: 0.045 }, { p: base.clone().add(new THREE.Vector3(-0.02, 0.03, 0.09 * s)), r: 0.035 }, { p: base.clone().add(new THREE.Vector3(0.0, 0.12, 0.12 * s)), r: 0.012 }], { b: 'head', part: 'horn', paint, around: 8, cap1: 0.5 });
      });
      b.loft([{ p: HF.pt(0.3, -0.12, 0), r: [0.07, 0.06, 0.06] }, { p: HF.pt(0.32, -0.28, 0), r: [0.065, 0.06, 0.06] }, { p: HF.pt(0.33, -0.38, 0), r: [0.03, 0.03, 0.03] }],
        { b: 'head', part: 'beard', paint, around: 8, ref: HF.d, disp: shag(0.05) });
      // woolly crown between the horns
      b.ell(HF.pt(-0.02, 0.2, 0), [0.17, 0.12, 0.2], 'head', '#2e1f12', { part: 'mop' });
    },
  });
}

function whiteRhino() {
  const grey = '#8f8a83';
  return buildQuad({
    key: 'white-rhino', hip: [-0.6, 1.05], sh: [0.48, 1.05],
    torso: [[-0.95, 1.4, 1.0, 0.18], [-0.85, 1.52, 0.85, 0.32], [-0.6, 1.58, 0.75, 0.4], [-0.25, 1.55, 0.7, 0.44], [0.1, 1.58, 0.7, 0.44], [0.4, 1.68, 0.76, 0.4], [0.62, 1.6, 0.86, 0.32], [0.75, 1.35, 0.98, 0.2]],
    torsoSq: [2.4, 2.2],
    neck: [[0.52, 1.35, 0.3, 0.36, 0.36], [0.8, 1.38, 0.27, 0.33, 0.3], [0.98, 1.28, 0.24, 0.26, 0.26]],
    head: { ang: -38, st: [[-0.1, 0.2, -0.2, 0.21], [0.08, 0.22, -0.23, 0.22], [0.3, 0.18, -0.19, 0.19], [0.5, 0.14, -0.15, 0.17], [0.66, 0.12, -0.14, 0.19], [0.74, 0.06, -0.08, 0.16]], sq: [2.6, 2.6], eye: [0.32, 0.06, 0.18, 0.022] },
    legF: { z: 0.25, j: [[0.52, 1.0], [0.42, 0.72], [0.47, 0.36], [0.48, 0.13], [0.52, 0.08]], r: [[0.24, 0.2], [0.16, 0.14], [0.11, 0.1], [0.1, 0.1], [0.1, 0.1]], mid: [[0.22, 0.18], [0.13, 0.12, null, 0.35], [0.1, 0.095], null], top: [0.52, 1.2, 0.22, 0.18], hoof: { h: 0.08, r0: 0.1, r1: 0.12, color: '#5a5650' }, cflex: 0.9, flex: 0.6, follow: 0.7 },
    legH: { z: 0.25, j: [[-0.62, 1.02], [-0.45, 0.72], [-0.62, 0.38], [-0.58, 0.13], [-0.54, 0.08]], r: [[0.26, 0.2], [0.17, 0.14], [0.1, 0.1], [0.1, 0.1], [0.1, 0.1]], mid: [[0.25, 0.19], [0.13, 0.12, null, 0.35], [0.095, 0.09], null], top: [-0.64, 1.22, 0.24, 0.18], hoof: { h: 0.08, r0: 0.1, r1: 0.12, color: '#5a5650' }, cflex: 0.3, flex: 0.6, follow: 0.9 },
    tail: { pts: [[-0.95, 1.36, 0.035], [-1.0, 1.22, 0.028], [-1.01, 1.08, 0.024], [-1.0, 1.0, 0.035]], swish: 0.5 },
    ears: { at: [-0.02, 0.18, 0.1], dir: [-0.3, 1, 0.35], len: 0.16, w: 0.055, th: 0.02, prof: [[0, 0.6], [0.4, 1], [0.8, 0.8], [1, 0.2]] },
    gait: { kind: 'walk', hz: 0.62, stride: 0.48, lift: 0.08, duty: 0.66, bob: 0.012, pitch: 0.008, nod: 0.05 },
    headFn: (st) => grazeHead(st, 0.6, 0.15, 0.4), grazeW: [0.7, 0.3],
    paint(q) {
      if (q.part === 'horn') return mix('#a49b8c', '#6e675d', q.u * 0.6);
      const fold = (x0, k = 0.035) => Math.exp(-Math.pow((q.p.x - x0) / k, 2));
      let c = grey;
      if (q.part === 'torso') c = mul(furCounter(q, '#928d86', '#7c7771', -0.9, 0.0), 1 - 0.18 * (fold(0.42) + fold(-0.48)) * sstep(0.95, 1.2, q.p.y));
      if (q.part === 'legF' || q.part === 'legH') c = mix(grey, '#6f6b65', sstep(0.6, 0.15, q.p.y));
      if (q.part === 'head') c = q.hl[0] > 0.62 ? '#7c766f' : '#8a857e';
      return mul(c, 0.94 + 0.12 * fbm(q.p.x * 7, q.p.y * 7, q.p.z * 7));
    },
    extras(b, { HF, paint }) {
      neckCrest(b, [[0.52, 1.35, 0.3, 0.36], [0.8, 1.38, 0.27, 0.33], [0.98, 1.28, 0.24, 0.26]], { w: 0.17, h: 0.09, inset: 0.6, paint: () => grey, prof: (k) => Math.sin(Math.PI * clamp(k * 1.2)) });
      const horn = (x, len, r, lean) => { const base = HF.pt(x, 0.11); const dir = HF.u.clone().multiplyScalar(Math.cos(lean)).addScaledVector(HF.d, Math.sin(lean)); b.loft([{ p: base, r: r }, { p: base.clone().addScaledVector(dir, len * 0.5).addScaledVector(HF.d, -len * 0.06), r: r * 0.55 }, { p: base.clone().addScaledVector(dir, len).addScaledVector(HF.d, -len * 0.15), r: r * 0.08 }], { b: 'head', part: 'horn', paint, around: 9, cap1: 0.3 }); };
      horn(0.6, 0.5, 0.085, 0.45);
      horn(0.38, 0.2, 0.065, 0.2);
    },
  });
}

function hippo() {
  return buildQuad({
    key: 'hippopotamus', hip: [-0.58, 0.68], sh: [0.48, 0.68],
    torso: [[-0.95, 1.1, 0.6, 0.2], [-0.85, 1.24, 0.45, 0.38], [-0.55, 1.31, 0.35, 0.48], [-0.15, 1.32, 0.32, 0.5], [0.25, 1.32, 0.33, 0.5], [0.55, 1.3, 0.38, 0.45], [0.75, 1.18, 0.48, 0.36], [0.88, 1.0, 0.6, 0.25]],
    torsoSq: [2.3, 2.1],
    neck: [[0.62, 0.92, 0.33, 0.33, 0.35], [0.84, 0.94, 0.31, 0.3, 0.31], [0.98, 0.94, 0.29, 0.28, 0.28]],
    head: { ang: -14, st: [[-0.12, 0.24, -0.26, 0.29], [0.05, 0.27, -0.27, 0.31], [0.25, 0.2, -0.22, 0.25], [0.42, 0.18, -0.17, 0.29], [0.58, 0.2, -0.16, 0.35], [0.7, 0.16, -0.14, 0.33], [0.76, 0.08, -0.08, 0.25]], sq: [2.5, 2.3], eye: [0.12, 0.27, 0.16, 0.035, '#2a1210'] },
    legF: { z: 0.3, j: [[0.5, 0.62], [0.42, 0.42], [0.46, 0.2], [0.48, 0.06]], r: [[0.21, 0.18], [0.16, 0.14], [0.13, 0.12], [0.135, 0.13]], mid: [[0.19, 0.17], [0.14, 0.13], null], cap1: 0.25, toeOff: 0.25, flex: 0.5 },
    legH: { z: 0.3, j: [[-0.6, 0.64], [-0.48, 0.42], [-0.6, 0.2], [-0.58, 0.06]], r: [[0.23, 0.19], [0.16, 0.14], [0.13, 0.12], [0.135, 0.13]], mid: [[0.21, 0.18], [0.14, 0.13], null], cap1: 0.25, toeOff: 0.25, flex: 0.5 },
    tail: { pts: [[-0.95, 1.05, 0.05], [-1.02, 0.96, 0.035], [-1.05, 0.86, 0.02]], swish: 0.8 },
    ears: { at: [-0.04, 0.27, 0.13], dir: [-0.4, 1, 0.5], len: 0.07, w: 0.035, th: 0.015, prof: [[0, 0.7], [0.5, 1], [1, 0.4]], cap: 1 },
    gait: { kind: 'walk', hz: 0.75, stride: 0.32, lift: 0.06, duty: 0.66, bob: 0.01, pitch: 0.008, nod: 0.04, roll: 0.03 },
    headFn: (st) => grazeHead(st, 0.35, 0.1, 0.35),
    grazeW: [0.6, 0.4],
    paint(q) {
      const pink = '#d39a8e';
      if (q.part === 'head') { const [x, y] = q.hl; if (x > 0.68 && y > 0.05) return '#5e4747'; if (y < -0.12) return mix('#7a6464', pink, 0.6); return furCounter(q, '#7a6767', '#a98380', -0.7, 0); }
      if (q.part === 'ear') return '#c98a80';
      if (q.part === 'jaw') return q.c > 0.75 ? '#e2a7a0' : mix('#7a6464', pink, sstep(0.0, -0.8, q.c) * 0.7);
      if (q.part === 'tooth') return '#efe6d3';
      if (q.part === 'legF' || q.part === 'legH') return q.p.y < 0.02 ? '#4a3c3c' : '#7a6767';
      return mix(furCounter(q, '#7b6868', pink, -0.75, -0.1), '#6d5a5a', sstep(0.4, 0.9, q.n.y) * 0.4);
    },
    extras(b, { HF, paint }) {
      // hinged lower jaw for the occasional yawn
      b.bone('jaw', 'head', HF.pt(0.08, -0.12));
      b.loft([[0.1, 0.05, 0.22], [0.3, 0.07, 0.26], [0.55, 0.06, 0.3], [0.7, 0.05, 0.28], [0.75, 0.03, 0.22]].map(([x, r, w]) => ({ p: HF.pt(x, -0.2), r: [w, r, r * 1.6] })),
        { b: 'jaw', axis: HF.d, ref: HF.u, part: 'jaw', paint, around: 12, cap1: 0.5 });
      [1, -1].forEach((s) => b.loft([{ p: HF.pt(0.66, -0.2, 0.2 * s), r: 0.035 }, { p: HF.pt(0.68, -0.05, 0.19 * s), r: 0.015 }], { b: 'jaw', part: 'tooth', paint, around: 6 }));
    },
    anim(B, st) { const x = fract(st.tau / 21), yawn = st.idle * sstep(0.55, 0.6, x) * (1 - sstep(0.7, 0.76, x)); B.jaw.rotation.z = -yawn * 0.9; B.head.rotation.z += yawn * 0.35; },
  });
}

// ---------------------------------------------------------------- elephants
function proboscid(o) {
  const nTr = 8;
  return buildQuad({
    key: o.key, hip: o.hip, sh: o.sh, torso: o.torso, torsoSq: [2.2, 2.0], torsoDisp: o.torsoDisp, neck: o.neck,
    head: { ang: o.ang, st: o.head, eye: o.eye, cap1: 0.3 },
    legF: o.legF, legH: o.legH, tail: o.tail,
    gait: { kind: 'walk', hz: 0.5, stride: 0.85, lift: 0.14, duty: 0.7, bob: 0.025, pitch: 0.012, nod: 0.03, roll: 0.025 },
    headFn: (st) => ({ down: 0, yaw: Math.sin(st.tau * 0.3) * 0.25 * (0.3 + 0.7 * st.idle), tilt: Math.sin(st.tau * 0.5) * 0.04 }),
    paint: o.paint,
    extras(b, { HF, paint }) {
      // trunk: a tapering loft over a chain of bones hanging from the face
      const pts = [HF.pt(o.trunk.at[0], o.trunk.at[1])];
      o.trunk.ang.forEach((a) => pts.push(pts[pts.length - 1].clone().add(new THREE.Vector3(Math.cos(a * D), Math.sin(a * D), 0).multiplyScalar(o.trunk.seg))));
      pts.slice(0, nTr).forEach((p, i) => b.bone('tr' + i, i ? 'tr' + (i - 1) : 'head', p));
      b.loft(pts.map((p, i) => ({ p, r: o.trunk.r[i], b: i === 0 ? { head: 0.5, tr0: 0.5 } : 'tr' + Math.min(i, nTr - 1) })), { part: 'trunk', paint, around: 12, cap1: 0.4, ref: [1, 0, 0] });
      // tusks
      if (o.tusks) [1, -1].forEach((s) => {
        const base = HF.pt(o.tusks.at[0], o.tusks.at[1], o.tusks.at[2] * s);
        b.loft(o.tusks.path.map(([x, y, z], i) => ({ p: base.clone().add(new THREE.Vector3(x, y, z * s)), r: o.tusks.r * (1 - (i / (o.tusks.path.length - 1)) * 0.75) })), { b: 'head', part: 'tusk', paint, around: 9, cap1: 0.6 });
      });
      // ears: flat flaps hung from the side of the head, outline given as forward/backward extents down the ear
      [1, -1].forEach((s) => {
        const a = HF.pt(o.ears.at[0], o.ears.at[1], o.ears.at[2] * s);
        const nm = s > 0 ? 'earL' : 'earR';
        b.bone(nm, 'head', a);
        b.loft(o.ears.prof.map(([dy, fw, bk, dz]) => ({ p: [a.x - o.ears.back, a.y + dy, (Math.abs(a.z) + (dz || 0)) * s], r: [o.ears.th, fw, bk] })),
          { b: nm, part: 'ear', side: s, paint, ref: [1, 0, 0], around: 14, cap0: 0.3, cap1: 0.3 });
      });
    },
    anim(B, st) {
      const tau = st.tau, curl = st.idle * sstep(0.55, 0.62, fract(tau / 15)) * (1 - sstep(0.8, 0.87, fract(tau / 15)));
      for (let i = 0; i < nTr; i++) {
        const tb = B['tr' + i];
        tb.rotation.z = Math.sin(tau * 1.1 - i * 0.5) * 0.06 * (1 + st.w) + curl * (i > 3 ? 0.38 : 0.06);
        tb.rotation.x = Math.sin(tau * 0.7 - i * 0.35) * 0.05;
      }
      const flap = Math.sin(tau * (o.flapHz || 1.3)) * 0.5 + 0.5;
      B.earL.rotation.y = -(0.08 + flap * (o.flap ?? 0.35));
      B.earR.rotation.y = 0.08 + flap * (o.flap ?? 0.35);
    },
  });
}
function elephant() {
  const grey = '#8a847d';
  const wr = (q) => mul(q.part === 'ear' ? mix(q.s * q.side < 0 ? '#a58f8a' : '#7f7972', '#5e5852', sstep(0.75, 0.3, Math.abs(q.s))) : grey, 0.9 + 0.16 * fbm(q.p.x * 5, q.p.y * 9, q.p.z * 5));
  return proboscid({
    key: 'african-elephant', hip: [-0.78, 2.05], sh: [0.62, 2.1], ang: -62,
    torso: [[-1.15, 2.55, 2.0, 0.3], [-1.02, 2.82, 1.62, 0.52], [-0.72, 2.92, 1.42, 0.64], [-0.3, 2.82, 1.32, 0.7], [0.12, 2.86, 1.32, 0.7], [0.5, 3.02, 1.42, 0.66], [0.8, 2.94, 1.6, 0.55], [1.0, 2.62, 1.86, 0.4]],
    neck: [[0.72, 2.42, 0.5, 0.55, 0.6], [0.95, 2.48, 0.46, 0.5, 0.52], [1.1, 2.5, 0.43, 0.46, 0.46]],
    head: [[-0.25, 0.42, -0.4, 0.44], [0.0, 0.52, -0.42, 0.47], [0.3, 0.46, -0.36, 0.42], [0.6, 0.3, -0.26, 0.3], [0.8, 0.22, -0.16, 0.21]],
    eye: [0.32, 0.18, 0.36, 0.04],
    legF: { z: 0.38, j: [[0.7, 2.0], [0.6, 1.38], [0.66, 0.46], [0.68, 0.12]], r: [[0.36, 0.3], [0.27, 0.25], [0.23, 0.22], [0.28, 0.27]], mid: [[0.32, 0.27], [0.24, 0.22], null], top: [0.7, 2.3, 0.32, 0.27], cap1: 0.2, toeOff: 0.2, flex: 0.45 },
    legH: { z: 0.38, j: [[-0.8, 2.0], [-0.68, 1.2], [-0.76, 0.44], [-0.74, 0.12]], r: [[0.4, 0.32], [0.28, 0.25], [0.22, 0.21], [0.27, 0.26]], mid: [[0.36, 0.3], [0.24, 0.22], null], top: [-0.82, 2.3, 0.34, 0.28], cap1: 0.2, toeOff: 0.2, flex: 0.45 },
    tail: { pts: [[-1.14, 2.5, 0.05], [-1.2, 2.25, 0.04], [-1.22, 1.95, 0.032], [-1.22, 1.7, 0.03], [-1.21, 1.55, 0.05], [-1.2, 1.45, 0.03]], swish: 0.5 },
    trunk: { at: [0.72, 0.0], seg: 0.25, ang: [-70, -80, -87, -90, -91, -88, -80, -70], r: [0.21, 0.18, 0.155, 0.135, 0.115, 0.1, 0.085, 0.075, 0.07] },
    tusks: { at: [0.62, -0.26, 0.2], r: 0.075, path: [[0, 0, 0], [0.14, -0.28, 0.03], [0.32, -0.42, 0.06], [0.52, -0.4, 0.06], [0.64, -0.3, 0.04]] },
    ears: { at: [0.05, 0.34, 0.5], back: 0.12, th: 0.03, prof: [[0.32, 0.02, 0.15, 0.06], [0.22, 0.1, 0.55, 0.1], [0.0, 0.16, 0.78, 0.15], [-0.35, 0.16, 0.76, 0.2], [-0.7, 0.12, 0.5, 0.2], [-0.95, 0.12, 0.24, 0.17], [-1.15, 0.08, 0.08, 0.12]] },
    paint(q) {
      if (q.part === 'tusk') return '#efe6cf';
      if (q.part === 'tail') return q.u > 0.75 ? '#2a2623' : grey;
      if ((q.part === 'legF' || q.part === 'legH') && q.p.y < 0.2 && q.n.x > 0.3 && q.p.y > 0.05) return Math.sin(Math.atan2(q.p.z, q.n.x) * 9) > 0.2 ? '#d8cfbf' : wr(q);
      if (q.part === 'trunk') return mul(wr(q), 0.92 + 0.1 * Math.sin(q.p.y * 60));
      return wr(q);
    },
  });
}
function mammoth() {
  const hair = (q) => mix('#4a2d17', '#7e5530', fbm(q.p.x * 8, q.p.y * 14, q.p.z * 8) * 0.9);
  const shag = (q) => (fbm(q.p.x * 7, q.p.y * 7, q.p.z * 7) - 0.4) * 0.14 + (q.c < -0.1 ? 0.12 * sstep(-0.1, -0.7, q.c) : 0);
  return proboscid({
    key: 'woolly-mammoth', hip: [-0.72, 1.9], sh: [0.6, 2.05], ang: -66, flap: 0.12,
    torso: [[-1.05, 2.25, 1.75, 0.3], [-0.95, 2.48, 1.45, 0.48], [-0.65, 2.62, 1.25, 0.6], [-0.3, 2.75, 1.15, 0.66], [0.1, 2.95, 1.15, 0.68], [0.42, 3.2, 1.25, 0.64], [0.7, 3.02, 1.42, 0.55], [0.92, 2.65, 1.65, 0.42]],
    torsoDisp: shag,
    neck: [[0.68, 2.45, 0.5, 0.56, 0.62], [0.9, 2.56, 0.47, 0.52, 0.52], [1.04, 2.62, 0.45, 0.47, 0.47]],
    head: [[-0.25, 0.48, -0.4, 0.42], [0.0, 0.64, -0.42, 0.45], [0.3, 0.52, -0.36, 0.4], [0.6, 0.3, -0.26, 0.3], [0.8, 0.22, -0.16, 0.2]],
    eye: [0.34, 0.16, 0.34, 0.035],
    legF: { z: 0.36, j: [[0.68, 1.95], [0.58, 1.32], [0.64, 0.44], [0.66, 0.12]], r: [[0.42, 0.35], [0.33, 0.3], [0.25, 0.24], [0.28, 0.27]], mid: [[0.4, 0.34], [0.28, 0.26], null], top: [0.68, 2.25, 0.36, 0.3], cap1: 0.2, toeOff: 0.2, flex: 0.45 },
    legH: { z: 0.36, j: [[-0.74, 1.88], [-0.62, 1.15], [-0.7, 0.42], [-0.68, 0.12]], r: [[0.44, 0.35], [0.32, 0.29], [0.24, 0.23], [0.27, 0.26]], mid: [[0.4, 0.33], [0.28, 0.25], null], top: [-0.76, 2.2, 0.38, 0.3], cap1: 0.2, toeOff: 0.2, flex: 0.45 },
    tail: { pts: [[-1.03, 2.2, 0.06], [-1.12, 2.05, 0.05], [-1.16, 1.88, 0.06]], swish: 0.4 },
    trunk: { at: [0.72, 0.0], seg: 0.24, ang: [-72, -82, -88, -90, -90, -86, -78, -66], r: [0.22, 0.19, 0.16, 0.14, 0.12, 0.105, 0.09, 0.08, 0.075] },
    tusks: { at: [0.6, -0.26, 0.2], r: 0.09, path: [[0, 0, 0], [0.25, -0.45, 0.15], [0.62, -0.62, 0.4], [1.0, -0.42, 0.58], [1.22, -0.05, 0.5], [1.18, 0.3, 0.28]] },
    ears: { at: [0.02, 0.3, 0.42], back: 0.05, th: 0.05, prof: [[0.12, 0.05, 0.1, 0.02], [0.0, 0.12, 0.18, 0.04], [-0.18, 0.1, 0.16, 0.04], [-0.3, 0.04, 0.06, 0.02]] },
    paint(q) {
      if (q.part === 'tusk') return mix('#efe4c8', '#c9b48e', q.u * 0.6);
      if (q.part === 'trunk') return mul(hair(q), 0.85);
      if (q.part === 'head') return mix(hair(q), '#3a2312', sstep(0.2, 0.7, q.hl[0]) * 0.5);
      if (q.part === 'torso') return mix(hair(q), '#3c2412', sstep(-0.3, -0.85, q.c) * 0.6);
      if ((q.part === 'legF' || q.part === 'legH') && q.p.y < 0.13) return '#2e2018';
      return hair(q);
    },
  });
}

// ---------------------------------------------------------------- bears & apes (plantigrade)
function bear(o) {
  const ear = { at: [0.0, 0.15 * o.hk, 0.13 * o.hk], dir: [-0.3, 1, 0.6], len: 0.055 * o.hk, w: 0.055 * o.hk, th: 0.032 * o.hk, prof: [[0, 0.8], [0.5, 1], [1, 0.75]], cap: 1.4 };
  return buildQuad({
    key: o.key, hip: o.hip, sh: o.sh, torso: o.torso, torsoSq: [2.3, 2.1], torsoDisp: o.fur, neck: o.neck, neckDisp: o.fur,
    head: { ang: o.ang, st: o.head, eye: o.eye, cap1: 0.5 },
    legF: { z: o.z, j: o.legF, r: o.rF, mid: o.midF, top: o.topF, toeOff: 0.55, flex: 0.9, follow: 0.6 },
    legH: { z: o.z, j: o.legH, r: o.rH, mid: o.midH, top: o.topH, toeOff: 0.6, flex: 0.8, follow: 0.8 },
    tail: o.tail, ears: o.ears || ear,
    gait: { kind: 'walk', hz: o.hz ?? 0.65, stride: o.stride ?? 0.45, lift: 0.09, duty: 0.62, bob: 0.012, pitch: 0.012, nod: 0.06, roll: 0.03 },
    headFn: o.headFn || ((st) => lookHead(st, 0.7, 0.7)), grazeW: [0.6, 0.4],
    paint: o.paint, pat: o.pat, extras: o.extras, anim: o.anim,
  });
}
const BEAR_RF = [[0.18, 0.14], [0.13, 0.12], [0.085, 0.09], [0.055, 0.1]];
const BEAR_MF = [[0.17, 0.13], [0.11, 0.1], [0.07, 0.105, null, 0.5]];
const BEAR_RH = [[0.2, 0.15], [0.13, 0.12], [0.085, 0.085], [0.055, 0.095]];
const BEAR_MH = [[0.19, 0.14], [0.11, 0.1], [0.06, 0.1, null, 0.5]];
const clawed = (q, fur) => ((q.part === 'legF' || q.part === 'legH') && q.p.y < 0.025 ? '#2a2420' : fur);
function brownBear() {
  const fur = (q) => (fbm(q.p.x * 14, q.p.y * 14, q.p.z * 14) - 0.5) * 0.035;
  return bear({
    key: 'brown-bear', hip: [-0.42, 0.84], sh: [0.36, 0.84], z: 0.2, hk: 1, ang: -22, fur,
    torso: [[-0.66, 1.04, 0.66, 0.16], [-0.58, 1.13, 0.54, 0.27], [-0.4, 1.16, 0.48, 0.33], [-0.13, 1.13, 0.45, 0.35], [0.13, 1.17, 0.45, 0.35], [0.33, 1.29, 0.48, 0.33], [0.49, 1.18, 0.56, 0.27], [0.6, 1.0, 0.66, 0.18]],
    neck: [[0.4, 0.94, 0.2, 0.25, 0.27], [0.6, 0.99, 0.18, 0.2, 0.22], [0.75, 1.0, 0.16, 0.16, 0.18]],
    head: [[-0.1, 0.14, -0.14, 0.16], [0.04, 0.18, -0.15, 0.19], [0.17, 0.16, -0.14, 0.18], [0.27, 0.1, -0.11, 0.11], [0.38, 0.075, -0.09, 0.08], [0.45, 0.06, -0.065, 0.06], [0.48, 0.03, -0.03, 0.04]],
    eye: [0.21, 0.075, 0.115, 0.018],
    legF: [[0.41, 0.86], [0.32, 0.52], [0.38, 0.12], [0.54, 0.05]], rF: BEAR_RF, midF: BEAR_MF, topF: [0.4, 0.94, 0.16, 0.12],
    legH: [[-0.44, 0.86], [-0.26, 0.52], [-0.4, 0.12], [-0.18, 0.05]], rH: BEAR_RH, midH: BEAR_MH, topH: [-0.46, 1.0, 0.18, 0.13],
    tail: { pts: [[-0.65, 1.0, 0.05], [-0.7, 0.96, 0.04], [-0.72, 0.92, 0.02]], swish: 0.1 },
    paint(q) {
      const g = fbm(q.p.x * 10, q.p.y * 10, q.p.z * 10);
      if (q.part === 'head') { const [x, y] = q.hl; if (x > 0.44 && y > -0.01) return '#171310'; if (x > 0.3) return '#8a6646'; return mix('#6a4529', '#7c5535', g); }
      if (q.part === 'ear') return '#4a3020';
      if (q.part === 'legF' || q.part === 'legH') return clawed(q, mix(mix('#3a2516', '#4e3320', g), '#6a4529', sstep(0.5, 0.85, q.p.y)));
      return mix(furCounter(q, mix('#6e4829', '#9a7448', sstep(1.05, 1.25, q.p.y) * 0.8), '#4d321e', -0.8, 0.2), '#a58258', sstep(0.6, 0.9, g) * 0.3);
    },
  });
}
function polarBear() {
  const fur = (q) => (fbm(q.p.x * 14, q.p.y * 14, q.p.z * 14) - 0.5) * 0.03;
  return bear({
    key: 'polar-bear', hip: [-0.48, 0.8], sh: [0.42, 0.78], z: 0.17, hk: 0.75, ang: -16, fur, stride: 0.45,
    torso: [[-0.74, 1.0, 0.62, 0.16], [-0.65, 1.1, 0.5, 0.28], [-0.43, 1.15, 0.44, 0.34], [-0.12, 1.13, 0.42, 0.35], [0.18, 1.1, 0.44, 0.34], [0.4, 1.08, 0.48, 0.3], [0.55, 1.0, 0.56, 0.25], [0.65, 0.88, 0.64, 0.18]],
    neck: [[0.44, 0.88, 0.22, 0.24, 0.27], [0.68, 0.93, 0.17, 0.19, 0.2], [0.88, 0.93, 0.135, 0.145, 0.155]],
    head: [[-0.08, 0.12, -0.12, 0.12], [0.06, 0.14, -0.12, 0.14], [0.18, 0.12, -0.11, 0.12], [0.3, 0.085, -0.09, 0.085], [0.4, 0.075, -0.075, 0.07], [0.46, 0.05, -0.05, 0.05]],
    eye: [0.2, 0.06, 0.1, 0.016],
    legF: [[0.46, 0.82], [0.37, 0.5], [0.44, 0.12], [0.61, 0.05]], rF: BEAR_RF, midF: BEAR_MF, topF: [0.45, 0.9, 0.15, 0.11],
    legH: [[-0.5, 0.82], [-0.33, 0.5], [-0.46, 0.12], [-0.23, 0.05]], rH: BEAR_RH, midH: BEAR_MH, topH: [-0.51, 0.92, 0.17, 0.12],
    tail: { pts: [[-0.73, 0.98, 0.045], [-0.77, 0.94, 0.035], [-0.79, 0.9, 0.02]], swish: 0.1 },
    paint(q) {
      const cream = mix('#f4efe2', '#e8dcc0', fbm(q.p.x * 8, q.p.y * 8, q.p.z * 8) * 0.8);
      if (q.part === 'head') { const [x, y] = q.hl; if (x > 0.43 && y > -0.01) return '#141414'; if (x > 0.38 && y < -0.03) return '#3a3836'; return cream; }
      if (q.part === 'legF' || q.part === 'legH') return clawed(q, mul(cream, 0.96));
      return furCounter(q, cream, mul(cream, 0.88), -0.8, 0.1);
    },
  });
}
function panda() {
  const B = '#161616', W = '#f2f0ea';
  const fur = (q) => (fbm(q.p.x * 16, q.p.y * 16, q.p.z * 16) - 0.5) * 0.025;
  return bear({
    key: 'giant-panda', hip: [-0.38, 0.55], sh: [0.32, 0.55], z: 0.17, hk: 1.15, ang: -12, fur, hz: 0.6, stride: 0.32,
    torso: [[-0.6, 0.78, 0.4, 0.16], [-0.52, 0.86, 0.3, 0.28], [-0.32, 0.9, 0.26, 0.33], [-0.05, 0.9, 0.25, 0.34], [0.2, 0.9, 0.26, 0.33], [0.4, 0.88, 0.3, 0.29], [0.52, 0.8, 0.36, 0.24], [0.6, 0.68, 0.44, 0.16]],
    neck: [[0.38, 0.66, 0.2, 0.22, 0.24], [0.54, 0.71, 0.19, 0.2, 0.21], [0.66, 0.73, 0.18, 0.19, 0.19]],
    head: [[-0.1, 0.17, -0.17, 0.2], [0.04, 0.22, -0.2, 0.24], [0.17, 0.2, -0.19, 0.24], [0.28, 0.12, -0.15, 0.15], [0.36, 0.075, -0.1, 0.095], [0.41, 0.04, -0.05, 0.05]],
    eye: [0.25, 0.07, 0.148, 0.02],
    legF: [[0.36, 0.56], [0.3, 0.34], [0.34, 0.09], [0.47, 0.04]], rF: BEAR_RF.map(([a, b]) => [a * 0.85, b * 0.85]), midF: BEAR_MF.map(([a, b, c, t]) => [a * 0.85, b * 0.85, c, t]), topF: [0.36, 0.72, 0.17, 0.15],
    legH: [[-0.4, 0.58], [-0.27, 0.34], [-0.37, 0.09], [-0.21, 0.04]], rH: BEAR_RH.map(([a, b]) => [a * 0.85, b * 0.85]), midH: BEAR_MH.map(([a, b, c, t]) => [a * 0.85, b * 0.85, c, t]), topH: [-0.42, 0.74, 0.17, 0.14],
    tail: { pts: [[-0.6, 0.74, 0.05], [-0.65, 0.7, 0.04], [-0.67, 0.66, 0.02]], swish: 0.1 },
    paint(q) {
      if (q.part === 'ear') return B;
      if (q.part === 'legH') return q.p.y > 0.66 && q.p.x < -0.36 ? W : B;
      if (q.part === 'legF') return B;
      if (q.part === 'head') {
        const [x, y] = q.hl, z = Math.abs(q.p.z);
        if (x > 0.38 && y > -0.02) return B;
        const ex = (x - 0.245) / 0.06, ey = (y - 0.055 + (x - 0.245) * 0.5) / 0.045;
        if (z > 0.08 && ex * ex + ey * ey < 1) return B;
        return W;
      }
      return W;
    },
    pat: (part) => (part === 'torso' || part === 'neck' ? {
      mode: PAT.mark, ink: B, scale: 1,
      coord: (q) => {
        const x = q.p.x, y = q.p.y;
        const band = Math.abs(x - 0.25 - (y - 0.6) * 0.15) - (0.1 + 0.06 * sstep(0.85, 0.55, y));
        const chest = Math.max(0.12 - x, y - 0.6), hips = Math.max(x + 0.22, y - 0.58 + q.n.y * 0.05);
        return Math.min(band, chest, hips);
      },
    } : null),
  });
}

/** Great apes: knuckle-walking quadruped rig with long arms and plantigrade feet. */
function greatApe(o) {
  return buildQuad({
    key: o.key, hip: o.hip, sh: o.sh, torso: o.torso, torsoSq: [2.2, 2.0], torsoDisp: o.fur, neck: o.neck, neckDisp: o.fur,
    head: { ang: o.ang ?? -5, st: o.head, eye: o.eye, cap1: 0.4, cap0: 0.9 },
    legF: { z: o.zf, j: o.arm, r: o.rA, mid: o.mA, top: o.topA, toeOff: 0.2, flex: 0.6, follow: 0.5 },
    legH: { z: o.zh, j: o.leg, r: o.rL, mid: o.mL, top: o.topL, toeOff: 0.6, flex: 0.8, follow: 0.8 },
    ears: o.ears,
    gait: { kind: 'walk', hz: 0.6, stride: o.stride ?? 0.36, lift: 0.08, duty: 0.62, bob: 0.015, pitch: 0.02, nod: 0.04, roll: 0.04 },
    headFn: (st) => lookHead(st, 1.0, 0.25), grazeW: [0.6, 0.4],
    paint: o.paint, extras: o.extras,
  });
}
function gorilla() {
  const fur = (q) => (fbm(q.p.x * 16, q.p.y * 16, q.p.z * 16) - 0.5) * 0.03;
  return greatApe({
    key: 'gorilla', hip: [-0.28, 0.74], sh: [0.32, 1.12], fur, zf: 0.31, zh: 0.19, ang: -8,
    torso: [[-0.48, 0.98, 0.6, 0.2], [-0.4, 1.07, 0.52, 0.27], [-0.22, 1.16, 0.53, 0.32], [0.0, 1.29, 0.6, 0.36], [0.2, 1.42, 0.72, 0.38], [0.36, 1.46, 0.86, 0.34], [0.46, 1.4, 0.98, 0.26], [0.52, 1.28, 1.06, 0.16]],
    neck: [[0.36, 1.28, 0.22, 0.22, 0.24], [0.54, 1.34, 0.19, 0.19, 0.2]],
    head: [[-0.12, 0.15, -0.15, 0.17], [0.0, 0.29, -0.18, 0.2], [0.13, 0.22, -0.2, 0.2], [0.23, 0.14, -0.2, 0.19], [0.31, 0.06, -0.17, 0.16], [0.35, 0.0, -0.1, 0.1]],
    eye: [0.25, 0.06, 0.1, 0.02],
    arm: [[0.38, 1.2], [0.26, 0.7], [0.4, 0.22], [0.44, 0.05]], rA: [[0.14, 0.13], [0.11, 0.1], [0.08, 0.08], [0.07, 0.085]], mA: [[0.15, 0.14], [0.13, 0.12, null, 0.35], [0.075, 0.09]], topA: [0.38, 1.34, 0.15, 0.14],
    leg: [[-0.3, 0.74], [-0.1, 0.46], [-0.22, 0.1], [-0.02, 0.04]], rL: [[0.15, 0.13], [0.11, 0.1], [0.07, 0.08], [0.05, 0.09]], mL: [[0.15, 0.13], [0.1, 0.09], [0.06, 0.1, null, 0.5]], topL: [-0.32, 0.9, 0.15, 0.13],
    ears: { at: [0.06, 0.04, 0.14], dir: [-0.3, 0.3, 1], len: 0.04, w: 0.03, th: 0.012, cap: 1 },
    paint(q) {
      const black = mix('#1c1b1a', '#2c2a28', fbm(q.p.x * 12, q.p.y * 12, q.p.z * 12));
      if (q.part === 'head') { const [x, y] = q.hl; if (x > 0.18 && y < 0.09) return '#101012'; return black; }
      if ((q.part === 'legF' || q.part === 'legH') && q.p.y < 0.12) return '#141414';
      if (q.part === 'torso') { const saddle = sstep(-0.1, 0.5, q.n.y) * sstep(0.25, -0.05, q.p.x) * sstep(-0.5, -0.38, q.p.x); return mix(black, '#a9a6a1', saddle * 0.85); }
      return black;
    },
  });
}
function orangutan() {
  const hair = (q) => (fbm(q.p.x * 12, q.p.y * 12, q.p.z * 12) - 0.45) * 0.06 + (q.n && q.n.y < -0.2 ? 0.03 : 0);
  return greatApe({
    key: 'orangutan', hip: [-0.25, 0.56], sh: [0.28, 0.96], fur: hair, zf: 0.27, zh: 0.17, ang: 0, stride: 0.3,
    torso: [[-0.42, 0.8, 0.42, 0.2], [-0.32, 0.9, 0.36, 0.27], [-0.12, 1.0, 0.36, 0.3], [0.08, 1.1, 0.42, 0.3], [0.24, 1.18, 0.56, 0.3], [0.36, 1.2, 0.72, 0.26], [0.44, 1.12, 0.86, 0.18]],
    neck: [[0.32, 1.08, 0.18, 0.18, 0.22], [0.44, 1.12, 0.16, 0.16, 0.2]],
    head: [[-0.08, 0.12, -0.12, 0.12], [0.04, 0.16, -0.15, 0.14], [0.14, 0.13, -0.16, 0.14], [0.2, 0.06, -0.14, 0.12], [0.25, 0.0, -0.1, 0.09]],
    eye: [0.2, 0.04, 0.06, 0.014],
    arm: [[0.32, 1.02], [0.14, 0.56], [0.36, 0.16], [0.4, 0.04]], rA: [[0.11, 0.1], [0.09, 0.085], [0.06, 0.06], [0.05, 0.065]], mA: [[0.14, 0.12, 0.17], [0.11, 0.1, 0.15, 0.4], [0.055, 0.07]], topA: [0.32, 1.12, 0.12, 0.11],
    leg: [[-0.27, 0.56], [-0.06, 0.36], [-0.2, 0.08], [-0.02, 0.035]], rL: [[0.12, 0.11], [0.09, 0.085], [0.06, 0.07], [0.04, 0.075]], mL: [[0.13, 0.12], [0.09, 0.085], [0.05, 0.08, null, 0.5]], topL: [-0.29, 0.7, 0.13, 0.12],
    paint(q) {
      const rust = mix('#9c4318', '#c4632a', fbm(q.p.x * 10, q.p.y * 18, q.p.z * 10));
      if (q.part === 'flange') return mix('#5b3b2b', '#3d281e', sstep(0.2, 0.8, Math.abs(q.n.z)));
      if (q.part === 'head') { const [x, y] = q.hl; if (x > 0.12 && y < 0.1) return '#4a3226'; return rust; }
      if ((q.part === 'legF' || q.part === 'legH') && q.p.y < 0.1) return '#3d2a20';
      return rust;
    },
    extras(b, { HF, paint }) {
      // cheek flanges and throat sac of an adult male
      b.ell(HF.pt(0.15, -0.03, 0), [0.04, 0.16, 0.25], 'head', paint, { part: 'flange', seg: 16 });
      b.ell(HF.pt(0.06, -0.17, 0), [0.12, 0.1, 0.13], 'head', '#8a3a16', { part: 'sac' });
    },
  });
}


// ================================================================ birds
/** Two-legged walker (ostrich, dodo): body loft, S-neck, head, IK legs with backward-bending ankles. */
function buildBiped(S) {
  const b = new Body(S.key);
  b.bone('root', null, [0, 0, 0]);
  const hip = S.hip;
  b.bone('body', 'root', [hip[0], hip[1], 0]);
  const nk = S.neck, nn = nk.length, poll = nk[nn - 1];
  const HF = frame2(poll[0], poll[1], S.head.ang);
  const paint = (q) => { q.hl = HF.local(q.p); return S.paint(q); };
  b.loft(S.torso.map(([x, top, bot, w, k = 0.5]) => { const c = bot + (top - bot) * k; return { p: [x, c, 0], r: [w, top - c, c - bot], b: 'body' }; }),
    { axis: [1, 0, 0], around: 16, part: 'torso', paint, cap0: S.rumpCap ?? 0.9, cap1: 0.9, ao: 0.2, disp: S.torsoDisp, step: S.torsoStep });
  for (let i = 0; i < nn - 1; i++) b.bone('n' + i, i ? 'n' + (i - 1) : 'body', [nk[i][0], nk[i][1], 0]);
  b.bone('head', 'n' + (nn - 2), HF.o);
  b.loft(nk.map(([x, y, w, up, dn], i) => ({ p: [x, y, 0], r: [w, up, dn ?? up], b: i === 0 ? { body: 0.5, n0: 0.5 } : i === nn - 1 ? 'head' : 'n' + i })),
    { ref: [-1, 0.2, 0], around: 10, part: 'neck', paint, cap0: 0.6, cap1: 0.6, disp: S.neckDisp });
  b.loft(S.head.st.map(([x, top, bot, w, k = 0.5]) => { const c = bot + (top - bot) * k; return { p: HF.pt(x, c), r: [w, top - c, c - bot], b: 'head' }; }),
    { axis: HF.d, ref: HF.u, around: 12, part: 'head', paint, cap0: 0.8, cap1: S.head.cap1 ?? 0.5 });
  if (S.head.beak) b.loft(S.head.beak.map(([x, top, bot, w]) => ({ p: HF.pt(x, (top + bot) / 2), r: [w, (top - bot) / 2, (top - bot) / 2], b: 'head' })), { ref: HF.u, around: 10, part: 'beak', paint, cap0: 0.5, cap1: 0.4 });
  if (S.head.eye) { const [ex, ey, ez, er] = S.head.eye; [1, -1].forEach((s) => b.eye(HF.pt(ex, ey, ez * s), er, 'head', S.head.iris || '#2a1a0a')); }
  const legs = [];
  [['l', 1, 0], ['r', -1, 0.5]].forEach(([nm, side, ph]) => { const L = buildLeg(b, nm, 'body', S.leg, side, { paint, part: 'leg' }); L.ph = ph; legs.push(L); });
  if (S.extras) S.extras(b, { HF, paint });
  const R = b.finish(), B = R.B, g = S.gait, ofs = nextOffset();
  const neck = [];
  for (let i = 0; i < nn - 1; i++) neck.push(B['n' + i]);
  const rest = B.body.position.clone(), st = { w: 0, s: 0, tau: 0, idle: 0 };
  return {
    root: R.root,
    update(t) { this.pose(t + ofs); },
    /** Pose at absolute animation time `tau` (tau = 0 is a neutral standing pose). */
    pose(tau) {
      const w = walkEnvelope(tau, g.per || 15, g.walk || 9), s = fract(tau * g.hz);
      st.w = w; st.s = s; st.tau = tau; st.idle = 1 - w;
      const bob = g.bob * w * Math.cos(TAU * 2 * s), roll = (g.roll || 0) * w * Math.sin(TAU * s), pitch = (g.pitch || 0) + (S.idlePitch ? S.idlePitch(st) : 0);
      B.body.position.set(rest.x, rest.y + bob, 0);
      B.body.rotation.set(roll, Math.sin(TAU * s) * (g.yaw || 0) * w, pitch);
      for (const L of legs) {
        const [wx, wy] = rot2(L.j0[0] - rest.x, L.j0[1] - rest.y, pitch);
        poseLeg(L, rest.x + wx, rest.y + bob + wy, pitch, s, w, g);
      }
      const h = S.headFn ? S.headFn(st) : lookHead(st, 0.8, 0.4);
      // birds hold the head steady, then thrust it forward with each step
      const thrust = w * (g.thrust || 0) * (fract(s * 2) < 0.7 ? -fract(s * 2) / 0.7 : (fract(s * 2) - 0.7) / 0.3 - 1);
      neck.forEach((nb, i) => { nb.rotation.z = (thrust + (h.down || 0)) * (S.neckW ? S.neckW[i] : 1 / neck.length) - pitch / neck.length; nb.rotation.y = (h.yaw || 0) / (neck.length + 1); });
      B.head.rotation.set(0, (h.yaw || 0) / (neck.length + 1), (h.lift || 0) - thrust * 0.5 + (h.tilt || 0));
      if (S.anim) S.anim(B, st);
    },
  };
}

function ostrich() {
  const fluff = (q) => (fbm(q.p.x * 10, q.p.y * 10, q.p.z * 10) - 0.45) * 0.08;
  const skin = '#d9a49a';
  return buildBiped({
    key: 'ostrich', hip: [0.02, 1.25],
    torso: [[-0.52, 1.46, 1.24, 0.14], [-0.4, 1.57, 1.06, 0.28], [-0.15, 1.62, 0.93, 0.36], [0.15, 1.6, 0.92, 0.36], [0.35, 1.52, 0.96, 0.3], [0.48, 1.4, 1.06, 0.2], [0.55, 1.3, 1.13, 0.12]],
    torsoDisp: fluff,
    neck: [[0.42, 1.36, 0.1, 0.11, 0.12], [0.52, 1.62, 0.06, 0.065, 0.07], [0.56, 1.92, 0.045, 0.048, 0.05], [0.56, 2.16, 0.04, 0.042, 0.042], [0.59, 2.34, 0.04, 0.042, 0.042]],
    head: { ang: -4, st: [[-0.05, 0.05, -0.04, 0.045], [0.02, 0.066, -0.05, 0.062], [0.08, 0.05, -0.04, 0.05], [0.12, 0.03, -0.03, 0.04]], cap1: 0.6, beak: [[0.1, 0.02, -0.035, 0.035], [0.16, 0.015, -0.03, 0.035], [0.21, 0.008, -0.016, 0.026]], eye: [0.035, 0.022, 0.052, 0.02] },
    leg: { z: 0.13, j: [[0.0, 1.22], [0.16, 1.02], [-0.07, 0.56], [0.02, 0.05], [0.19, 0.03]], r: [[0.14, 0.12], [0.1, 0.08], [0.04, 0.035], [0.03, 0.04], [0.022, 0.032]], mid: [[0.14, 0.11], [0.065, 0.055, null, 0.3], [0.026, 0.024], null], toeOff: 0.7, flex: 1.0, cflex: 0.3, follow: 0.8 },
    gait: { hz: 0.85, stride: 0.75, lift: 0.2, duty: 0.58, bob: 0.025, roll: 0.03, yaw: 0.04, thrust: 0.18, pitch: 0 },
    neckW: [0.15, 0.25, 0.3, 0.3],
    headFn: (st) => { const h = lookHead(st, 0.9, 0.9); return { ...h, down: h.down * 1.6 }; },
    paint(q) {
      if (q.part === 'leg') return q.p.y > 1.0 ? mix(skin, '#1b1a1a', sstep(1.05, 1.15, q.p.y)) : q.p.y < 0.06 ? '#8a5e52' : skin;
      if (q.part === 'neck') return q.p.y < 1.48 ? mix('#f3efe6', '#1b1a1a', sstep(1.43, 1.38, q.p.y)) : mix('#c99a92', '#d8b0a6', fbm(q.p.y * 20, 0, 0));
      if (q.part === 'head') return '#c39b90';
      if (q.part === 'beak') return '#d5b28c';
      if (q.part === 'plume') return '#f5f3ee';
      return mix('#161515', '#2d2a28', fbm(q.p.x * 9, q.p.y * 9, q.p.z * 9));
    },
    extras(b, { paint }) {
      // white wing plumes drooping over the flanks and a white tail plume
      [1, -1].forEach((s) => b.loft([[0.25, 1.42, 0.3], [0.0, 1.36, 0.36], [-0.3, 1.3, 0.37], [-0.48, 1.26, 0.3], [-0.58, 1.28, 0.2]].map(([x, y, z]) => ({ p: [x, y, z * s], r: [0.04, 0.1, 0.1] })),
        { b: 'body', part: 'plume', paint, ref: [0, 1, 0], around: 10, disp: (q) => (fbm(q.p.x * 14, q.p.y * 14, q.p.z * 14) - 0.45) * 0.08 }));
      b.ell([-0.58, 1.42, 0], [0.16, 0.13, 0.17], 'body', paint, { part: 'plume', rot: [0, 0, 0.5] });
    },
  });
}

function dodo() {
  const grey = '#8b8a86';
  return buildBiped({
    key: 'dodo', hip: [-0.02, 0.36],
    torso: [[-0.36, 0.56, 0.36, 0.14], [-0.28, 0.66, 0.24, 0.26], [-0.1, 0.71, 0.18, 0.31], [0.1, 0.71, 0.2, 0.3], [0.25, 0.66, 0.26, 0.25], [0.34, 0.58, 0.34, 0.16]],
    torsoDisp: (q) => (fbm(q.p.x * 14, q.p.y * 14, q.p.z * 14) - 0.45) * 0.03,
    neck: [[0.24, 0.56, 0.12, 0.13, 0.14], [0.31, 0.66, 0.1, 0.1, 0.11], [0.34, 0.76, 0.095, 0.1, 0.1]],
    head: { ang: -18, st: [[-0.08, 0.08, -0.08, 0.09], [0.0, 0.11, -0.09, 0.1], [0.07, 0.09, -0.08, 0.085], [0.12, 0.06, -0.06, 0.06]], cap1: 0.6,
      beak: [[0.1, 0.06, -0.06, 0.05], [0.18, 0.06, -0.05, 0.045], [0.26, 0.055, -0.045, 0.042], [0.32, 0.03, -0.06, 0.035], [0.34, -0.03, -0.07, 0.02]], eye: [0.04, 0.035, 0.078, 0.016] },
    leg: { z: 0.1, j: [[0.0, 0.36], [0.06, 0.28], [-0.03, 0.17], [0.0, 0.035], [0.08, 0.02]], r: [[0.08, 0.07], [0.06, 0.05], [0.032, 0.03], [0.024, 0.032], [0.018, 0.03]], mid: [[0.08, 0.065], [0.045, 0.04], [0.026, 0.024], null], toeOff: 0.6, flex: 0.8, cflex: 0.2, follow: 0.8 },
    gait: { hz: 1.1, stride: 0.2, lift: 0.07, duty: 0.6, bob: 0.008, roll: 0.07, yaw: 0.06, thrust: 0.1, pitch: 0 },
    neckW: [0.4, 0.6],
    headFn: (st) => { const h = lookHead(st, 1.0, 0.8); return { ...h, down: h.down * 1.3 }; },
    paint(q) {
      if (q.part === 'leg') return q.p.y > 0.24 ? grey : '#d8b23c';
      if (q.part === 'head') return q.hl[0] > 0.04 && q.hl[1] < 0.06 ? '#a39a7e' : '#7d7c78';
      if (q.part === 'beak') { const x = q.hl[0]; return x > 0.28 ? '#2f3a2a' : x > 0.2 ? '#a9a85a' : '#c9c472'; }
      if (q.part === 'tuft') return '#efeadc';
      if (q.part === 'wing') return mix('#6c6b67', '#9a958a', sstep(0.2, 1, q.u));
      return furCounter(q, grey, '#a9a6a0', -0.8, 0.0);
    },
    extras(b, { paint }) {
      [1, -1].forEach((s) => b.loft([[0.1, 0.6, 0.27, 0.05], [0.0, 0.59, 0.305, 0.07], [-0.1, 0.56, 0.305, 0.065], [-0.18, 0.52, 0.28, 0.04], [-0.23, 0.49, 0.24, 0.015]].map(([x, y, z, r]) => ({ p: [x, y, z * s], r: [0.022, r, r * 0.8] })), { b: 'body', part: 'wing', paint, ref: [0, 1, 0], around: 10, cap1: 0.8 }));
      for (let k = 0; k < 5; k++) { const a = -0.6 + k * 0.3; b.loft([{ p: [-0.32, 0.6, 0], r: 0.03 }, { p: [-0.42, 0.68 + Math.cos(a) * 0.04, Math.sin(a) * 0.08], r: 0.035 }, { p: [-0.44, 0.78, Math.sin(a) * 0.12], r: 0.015 }], { b: 'body', part: 'tuft', paint, around: 7 }); }
    },
  });
}

function penguin() {
  const b = new Body('emperor-penguin');
  b.bone('root', null, [0, 0, 0]);
  b.bone('body', 'root', [0, 0.12, 0]);
  b.bone('chest', 'body', [0, 0.7, 0]);
  b.bone('head', 'chest', [0.04, 1.08, 0]);
  const paint = (q) => {
    if (q.part === 'beak') return q.u > 0.35 && q.c < -0.2 ? '#e8823a' : '#1c1d22';
    if (q.part === 'foot') return '#2a2a2c';
    if (q.part === 'flipper') return q.s * q.side > 0.2 ? '#191b21' : mix('#f2f0ea', '#1a1c22', sstep(0.75, 0.95, q.u));
    if (q.part === 'patch') return mix('#f6b52c', '#f9dc8a', sstep(1.05, 0.95, q.p.y));
    if (q.part === 'head') return '#121318';
    return mix(mix('#f4f2ec', '#f8e2a0', sstep(0.72, 0.97, q.p.y)), '#e9e6de', sstep(0.4, 0.1, q.p.y));
  };
  // upright body: a loft up the spine from tail to the neck
  const st = [[0.0, 0.06, 0.08, 0.1, 0.12], [-0.02, 0.18, 0.24, 0.27, 0.3], [-0.02, 0.4, 0.28, 0.27, 0.32], [0.0, 0.65, 0.26, 0.24, 0.27], [0.02, 0.85, 0.2, 0.19, 0.2], [0.03, 1.0, 0.14, 0.14, 0.14], [0.04, 1.08, 0.12, 0.12, 0.12]];
  // white front as a crisp marking: ink (black) everywhere except a shield on the chest and belly
  const shield = (q) => Math.min((q.n.x - 0.18 + sstep(0.35, 0.1, q.p.y) * 0.5) * 0.25, 1.02 - q.p.y);
  b.loft(st.map(([x, y, w, back, front], i) => ({ p: [x, y, 0], r: [w, back, front], b: y < 0.5 ? 'body' : y < 0.95 ? { body: 0.3, chest: 0.7 } : 'chest' })),
    { ref: [-1, 0, 0], around: 18, part: 'body', paint, cap0: 0.4, cap1: 0.4, step: 0.05, pat: { mode: PAT.mark, ink: '#15171d', scale: 1, coord: shield } });
  b.loft([[0.0, 1.06, 0.1], [0.04, 1.16, 0.12], [0.08, 1.24, 0.11], [0.14, 1.27, 0.085]].map(([x, y, r]) => ({ p: [x, y, 0], r: [r, r, r * 0.95] })), { b: 'head', ref: [0, 1, 0], around: 12, part: 'head', paint, cap0: 0.5, cap1: 0.9 });
  b.loft([{ p: [0.2, 1.25, 0], r: [0.03, 0.025, 0.03] }, { p: [0.29, 1.23, 0], r: [0.02, 0.016, 0.02] }, { p: [0.36, 1.19, 0], r: [0.008, 0.006, 0.008] }], { b: 'head', ref: [0, 1, 0], around: 8, part: 'beak', paint });
  [1, -1].forEach((s) => {
    b.ell([0.08, 1.12, 0.085 * s], [0.04, 0.07, 0.035], 'head', paint, { part: 'patch', rot: [0, 0, -0.4] });
    b.eye([0.15, 1.25, 0.07 * s], 0.014, 'head');
    b.bone(s > 0 ? 'fl' : 'fr', 'chest', [0.0, 0.88, 0.22 * s]);
    b.loft([{ p: [0.0, 0.9, 0.22 * s], r: [0.025, 0.065, 0.055] }, { p: [-0.02, 0.68, 0.27 * s], r: [0.02, 0.065, 0.045] }, { p: [-0.04, 0.48, 0.265 * s], r: [0.012, 0.03, 0.02] }],
      { b: s > 0 ? 'fl' : 'fr', ref: [1, 0, 0], around: 10, part: 'flipper', side: s, paint });
    b.bone(s > 0 ? 'ftl' : 'ftr', 'body', [0.04, 0.06, 0.1 * s]);
    b.loft([{ p: [0.0, 0.05, 0.1 * s], r: [0.05, 0.025, 0.025] }, { p: [0.1, 0.025, 0.11 * s], r: [0.055, 0.016, 0.016] }, { p: [0.15, 0.02, 0.115 * s], r: [0.04, 0.012, 0.012] }], { b: s > 0 ? 'ftl' : 'ftr', ref: [0, 1, 0], around: 8, part: 'foot', paint });
  });
  const R = b.finish(), B = R.B, ofs = nextOffset();
  return {
    root: R.root,
    update(t) { this.pose(t + ofs); },
    /** Pose at absolute animation time `tau` (tau = 0 is a neutral standing pose). */
    pose(tau) {
      const w = walkEnvelope(tau, 14, 8), s = TAU * tau * 1.6;
      B.body.rotation.set(Math.sin(s) * 0.12 * w, Math.sin(s) * 0.06 * w, Math.sin(tau * 0.4) * 0.03 + 0.02 * w);
      B.body.position.y = 0.12 + Math.abs(Math.sin(s)) * 0.015 * w;
      B.ftl.position.y = Math.max(0, Math.sin(s)) * 0.035 * w; B.ftr.position.y = Math.max(0, -Math.sin(s)) * 0.035 * w;
      B.ftl.rotation.y = B.ftr.rotation.y = -Math.sin(s) * 0.15 * w;
      const flap = (1 - w) * twitch(tau, 0.11, 0.4, 0.12);
      B.fl.rotation.x = -(0.2 + 0.25 * w + Math.sin(s) * 0.08 * w + flap * 0.9);
      B.fr.rotation.x = 0.2 + 0.25 * w - Math.sin(s) * 0.08 * w + flap * 0.9;
      const look = Math.sin(tau * 0.5) * 0.6 + Math.sin(tau * 1.3) * 0.2;
      B.head.rotation.set(0, look * (0.3 + 0.7 * (1 - w)), Math.sin(tau * 0.7) * 0.08 - (1 - w) * twitch(tau, 0.07, 0.2, 0.2) * 0.6);
      B.chest.rotation.z = (1 - w) * twitch(tau, 0.07, 0.2, 0.2) * -0.25;
    },
  };
}

function baldEagle() {
  const b = new Body('bald-eagle');
  b.bone('root', null, [0, 0, 0]);
  b.bone('body', 'root', [0, 0.3, 0]);
  b.bone('head', 'body', [0.42, 0.33, 0]);
  b.bone('tail', 'body', [-0.36, 0.3, 0]);
  const brown = '#3e2a1b', white = '#f6f4ee';
  const paint = (q) => {
    if (q.part === 'beak') return '#f0b81e';
    if (q.part === 'foot') return '#efb928';
    if (q.part === 'head' || q.part === 'tail') return white;
    if (q.part === 'wing' || q.part === 'primary') return mix(brown, '#241810', sstep(0.4, 1, q.u) * 0.6 + (q.part === 'primary' ? 0.3 : 0));
    return mix(brown, '#4e3726', sstep(-0.2, -0.9, q.n.y) * 0.4);
  };
  b.loft([[-0.38, 0.3, 0.05, 0.04, 0.03], [-0.28, 0.31, 0.1, 0.08, 0.07], [-0.05, 0.31, 0.15, 0.12, 0.12], [0.2, 0.32, 0.14, 0.12, 0.12], [0.34, 0.33, 0.1, 0.09, 0.09]].map(([x, y, w, u, d]) => ({ p: [x, y, 0], r: [w, u, d], b: x < -0.2 ? { body: 0.6, tail: 0.4 } : x > 0.3 ? { body: 0.5, head: 0.5 } : 'body' })),
    { axis: [1, 0, 0], around: 14, part: 'torso', paint, cap0: 0.6, cap1: 0.6 });
  b.loft([[0.3, 0.33, 0.085], [0.42, 0.35, 0.095], [0.52, 0.35, 0.08], [0.58, 0.34, 0.05]].map(([x, y, r]) => ({ p: [x, y, 0], r: [r * 0.95, r, r * 0.9] })), { b: 'head', axis: [1, 0, 0], around: 12, part: 'head', paint, cap1: 0.6 });
  b.loft([{ p: [0.56, 0.35, 0], r: [0.035, 0.035, 0.03] }, { p: [0.63, 0.345, 0], r: [0.028, 0.03, 0.022] }, { p: [0.68, 0.33, 0], r: [0.016, 0.018, 0.016] }, { p: [0.69, 0.3, 0], r: [0.006, 0.008, 0.006] }], { b: 'head', ref: [0, 1, 0], around: 8, part: 'beak', paint });
  [1, -1].forEach((s) => b.eye([0.53, 0.37, 0.05 * s], 0.012, 'head', '#d9b23a'));
  // tail fan
  b.loft([[-0.36, 0.3, 0.06], [-0.48, 0.3, 0.12], [-0.58, 0.3, 0.15], [-0.62, 0.3, 0.13]].map(([x, y, w]) => ({ p: [x, y, 0], r: [w, 0.015, 0.015] })), { b: 'tail', axis: [1, 0, 0], around: 10, part: 'tail', paint, cap0: 0.3, cap1: 0.2, step: 0.04 });
  // tucked feet
  [1, -1].forEach((s) => b.loft([{ p: [-0.1, 0.22, 0.06 * s], r: 0.03 }, { p: [-0.25, 0.2, 0.06 * s], r: 0.025 }, { p: [-0.32, 0.2, 0.06 * s], r: 0.02 }], { b: 'body', part: 'foot', paint, around: 7 }));
  // wings: shoulder -> elbow -> wrist -> tip, chord as forward/backward radii, plus spread primary "fingers"
  const wings = [];
  [1, -1].forEach((s) => {
    const n = s > 0 ? 'L' : 'R';
    b.bone('sh' + n, 'body', [0.08, 0.36, 0.1 * s]);
    b.bone('el' + n, 'sh' + n, [0.04, 0.37, 0.42 * s]);
    b.bone('wr' + n, 'el' + n, [0.02, 0.38, 0.72 * s]);
    const prof = [[0.08, 0.1, 0.03, 0.1, { body: 0.3, ['sh' + n]: 0.7 }], [0.07, 0.25, 0.05, 0.2, 'sh' + n], [0.05, 0.42, 0.05, 0.24, { ['sh' + n]: 0.5, ['el' + n]: 0.5 }], [0.04, 0.58, 0.05, 0.24, 'el' + n], [0.02, 0.72, 0.045, 0.21, { ['el' + n]: 0.5, ['wr' + n]: 0.5 }], [0.0, 0.84, 0.035, 0.17, 'wr' + n], [-0.03, 0.92, 0.02, 0.1, 'wr' + n]];
    b.loft(prof.map(([x, z, fw, bk, bw]) => ({ p: [x, 0.37 + z * 0.02, z * s], r: [0.016, fw, bk], b: bw })), { ref: [1, 0, 0], around: 12, part: 'wing', paint, cap0: 0.3, cap1: 0.4, step: 0.05 });
    for (let k = 0; k < 6; k++) {
      const a = (-8 - k * 13) * D, root = new THREE.Vector3(-0.02 - k * 0.025, 0.375, (0.86 - k * 0.025) * s);
      const len = 0.2 - k * 0.012, tip = root.clone().add(new THREE.Vector3(Math.sin(a) * len, 0.0, Math.cos(a) * len * s));
      b.loft([{ p: root, r: [0.006, 0.025, 0.025] }, { p: root.clone().lerp(tip, 0.6), r: [0.005, 0.022, 0.022] }, { p: tip, r: [0.003, 0.008, 0.008] }], { b: 'wr' + n, ref: [1, 0, 0], around: 6, part: 'primary', paint, cap1: 0.6, step: 0.06 });
    }
    wings.push(n);
  });
  const R = b.finish(), B = R.B, ofs = nextOffset();
  return {
    root: R.root,
    update(t) { this.pose(t + ofs); },
    /** Pose at absolute animation time `tau` (tau = 0 is a neutral standing pose). */
    pose(tau) {
      // soar most of the time; bursts of powerful flaps
      const flapW = sstep(0.0, 0.08, fract(tau / 9)) * (1 - sstep(0.32, 0.4, fract(tau / 9)));
      const ph = tau * TAU * 1.6, f = Math.sin(ph);
      wings.forEach((n) => {
        const s = n === 'L' ? 1 : -1;
        const up = flapW * f * 0.75 + (1 - flapW) * (0.1 + Math.sin(tau * 0.8) * 0.03);
        B['sh' + n].rotation.set(s * up, s * flapW * Math.cos(ph) * 0.15, 0);
        const fold = flapW * Math.max(0, -Math.cos(ph)) * 0.6;
        B['el' + n].rotation.set(-s * fold * 0.3, -s * fold * 0.5, 0);
        B['wr' + n].rotation.set(s * flapW * Math.sin(ph - 0.8) * 0.35, s * fold * 0.6, 0);
      });
      B.body.position.y = 0.3 - flapW * f * 0.03 + Math.sin(tau * 0.7) * 0.02;
      B.body.rotation.set(Math.sin(tau * 0.35) * 0.25 * (1 - flapW), Math.sin(tau * 0.35) * 0.1, Math.sin(tau * 0.5) * 0.04);
      B.tail.rotation.set(Math.sin(tau * 0.35) * 0.2, 0, Math.sin(tau * 0.9) * 0.06);
      B.head.rotation.set(0, Math.sin(tau * 0.45) * 0.5, Math.sin(tau * 0.6) * 0.1);
    },
  };
}


// ================================================================ marine
/**
 * Swimmer on a spine of bones from snout to tail. S.body: [[x, top, bottom, halfWidth]] snout→tail.
 * Fins are flat lofts ({ path: [[x,y,z]...], r: [[thickness, leadingEdge, trailingEdge]...] }) bound to the nearest bone.
 * lateral = true swims side-to-side (fish/sharks), otherwise up-and-down (cetaceans).
 */
function buildSwimmer(S) {
  const b = new Body(S.key);
  const xs = S.body.map((s) => s[0]), x0 = xs[0], x1 = xs[xs.length - 1], L = x0 - x1;
  const nb = S.bones || 7;
  b.bone('root', null, [0, 0, 0]);
  const yAt = (x) => { for (let i = 0; i < S.body.length - 1; i++) { const a = S.body[i], c = S.body[i + 1]; if (x <= a[0] && x >= c[0]) { const t = (a[0] - x) / (a[0] - c[0]); return lerp((a[1] + a[2]) / 2, (c[1] + c[2]) / 2, t); } } return (S.body[0][1] + S.body[0][2]) / 2; };
  const bx = [];
  for (let i = 0; i < nb; i++) { const x = x0 - L * (S.boneStart ?? 0.25) - (L * (1 - (S.boneStart ?? 0.25)) * i) / (nb - 1) * 0.97; bx.push(x); b.bone('s' + i, i ? 's' + (i - 1) : 'root', [x, yAt(x), 0]); }
  const wAt = (x) => {                     // skin weights along the spine
    if (x >= bx[0]) return { s0: 1 };
    for (let i = 0; i < nb - 1; i++) if (x >= bx[i + 1]) { const t = (bx[i] - x) / (bx[i] - bx[i + 1]); return { ['s' + i]: 1 - t, ['s' + (i + 1)]: t }; }
    return { ['s' + (nb - 1)]: 1 };
  };
  const paint = (q) => { q.bx = (x0 - q.p.x) / L; return S.paint(q); };
  b.loft(S.body.map(([x, top, bot, w]) => ({ p: [x, (top + bot) / 2, 0], r: [w, (top - bot) / 2, (top - bot) / 2] })),
    { wfn: (P) => wAt(P.x), axis: [-1, 0, 0], ref: [0, 1, 0], around: S.around || 16, part: 'body', paint, pat: S.pat ? S.pat('body') : null, cap0: 0.5, cap1: 0.4, step: L / (S.rings || 46), sq: S.sq });
  for (const f of S.fins || []) {
    [1, -1].slice(0, f.mirror ? 2 : 1).forEach((s) => {
      b.loft(f.path.map(([x, y, z], i) => ({ p: [x, y, z * s], r: f.r[i], b: wAt(f.path[0][0]) })), { ref: f.ref || [1, 0, 0], around: 10, part: f.part, side: s, paint, pat: S.pat ? S.pat(f.part) : null, cap0: 0.4, cap1: 0.5, step: f.step || 0.05 });
    });
  }
  if (S.eye) { const [ex, ey, ez, er] = S.eye; [1, -1].forEach((s) => b.eye([ex, ey, ez * s], er, wAt(ex), S.iris || '#0c0c0c')); }
  if (S.extras) S.extras(b, { paint, wAt });
  const R = b.finish(), B = R.B, ofs = nextOffset();
  const sp = S.speed || 1.5, amp = S.amp || 0.12;
  return {
    root: R.root,
    update(t) { this.pose(t + ofs); },
    /** Pose at absolute animation time `tau` (tau = 0 is a neutral standing pose). */
    pose(tau) {
      for (let i = 0; i < nb; i++) {
        const k = Math.pow(i / (nb - 1), 1.6), a = amp * (0.15 + k) * Math.sin(tau * sp * TAU - i * (S.lag || 0.55));
        if (S.lateral) B['s' + i].rotation.set(0, a, 0); else B['s' + i].rotation.set(0, 0, a);
      }
      B.s0.rotation.z += Math.sin(tau * 0.3) * 0.05;
      B.s0.rotation.x = Math.sin(tau * 0.23) * (S.roll ?? 0.08);
      B.s0.position.y = yAt(bx[0]) + Math.sin(tau * sp * TAU + 1) * L * 0.006;
      if (S.anim) S.anim(B, tau);
    },
  };
}
/** Fin outline helper: root (x,y,z) swept along `dir` (unit-ish) by `len`; chord profile [[t, thickness, lead, trail]]. */
function fin(root, dir, len, prof, o = {}) {
  const d = new THREE.Vector3(...dir).normalize();
  return { ...o, path: prof.map(([t]) => [root[0] + d.x * len * t + (o.sweep || 0) * t * t, root[1] + d.y * len * t, root[2] + d.z * len * t]), r: prof.map(([, th, a, c]) => [th, a, c]) };
}
const FLUKE = [[0, 0.02, 0.08, 0.1], [0.35, 0.018, 0.06, 0.11], [0.7, 0.012, 0.035, 0.08], [1, 0.006, 0.01, 0.02]];

function blueWhale() {
  return buildSwimmer({
    key: 'blue-whale', amp: 0.08, speed: 0.45, bones: 8,
    body: [[1.5, 0.3, 0.27, 0.03], [1.42, 0.35, 0.2, 0.11], [1.25, 0.39, 0.14, 0.17], [1.0, 0.43, 0.1, 0.2], [0.6, 0.46, 0.09, 0.22], [0.1, 0.46, 0.11, 0.21], [-0.4, 0.43, 0.15, 0.17], [-0.85, 0.4, 0.22, 0.1], [-1.2, 0.36, 0.27, 0.05], [-1.42, 0.33, 0.29, 0.03], [-1.48, 0.315, 0.305, 0.02]],
    eye: [1.08, 0.24, 0.185, 0.013],
    fins: [
      { part: 'fluke', mirror: true, ...fin([-1.44, 0.31, 0], [-0.25, 0, 1], 0.4, FLUKE, { sweep: -0.12 }) },
      { part: 'dorsal', ...fin([-0.72, 0.42, 0], [-0.7, 1, 0], 0.07, [[0, 0.012, 0.08, 0.04], [1, 0.004, 0.005, 0.03]], { ref: [1, 0, 0] }) },
      { part: 'flipper', mirror: true, ...fin([0.78, 0.18, 0.16], [-0.6, -0.35, 1], 0.34, [[0, 0.02, 0.05, 0.05], [0.5, 0.015, 0.035, 0.045], [1, 0.006, 0.008, 0.012]]) },
    ],
    paint(q) {
      const top = mix('#57728b', '#4b657c', fbm(q.p.x * 3, q.p.y * 3, q.p.z * 3));
      if (q.part === 'body' && q.bx < 0.5 && q.n.y < -0.45) return Math.sin(q.p.z * 140) > 0.5 ? '#7f93a5' : '#9fb1c0';
      return furCounter(q, top, '#8ea3b4', -0.8, -0.3);
    },
    pat: (part) => (part === 'body' ? { mode: PAT.mottle, ink: '#7f98ad', scale: 16, mask: (q) => sstep(-0.4, 0.2, q.n.y) * 0.75 } : null),
  });
}
function humpback() {
  const dark = '#2b2e33', white = '#e8e8e4';
  return buildSwimmer({
    key: 'humpback-whale', amp: 0.1, speed: 0.5,
    body: [[1.3, 0.3, 0.26, 0.04], [1.2, 0.37, 0.18, 0.13], [0.98, 0.44, 0.1, 0.2], [0.6, 0.5, 0.06, 0.25], [0.15, 0.5, 0.08, 0.25], [-0.3, 0.47, 0.14, 0.2], [-0.72, 0.41, 0.22, 0.11], [-1.02, 0.37, 0.27, 0.05], [-1.22, 0.33, 0.3, 0.025], [-1.27, 0.32, 0.31, 0.015]],
    eye: [0.95, 0.27, 0.19, 0.013],
    fins: [
      { part: 'fluke', mirror: true, ...fin([-1.24, 0.315, 0], [-0.3, 0, 1], 0.42, [[0, 0.022, 0.08, 0.1], [0.4, 0.02, 0.06, 0.12], [0.75, 0.014, 0.04, 0.09], [1, 0.006, 0.01, 0.025]], { sweep: -0.1 }) },
      { part: 'dorsal', ...fin([-0.42, 0.47, 0], [-0.4, 1, 0], 0.08, [[0, 0.02, 0.1, 0.08], [1, 0.006, 0.02, 0.03]]) },
      { part: 'flipper', mirror: true, ...fin([0.62, 0.16, 0.2], [-0.55, -0.55, 1], 0.85, [[0, 0.035, 0.09, 0.1], [0.3, 0.03, 0.1, 0.08], [0.7, 0.02, 0.075, 0.06], [1, 0.008, 0.015, 0.025]]) },
    ],
    paint(q) {
      if (q.part === 'flipper') return mix(white, '#bfc0bd', fbm(q.p.x * 20, q.p.y * 20, q.p.z * 20) * 0.5);
      if (q.part === 'fluke') return q.n.y < 0 ? '#d9d9d5' : dark;
      if (q.part === 'knob') return '#3c4046';
      if (q.n.y < -0.35 && q.bx < 0.55) return Math.sin(q.p.z * 120) > 0.45 ? '#9a9c9a' : white;
      return mix(dark, white, sstep(-0.35, -0.7, q.n.y));
    },
    extras(b, { paint, wAt }) {
      for (let i = 0; i < 9; i++) { const x = 1.22 - i * 0.05; b.ell([x, 0.36 + i * 0.008, (i % 2 ? 1 : -1) * 0.03], [0.018, 0.012, 0.018], wAt(x), paint, { part: 'knob', seg: 6 }); }
    },
  });
}
function orca() {
  const black = '#121317', white = '#f4f4f2';
  return buildSwimmer({
    key: 'orca', amp: 0.12, speed: 0.7,
    body: [[1.0, 0.3, 0.26, 0.04], [0.93, 0.37, 0.19, 0.11], [0.75, 0.43, 0.13, 0.17], [0.4, 0.48, 0.1, 0.2], [0.0, 0.48, 0.11, 0.2], [-0.4, 0.44, 0.17, 0.14], [-0.7, 0.39, 0.23, 0.07], [-0.92, 0.34, 0.28, 0.03], [-0.98, 0.32, 0.3, 0.02]],
    eye: [0.74, 0.31, 0.16, 0.012], iris: '#050505',
    fins: [
      { part: 'fluke', mirror: true, ...fin([-0.95, 0.31, 0], [-0.3, 0, 1], 0.3, FLUKE, { sweep: -0.06 }) },
      { part: 'dorsal', ...fin([0.05, 0.47, 0], [-0.12, 1, 0], 0.52, [[0, 0.03, 0.14, 0.12], [0.5, 0.02, 0.07, 0.06], [1, 0.006, 0.01, 0.02]]) },
      { part: 'flipper', mirror: true, ...fin([0.55, 0.18, 0.15], [-0.5, -0.5, 1], 0.3, [[0, 0.025, 0.06, 0.06], [0.5, 0.02, 0.08, 0.06], [1, 0.012, 0.03, 0.03]]) },
    ],
    paint(q) {
      if (q.part === 'fluke') return q.n.y < -0.2 ? white : black;
      if (q.part !== 'body') return black;
      return q.n.y > 0.72 && q.p.x < -0.02 && q.p.x > -0.32 ? '#6a6d73' : black;
    },
    pat: (part) => (part !== 'body' ? null : {
      mode: PAT.mark, ink: white, scale: 1,
      coord: (q) => {
        const x = q.p.x, y = q.p.y, z = Math.abs(q.p.z);
        const eye = Math.max(sdEll(x, y + (x - 0.6) * 0.18, 0.6, 0.375, 0.12, 0.034), 0.07 - z);
        const throat = Math.max((q.n.y + (x > 0.25 ? 0.2 : 0.62)) * 0.12, -0.75 - x);
        const flank = Math.max(sdEll(x, y, -0.3, 0.23, 0.27, 0.085), 0.05 - z);
        return Math.min(eye, throat, flank);
      },
    }),
  });
}
function dolphin() {
  return buildSwimmer({
    key: 'bottlenose-dolphin', amp: 0.13, speed: 0.85,
    body: [[0.8, 0.3, 0.285, 0.015], [0.72, 0.312, 0.272, 0.035], [0.66, 0.335, 0.26, 0.055], [0.6, 0.39, 0.23, 0.09], [0.48, 0.43, 0.19, 0.125], [0.25, 0.455, 0.175, 0.145], [0.0, 0.46, 0.175, 0.15], [-0.3, 0.43, 0.2, 0.11], [-0.55, 0.38, 0.25, 0.05], [-0.72, 0.33, 0.29, 0.025], [-0.78, 0.315, 0.305, 0.015]],
    eye: [0.6, 0.31, 0.083, 0.011],
    fins: [
      { part: 'fluke', mirror: true, ...fin([-0.75, 0.31, 0], [-0.3, 0, 1], 0.22, FLUKE.map(([t, a, b2, c]) => [t, a * 0.8, b2 * 0.8, c * 0.8]), { sweep: -0.07 }) },
      { part: 'dorsal', ...fin([0.05, 0.45, 0], [-0.75, 1, 0], 0.2, [[0, 0.02, 0.09, 0.05], [0.6, 0.012, 0.04, 0.03], [1, 0.004, 0.006, 0.01]], { sweep: -0.08 }) },
      { part: 'flipper', mirror: true, ...fin([0.42, 0.22, 0.1], [-0.7, -0.45, 1], 0.17, [[0, 0.015, 0.035, 0.035], [1, 0.006, 0.008, 0.012]], { sweep: -0.04 }) },
    ],
    paint(q) {
      if (q.part !== 'body') return '#5f6a76';
      const cape = sstep(0.55, 0.85, q.n.y) * sstep(0.62, 0.4, q.p.x) * sstep(-0.5, -0.2, q.p.x);
      return mix(mix('#7f8a96', '#5f6a76', cape), '#e6e9eb', sstep(-0.2, -0.6, q.n.y));
    },
  });
}
function greatWhite() {
  const grey = '#68717c', white = '#f3f3f1';
  return buildSwimmer({
    key: 'great-white-shark', lateral: true, amp: 0.13, speed: 0.75, lag: 0.7, roll: 0.03,
    body: [[1.0, 0.335, 0.295, 0.02], [0.93, 0.37, 0.255, 0.07], [0.78, 0.42, 0.205, 0.13], [0.5, 0.47, 0.165, 0.18], [0.15, 0.48, 0.165, 0.18], [-0.2, 0.45, 0.2, 0.14], [-0.5, 0.39, 0.25, 0.075], [-0.75, 0.35, 0.29, 0.035], [-0.84, 0.335, 0.305, 0.022]],
    sq: [2.0, 2.4],
    eye: [0.84, 0.36, 0.085, 0.012],
    fins: [
      { part: 'caudal', ...fin([-0.8, 0.33, 0], [-0.6, 1, 0], 0.45, [[0, 0.02, 0.08, 0.08], [0.5, 0.014, 0.05, 0.06], [1, 0.005, 0.01, 0.015]], { sweep: -0.1 }) },
      { part: 'caudal', ...fin([-0.8, 0.31, 0], [-0.55, -1, 0], 0.32, [[0, 0.02, 0.07, 0.06], [0.5, 0.014, 0.045, 0.045], [1, 0.005, 0.01, 0.012]], { sweep: -0.06, ref: [1, 0, 0] }) },
      { part: 'dorsal', ...fin([0.2, 0.47, 0], [-0.45, 1, 0], 0.36, [[0, 0.025, 0.14, 0.1], [0.5, 0.016, 0.07, 0.07], [1, 0.005, 0.01, 0.015]], { sweep: -0.06 }) },
      { part: 'dorsal', ...fin([-0.55, 0.38, 0], [-0.5, 1, 0], 0.05, [[0, 0.01, 0.025, 0.02], [1, 0.004, 0.005, 0.008]]) },
      { part: 'pectoral', mirror: true, ...fin([0.5, 0.22, 0.12], [-0.6, -0.55, 1], 0.42, [[0, 0.025, 0.08, 0.07], [0.5, 0.018, 0.055, 0.06], [1, 0.006, 0.01, 0.014]], { sweep: -0.1 }) },
    ],
    paint(q) {
      if (q.part === 'pectoral') return q.n.y < -0.3 ? '#e2e2df' : grey;
      if (q.part !== 'body') return grey;
      const x = q.p.x, z = Math.abs(q.p.z);
      const line = -0.05 + 0.12 * Math.sin(x * 9) * 0.3;
      if (x > 0.53 && x < 0.66 && z > 0.12 && q.n.y > -0.4 && q.n.y < 0.5 && Math.sin((x - 0.53) / 0.13 * Math.PI * 5) > 0.75) return '#3c434b';
      if (x > 0.78 && x < 0.95 && q.n.y < -0.5 && Math.abs((x - 0.86) * 0.5 - (z - 0.04) * 0) < 0.02 && z < 0.06) return '#2b2224';
      return q.n.y < line ? white : mix(grey, '#535b65', sstep(0.5, 0.95, q.n.y));
    },
  });
}

function seaTurtle() {
  const b = new Body('green-sea-turtle');
  b.bone('root', null, [0, 0, 0]);
  b.bone('body', 'root', [0, 0.3, 0]);
  b.bone('head', 'body', [0.5, 0.3, 0]);
  const shell = '#6b5b35';
  const paint = (q) => {
    if (q.part === 'shell') return q.n.y > -0.2 ? mix('#5c4d2c', '#8a7442', fbm(q.p.x * 6, 0, q.p.z * 6)) : '#e3d7a4';
    if (q.part === 'head' || q.part === 'flipper') return q.n.y > -0.2 ? '#6f7a49' : '#d8d2a0';
    return shell;
  };
  // shell: domed carapace over a flat plastron
  b.loft([[-0.5, 0.33, 0.28, 0.06], [-0.42, 0.42, 0.24, 0.28], [-0.2, 0.5, 0.22, 0.42], [0.08, 0.52, 0.22, 0.44], [0.3, 0.48, 0.23, 0.36], [0.44, 0.41, 0.25, 0.24], [0.5, 0.36, 0.27, 0.1]].map(([x, top, bot, w]) => ({ p: [x, 0.27, 0], r: [w, top - 0.27, 0.27 - bot], b: 'body' })),
    { axis: [1, 0, 0], around: 20, sq: [2.0, 4.0], part: 'shell', paint, cap0: 0.4, cap1: 0.4, step: 0.04,
      pat: { mode: PAT.cells, ink: '#2e2412', scale: 5.2, prm: 0.06, mask: (q) => sstep(-0.1, 0.2, q.n.y) } });
  b.loft([[0.42, 0.29, 0.05], [0.52, 0.3, 0.06], [0.62, 0.31, 0.068], [0.7, 0.305, 0.06], [0.75, 0.295, 0.04]].map(([x, y, r]) => ({ p: [x, y, 0], r: [r, r * 0.9, r * 0.85], b: x < 0.5 ? { body: 0.5, head: 0.5 } : 'head' })),
    { axis: [1, 0, 0], around: 12, part: 'head', paint, cap1: 0.7, pat: { mode: PAT.cells, ink: '#e9e3c0', scale: 40, prm: 0.08, mask: (q) => sstep(-0.3, 0.1, q.n.y) } });
  [1, -1].forEach((s) => b.eye([0.68, 0.33, 0.045 * s], 0.012, 'head'));
  const flips = [];
  [[0.3, 0.22, 0.55, 1, 'f'], [-0.38, 0.12, 0.22, 0, 'h']].forEach(([x, z, len, front, nm]) => {
    [1, -1].forEach((s) => {
      const n = nm + (s > 0 ? 'L' : 'R');
      b.bone(n, 'body', [x, 0.28, z * s]);
      const prof = front ? [[0, 0.025, 0.06, 0.05], [0.3, 0.02, 0.07, 0.07], [0.7, 0.013, 0.05, 0.06], [1, 0.006, 0.01, 0.015]] : [[0, 0.02, 0.05, 0.04], [0.6, 0.014, 0.06, 0.06], [1, 0.006, 0.02, 0.03]];
      const dir = front ? new THREE.Vector3(-0.35, -0.08, 1).normalize() : new THREE.Vector3(-0.7, -0.05, 0.7).normalize();
      b.loft(prof.map(([t, th, a, c]) => ({ p: [x + dir.x * len * t, 0.28 + dir.y * len * t, (z + dir.z * len * t) * s], r: [th, a, c] })), { b: n, ref: [1, 0, 0], around: 10, part: 'flipper', paint, cap1: 0.5,
        pat: { mode: PAT.cells, ink: '#e6dfb8', scale: 30, prm: 0.1, mask: (q) => sstep(-0.2, 0.2, q.n.y) } });
      flips.push({ n, s, front });
    });
  });
  const R = b.finish(), B = R.B, ofs = nextOffset();
  return {
    root: R.root,
    update(t) { this.pose(t + ofs); },
    /** Pose at absolute animation time `tau` (tau = 0 is a neutral standing pose). */
    pose(tau) {
      const ph = tau * TAU * 0.45, glide = 0.35 + 0.65 * sstep(-0.2, 0.4, Math.sin(tau * 0.25));
      flips.forEach(({ n, s, front }) => {
        if (front) B[n].rotation.set(s * Math.sin(ph) * 0.6 * glide - s * 0.1, -s * Math.cos(ph) * 0.35 * glide, -Math.cos(ph) * 0.3 * glide);
        else B[n].rotation.set(s * Math.sin(ph + 1) * 0.15, -s * 0.2 + s * Math.sin(ph) * 0.15, 0);
      });
      B.body.position.y = 0.3 + Math.sin(ph - 0.8) * 0.02 * glide;
      B.body.rotation.set(Math.sin(tau * 0.3) * 0.05, Math.sin(tau * 0.2) * 0.1, Math.sin(ph - 0.8) * 0.04 * glide);
      B.head.rotation.set(0, Math.sin(tau * 0.5) * 0.3, Math.sin(tau * 0.7) * 0.1);
    },
  };
}


// ================================================================ reptiles
/**
 * Sprawling lizard (crocodile, monitor): one loft from tail tip to neck, separate upper/lower jaws,
 * legs that stick out sideways and swing horizontally, and a body that undulates with the stride.
 */
function buildLizard(S) {
  const b = new Body(S.key);
  b.bone('root', null, [0, 0, 0]);
  b.bone('pelvis', 'root', [S.hipX, S.spineY, 0]);
  b.bone('chest', 'pelvis', [S.shX, S.spineY, 0]);
  b.bone('neck', 'chest', [S.neckX, S.spineY, 0]);
  b.bone('head', 'neck', [S.headX, S.headY, 0]);
  b.bone('jaw', 'head', [S.headX, S.jawY, 0]);
  const tx = S.tailX;
  tx.forEach((x, i) => b.bone('t' + i, i ? 't' + (i - 1) : 'pelvis', [x, S.spineY, 0]));
  const wAt = (x) => {
    const pts = [[S.headX, 'head'], [S.neckX, 'neck'], [S.shX, 'chest'], [S.hipX, 'pelvis'], ...tx.map((x2, i) => [x2, 't' + i])];
    if (x >= pts[0][0]) return { head: 1 };
    for (let i = 0; i < pts.length - 1; i++) if (x >= pts[i + 1][0]) { const t = (pts[i][0] - x) / (pts[i][0] - pts[i + 1][0]); return { [pts[i][1]]: 1 - t, [pts[i + 1][1]]: t }; }
    return { ['t' + (tx.length - 1)]: 1 };
  };
  const paint = S.paint;
  b.loft(S.body.map(([x, top, bot, w]) => ({ p: [x, (top + bot) / 2, 0], r: [w, (top - bot) / 2, (top - bot) / 2] })),
    { wfn: (P) => wAt(P.x), axis: [1, 0, 0], around: 16, sq: S.sq || [2.4, 2.6], part: 'body', paint, pat: S.pat && S.pat('body'), cap0: 0.6, cap1: 0.5, step: 0.05 });
  b.loft(S.head.map(([x, top, bot, w]) => ({ p: [x, (top + bot) / 2, 0], r: [w, (top - bot) / 2, (top - bot) / 2], b: 'head' })),
    { axis: [1, 0, 0], around: 14, sq: S.headSq || [2.2, 3], part: 'head', paint, pat: S.pat && S.pat('head'), cap0: 0.6, cap1: 0.6, step: 0.04 });
  b.loft(S.jaw.map(([x, top, bot, w]) => ({ p: [x, (top + bot) / 2, 0], r: [w, (top - bot) / 2, (top - bot) / 2], b: 'jaw' })),
    { axis: [1, 0, 0], around: 12, sq: [3, 2.2], part: 'jaw', paint, pat: S.pat && S.pat('jaw'), cap0: 0.6, cap1: 0.6, step: 0.04 });
  if (S.eye) { const [ex, ey, ez, er] = S.eye; [1, -1].forEach((s) => { b.ell([ex, ey - er * 0.3, ez * s], [er * 1.6, er * 1.1, er * 1.3], 'head', paint, { part: 'brow' }); b.eye([ex + er * 0.3, ey, (ez + er * 0.5) * s], er, 'head', S.iris || '#b8a23a'); }); }
  // legs
  const legs = [];
  [['f', S.legF, 'chest'], ['h', S.legH, 'pelvis']].forEach(([k, L, parent]) => {
    [1, -1].forEach((s) => {
      const n = k + (s > 0 ? 'L' : 'R');
      const J = L.j.map(([x, y, z]) => [x, y, z * s]);
      b.bone(n + '0', parent, J[0]); b.bone(n + '1', n + '0', J[1]); b.bone(n + '2', n + '1', J[2]);
      const st = J.map((p, i) => ({ p, r: L.r[i], b: i === 0 ? { [parent]: 0.4, [n + '0']: 0.6 } : i === 1 ? { [n + '0']: 0.5, [n + '1']: 0.5 } : i === 2 ? { [n + '1']: 0.5, [n + '2']: 0.5 } : n + '2' }));
      b.loft(st, { ref: [1, 0, 0], around: 9, part: 'leg', side: s, paint, pat: S.pat && S.pat('leg'), cap0: 0.5, cap1: 0.6, step: 0.04 });
      for (let t = -1; t <= 1; t++) { const p = new THREE.Vector3(...J[3]); b.loft([{ p, r: L.r[3][0] * 0.5 }, { p: p.clone().add(new THREE.Vector3(0.07 * Math.cos(t * 0.5) * L.toe, -0.008, 0.06 * Math.sin(t * 0.5) * s * L.toe)), r: L.r[3][0] * 0.25 }], { b: n + '2', part: 'leg', paint, around: 6, cap1: 0.6 }); }
      legs.push({ n, s, front: k === 'f', ph: (k === 'f') === (s > 0) ? 0 : 0.5 });
    });
  });
  if (S.extras) S.extras(b, { paint, wAt });
  const R = b.finish(), B = R.B, ofs = nextOffset(), g = S.gait;
  const spineY = S.spineY;
  return {
    root: R.root,
    update(t) { this.pose(t + ofs); },
    /** Pose at absolute animation time `tau` (tau = 0 is a neutral standing pose). */
    pose(tau) {
      const w = walkEnvelope(tau, g.per || 16, g.walk || 8), s = fract(tau * g.hz);
      const ph = TAU * s;
      // high walk raises the belly off the ground; at rest it sinks down
      B.pelvis.position.y = spineY + (S.lift || 0) * (w - 1) + Math.abs(Math.sin(ph)) * 0.008 * w;
      const bend = Math.sin(ph) * (g.bend || 0.16) * w;
      B.pelvis.rotation.set(0, -bend * 0.6, 0);
      B.chest.rotation.set(0, bend * 1.2, 0);
      B.neck.rotation.set(0, -bend * 0.6 + Math.sin(tau * 0.4) * 0.15 * (1 - w), 0);
      B.head.rotation.set(0, -bend * 0.3, Math.sin(tau * 0.5) * 0.03);
      tx.forEach((x, i) => (B['t' + i].rotation.y = Math.sin(ph - (i + 1) * 0.7) * (g.tail || 0.12) * w + Math.sin(tau * 0.6 - i * 0.5) * 0.04 * (1 - w)));
      for (const L of legs) {
        const sl = fract(s + L.ph), stance = sl < 0.6, q = stance ? sl / 0.6 : (sl - 0.6) / 0.4;
        const swing = stance ? lerp(0.5, -0.5, q) : lerp(-0.5, 0.5, (1 - Math.cos(Math.PI * q)) / 2);
        const lift = stance ? 0 : Math.sin(Math.PI * q);
        const a = swing * (L.front ? 0.9 : 0.8) * w;
        B[L.n + '0'].rotation.set(-L.s * (lift * 0.35 * w + (1 - w) * 0.15), L.s * a * (L.front ? 1 : -1) * L.s, 0);
        B[L.n + '1'].rotation.set(L.s * lift * 0.2 * w, -a * 0.4 * (L.front ? 1 : -1), 0);
        B[L.n + '2'].rotation.set(0, -a * 0.5 * (L.front ? 1 : -1), lift * 0.4 * w);
      }
      if (S.anim) S.anim(B, tau, w);
    },
  };
}

function crocodile() {
  const top = '#4c5233', belly = '#d4cc9c';
  const paint = (q) => {
    if (q.part === 'tooth') return '#f1ecd8';
    if (q.part === 'scute') return '#2f3420';
    if (q.part === 'brow') return '#3f452a';
    if (q.part === 'mouth') return '#e2c3a8';
    return mix(mix(top, '#5d6440', fbm(q.p.x * 6, 0, q.p.z * 6)), belly, sstep(-0.1, -0.6, q.n.y));
  };
  return buildLizard({
    key: 'saltwater-crocodile', spineY: 0.2, hipX: -0.32, shX: 0.36, neckX: 0.52, headX: 0.64, headY: 0.22, jawY: 0.18, lift: 0.06,
    tailX: [-0.55, -0.8, -1.05, -1.3, -1.5],
    body: [[-1.68, 0.22, 0.18, 0.012], [-1.45, 0.26, 0.13, 0.03], [-1.15, 0.3, 0.1, 0.05], [-0.85, 0.32, 0.08, 0.08], [-0.55, 0.33, 0.07, 0.12], [-0.32, 0.34, 0.06, 0.19], [0.0, 0.35, 0.05, 0.24], [0.25, 0.34, 0.06, 0.23], [0.42, 0.32, 0.08, 0.18], [0.55, 0.3, 0.1, 0.13], [0.66, 0.29, 0.12, 0.12]],
    head: [[0.6, 0.3, 0.18, 0.12], [0.72, 0.31, 0.19, 0.115], [0.85, 0.28, 0.195, 0.09], [1.0, 0.245, 0.195, 0.068], [1.15, 0.23, 0.195, 0.058], [1.25, 0.22, 0.195, 0.052], [1.3, 0.21, 0.195, 0.03]],
    jaw: [[0.62, 0.2, 0.11, 0.11], [0.8, 0.198, 0.14, 0.085], [1.0, 0.196, 0.16, 0.062], [1.15, 0.195, 0.165, 0.054], [1.26, 0.194, 0.17, 0.045], [1.3, 0.193, 0.175, 0.025]],
    eye: [0.82, 0.31, 0.05, 0.018], iris: '#b9a640',
    legF: { j: [[0.38, 0.2, 0.15], [0.36, 0.17, 0.32], [0.39, 0.06, 0.36], [0.44, 0.02, 0.38]], r: [[0.06, 0.06], [0.048, 0.048], [0.04, 0.035], [0.035, 0.018]], toe: 1 },
    legH: { j: [[-0.3, 0.2, 0.15], [-0.22, 0.17, 0.36], [-0.3, 0.06, 0.4], [-0.22, 0.02, 0.43]], r: [[0.075, 0.07], [0.055, 0.055], [0.045, 0.04], [0.04, 0.02]], toe: 1.2 },
    gait: { hz: 0.55, bend: 0.14, tail: 0.14, per: 18, walk: 7 },
    paint,
    pat: (part) => (part === 'body' ? { mode: PAT.cells, ink: '#272c18', scale: 14, prm: 0.08, mask: (q) => sstep(-0.2, 0.4, q.n.y) } : part === 'head' ? { mode: PAT.cells, ink: '#30361f', scale: 24, prm: 0.07, mask: (q) => sstep(0.0, 0.5, q.n.y) } : null),
    extras(b, { paint, wAt }) {
      // osteoderm ridges along the back and the double crest of the tail
      for (let x = 0.45; x > -1.6; x -= 0.07) {
        const y = x > -0.55 ? 0.34 + (x > 0 ? -0.005 : 0) : 0.33 + (x + 0.55) * 0.09;
        const rows = x > -0.5 ? [0.05, 0.1] : x > -1.0 ? [0.035] : [0];
        const h = x > -0.55 ? 0.025 : 0.045 * sstep(-1.68, -0.6, x) + 0.01;
        rows.forEach((z) => [1, -1].slice(0, z ? 2 : 1).forEach((s) => b.loft([{ p: [x + 0.02, y - 0.01, z * s * (x < -0.55 ? sstep(-1.6, -0.6, x) : 1)], r: [0.012, 0.02, 0.02] }, { p: [x, y + h, z * s * (x < -0.55 ? sstep(-1.6, -0.6, x) : 1)], r: [0.005, 0.006, 0.006] }], { b: wAt(x), part: 'scute', paint, around: 5, cap1: 0.4, ref: [1, 0, 0] })));
      }
      // interlocking teeth along the jaw line
      for (let k = 0; k < 9; k++) [1, -1].forEach((s) => {
        const x = 0.74 + k * 0.06, z = (0.1 - k * 0.006) * s * (x > 1.0 ? 0.6 : 1);
        const up = k % 2 === 0;
        b.loft([{ p: [x, up ? 0.2 : 0.19, z], r: 0.008 }, { p: [x, up ? 0.165 : 0.225, z], r: 0.002 }], { b: up ? 'head' : 'jaw', part: 'tooth', paint, around: 5 });
      });
      b.ell([1.27, 0.235, 0], [0.025, 0.012, 0.03], 'head', '#2a2d1d', { part: 'nostril' });
    },
    anim(B, tau, w) { const x = fract(tau / 13), gape = (1 - w) * sstep(0.45, 0.55, x) * (1 - sstep(0.85, 0.95, x)); B.jaw.rotation.z = -gape * 0.55; B.head.rotation.z += gape * 0.12; },
  });
}

function komodo() {
  const base = '#6e6450';
  const paint = (q) => {
    if (q.part === 'tongue') return '#e8c33c';
    if (q.part === 'brow') return '#5d5444';
    return mix(mix(base, '#7d7258', fbm(q.p.x * 9, q.p.y * 9, q.p.z * 9)), '#9a917a', sstep(-0.2, -0.7, q.n.y));
  };
  return buildLizard({
    key: 'komodo-dragon', spineY: 0.27, hipX: -0.32, shX: 0.32, neckX: 0.5, headX: 0.72, headY: 0.33, jawY: 0.28, lift: 0.08,
    tailX: [-0.55, -0.78, -1.0, -1.2],
    body: [[-1.38, 0.28, 0.25, 0.012], [-1.2, 0.31, 0.21, 0.035], [-0.95, 0.34, 0.17, 0.06], [-0.7, 0.37, 0.15, 0.09], [-0.45, 0.39, 0.14, 0.14], [-0.3, 0.4, 0.13, 0.19], [0.0, 0.41, 0.12, 0.22], [0.22, 0.4, 0.13, 0.21], [0.36, 0.38, 0.16, 0.16], [0.52, 0.38, 0.21, 0.1], [0.68, 0.39, 0.24, 0.09], [0.76, 0.39, 0.25, 0.085]],
    sq: [2.1, 2.2],
    head: [[0.7, 0.4, 0.28, 0.085], [0.82, 0.405, 0.285, 0.085], [0.94, 0.38, 0.29, 0.07], [1.04, 0.35, 0.29, 0.055], [1.1, 0.33, 0.29, 0.04], [1.13, 0.315, 0.29, 0.025]],
    jaw: [[0.72, 0.29, 0.22, 0.08], [0.86, 0.29, 0.24, 0.07], [1.0, 0.29, 0.26, 0.05], [1.1, 0.289, 0.27, 0.035], [1.13, 0.288, 0.275, 0.02]],
    headSq: [2, 2.4],
    eye: [0.86, 0.375, 0.065, 0.014], iris: '#4a3a1c',
    legF: { j: [[0.34, 0.27, 0.15], [0.33, 0.24, 0.3], [0.37, 0.07, 0.31], [0.43, 0.02, 0.33]], r: [[0.08, 0.08], [0.065, 0.065], [0.05, 0.045], [0.04, 0.02]], toe: 1.1 },
    legH: { j: [[-0.3, 0.27, 0.15], [-0.2, 0.24, 0.32], [-0.28, 0.07, 0.34], [-0.2, 0.02, 0.36]], r: [[0.1, 0.09], [0.075, 0.07], [0.055, 0.05], [0.045, 0.022]], toe: 1.3 },
    gait: { hz: 0.6, bend: 0.16, tail: 0.16, per: 15, walk: 9 },
    paint,
    pat: (part) => ({ mode: PAT.cells, ink: '#4a4334', scale: part === 'head' || part === 'jaw' ? 45 : 32, prm: 0.1, mask: () => 0.8 }),
    extras(b, { paint }) {
      b.bone('tongue', 'jaw', [1.1, 0.29, 0]);
      b.loft([{ p: [1.08, 0.29, 0], r: [0.016, 0.006, 0.006] }, { p: [1.24, 0.285, 0], r: [0.013, 0.005, 0.005] }], { b: 'tongue', part: 'tongue', paint, around: 6 });
      [1, -1].forEach((s) => b.loft([{ p: [1.23, 0.285, 0.005 * s], r: 0.005 }, { p: [1.31, 0.282, 0.026 * s], r: 0.0025 }], { b: 'tongue', part: 'tongue', paint, around: 5 }));
    },
    anim(B, tau) { const f = fract(tau * 0.45) < 0.3 ? Math.sin(fract(tau * 0.45) / 0.3 * Math.PI) : 0; B.tongue.scale.set(0.05 + 0.95 * f, 1, 1); B.jaw.rotation.z = -f * 0.05; },
  });
}

function kingCobra() {
  const b = new Body('king-cobra');
  const N = 18, L = 2.0, R0 = 0.045;
  b.bone('root', null, [0, 0, 0]);
  const xs = [], anchor = 6;
  for (let i = 0; i < N; i++) xs.push(0.45 - (L * i) / (N - 1));
  const y0 = R0;
  b.bone('c' + anchor, 'root', [xs[anchor], y0, 0]);
  for (let i = anchor - 1; i >= 0; i--) b.bone('c' + i, 'c' + (i + 1), [xs[i], y0, 0]);
  for (let i = anchor + 1; i < N; i++) b.bone('c' + i, 'c' + (i - 1), [xs[i], y0, 0]);
  const wAt = (x) => {
    if (x >= xs[0]) return { c0: 1 };
    for (let i = 0; i < N - 1; i++) if (x >= xs[i + 1]) { const t = (xs[i] - x) / (xs[i] - xs[i + 1]); return { ['c' + i]: 1 - t, ['c' + (i + 1)]: t }; }
    return { ['c' + (N - 1)]: 1 };
  };
  const paint = (q) => {
    if (q.part === 'tongue') return '#2a1a1e';
    const x = q.p.x;
    if (x > 0.2 && q.n.y < -0.1) return mix('#e8dca0', '#d9c27a', sstep(0.25, 0.35, x));
    return mix(mix('#4c4a2a', '#2f2e1a', sstep(0.5, -1.3, x)), '#d8cf9a', sstep(-0.2, -0.75, q.n.y));
  };
  // head, a flattened hood behind it, then a long tapering body
  const st = [[0.47, 0.012, 0.012, 0.016], [0.44, 0.024, 0.02, 0.036], [0.4, 0.03, 0.024, 0.046], [0.36, 0.026, 0.022, 0.04], [0.32, 0.026, 0.022, 0.05], [0.26, 0.024, 0.022, 0.13], [0.18, 0.026, 0.024, 0.15], [0.1, 0.034, 0.03, 0.11], [0.02, R0, R0, 0.06], [-0.4, R0 * 1.1, R0 * 1.1, R0 * 1.15], [-0.9, R0 * 1.05, R0 * 1.05, R0 * 1.1], [-1.3, R0 * 0.7, R0 * 0.7, R0 * 0.7], [-1.55, 0.006, 0.006, 0.006]];
  b.loft(st.map(([x, u, d, w]) => ({ p: [x, y0, 0], r: [w, u, d] })), { wfn: (P) => wAt(P.x), axis: [-1, 0, 0], ref: [0, 1, 0], around: 14, part: 'body', paint, cap0: 0.6, cap1: 0.6, step: 0.03,
    pat: { mode: PAT.zebra, ink: '#d6cc95', scale: 9, prm: 0.16, coord: (q) => -q.p.x, mask: (q) => (q.p.x < 0.3 ? sstep(-0.6, 0.1, q.n.y) : 0) } });
  [1, -1].forEach((s) => b.eye([0.425, y0 + 0.018, 0.03 * s], 0.009, 'c0', '#c9a73a'));
  b.bone('tongue', 'c0', [0.46, y0 - 0.005, 0]);
  b.loft([{ p: [0.45, y0 - 0.005, 0], r: 0.003 }, { p: [0.53, y0 - 0.005, 0], r: 0.002 }], { b: 'tongue', part: 'tongue', paint, around: 5 });
  [1, -1].forEach((s) => b.loft([{ p: [0.53, y0 - 0.005, 0], r: 0.002 }, { p: [0.56, y0 - 0.01, 0.012 * s], r: 0.001 }], { b: 'tongue', part: 'tongue', paint, around: 4 }));
  const R = b.finish(), B = R.B, ofs = nextOffset();
  // raised fore-body: angles (radians) for the bones between the anchor and the head
  const rise = [-0.15, -0.6, -0.6, -0.15, 0.0, 1.45];  // c0..c5 relative pitch
  return {
    root: R.root,
    update(t) { this.pose(t + ofs); },
    /** Pose at absolute animation time `tau` (tau = 0 is a neutral standing pose). */
    pose(tau) {
      const sway = Math.sin(tau * 0.9) * 0.12, alert = 0.5 + 0.5 * Math.sin(tau * 0.23);
      for (let i = 0; i < anchor; i++) B['c' + i].rotation.set(0, i < 4 ? sway * 0.5 : 0, rise[i] * (0.85 + 0.15 * alert));
      B['c' + anchor].rotation.set(0, sway * 0.3, 0);
      for (let i = anchor + 1; i < N; i++) B['c' + i].rotation.set(0, Math.sin(tau * 1.3 - i * 0.7) * 0.32 + (i === anchor + 1 ? 0.5 : 0), 0);
      const f = fract(tau * 0.6) < 0.25 ? Math.sin(fract(tau * 0.6) / 0.25 * Math.PI) : 0;
      B.tongue.scale.set(0.05 + f * 0.95, 1, 1);
      B.tongue.rotation.z = Math.sin(tau * 40) * 0.15 * f;
    },
  };
}


// ================================================================ other mammals & insects
function kangaroo() {
  const b = new Body('red-kangaroo');
  b.bone('root', null, [0, 0, 0]);
  b.bone('pelvis', 'root', [-0.05, 0.45, 0]);
  b.bone('spine', 'pelvis', [0.05, 0.72, 0]);
  b.bone('chest', 'spine', [0.16, 0.94, 0]);
  b.bone('neck', 'chest', [0.24, 1.06, 0]);
  const HF = frame2(0.3, 1.19, -12);
  b.bone('head', 'neck', HF.o);
  const red = '#b4643a', pale = '#e6cfb2';
  const paint = (q) => {
    q.hl = HF.local(q.p);
    if (q.part === 'head') { const [x, y] = q.hl; if (x > 0.22) return '#2a1d18'; if (x > 0.12 && y < -0.01) return '#d8b898'; if (x > 0.02 && x < 0.16 && y > -0.03 && y < 0.0 && Math.abs(q.p.z) > 0.04) return '#f1e4d0'; return mix(red, '#a5583a', sstep(0.6, 0.95, q.n.y) * 0.4); }
    if (q.part === 'ear') return q.u > 0.8 ? '#3a2a22' : q.c < -0.2 ? '#e8d2bd' : red;
    if (q.part === 'leg') return q.p.y < 0.07 ? mix('#5a3a28', red, sstep(0.0, 0.07, q.p.y)) : furCounter(q, red, pale, -0.4, 0.3);
    if (q.part === 'arm') return q.u > 0.75 ? '#3a2a22' : pale;
    if (q.part === 'tail') return q.u > 0.85 ? '#7a4a30' : mix(red, '#c8835a', sstep(0.3, -0.6, q.n.y));
    return furCounter(q, red, pale, -0.5, 0.25);
  };
  const tw = (y) => (y < 0.62 ? 'pelvis' : y < 0.86 ? { pelvis: 0.3, spine: 0.7 } : y < 0.98 ? { spine: 0.4, chest: 0.6 } : 'chest');
  b.loft([[-0.24, 0.5, 0.14, 0.15, 0.15], [-0.1, 0.62, 0.17, 0.18, 0.2], [0.05, 0.77, 0.16, 0.16, 0.19], [0.15, 0.91, 0.13, 0.13, 0.15], [0.22, 1.03, 0.09, 0.09, 0.1], [0.27, 1.12, 0.07, 0.07, 0.075]].map(([x, y, w, u, d]) => ({ p: [x, y, 0], r: [w, u, d], b: tw(y) })),
    { ref: [0, 1, 0], around: 16, part: 'torso', paint, cap0: 0.7, cap1: 0.5, step: 0.04 });
  b.loft([[-0.05, 0.065, -0.06, 0.06], [0.05, 0.075, -0.065, 0.07], [0.13, 0.055, -0.05, 0.05], [0.21, 0.04, -0.04, 0.035], [0.25, 0.028, -0.03, 0.026]].map(([x, top, bot, w]) => ({ p: HF.pt(x, (top + bot) / 2), r: [w, (top - bot) / 2, (top - bot) / 2], b: 'head' })),
    { axis: HF.d, ref: HF.u, around: 12, part: 'head', paint, cap1: 0.6 });
  [1, -1].forEach((s) => {
    b.eye(HF.pt(0.09, 0.03, 0.055 * s), 0.013, 'head');
    const nm = s > 0 ? 'earL' : 'earR', base = HF.pt(0.0, 0.06, 0.035 * s);
    b.bone(nm, 'head', base);
    const dir = new THREE.Vector3(-0.25, 1, 0.3 * s).normalize();
    b.loft([[0, 0.03], [0.4, 0.05], [0.8, 0.036], [1, 0.01]].map(([t, wk]) => ({ p: base.clone().addScaledVector(dir, t * 0.15), r: [wk, 0.014, 0.014] })), { b: nm, ref: [-1, 0, 0], around: 8, part: 'ear', paint, cap1: 0.6 });
    // little forearms
    const sh = [0.17, 0.93, 0.1 * s];
    b.bone('arm' + (s > 0 ? 'L' : 'R'), 'chest', sh);
    b.loft([{ p: sh, r: 0.04 }, { p: [0.23, 0.82, 0.11 * s], r: 0.03 }, { p: [0.28, 0.74, 0.1 * s], r: 0.022 }, { p: [0.3, 0.71, 0.1 * s], r: 0.015 }], { b: 'arm' + (s > 0 ? 'L' : 'R'), part: 'arm', paint, around: 8 });
  });
  const legSpec = { z: 0.11, j: [[-0.05, 0.45], [0.13, 0.28], [-0.12, 0.065], [0.21, 0.025]], r: [[0.15, 0.11], [0.07, 0.06], [0.035, 0.035], [0.015, 0.03]], mid: [[0.15, 0.11], [0.05, 0.045, null, 0.4], [0.032, 0.045, null, 0.5]], top: [-0.09, 0.6, 0.15, 0.11] };
  const legs = [1, -1].map((s) => buildLeg(b, s > 0 ? 'l' : 'r', 'pelvis', legSpec, s, { paint, part: 'leg' }));
  const tp = [[-0.22, 0.5, 0.075], [-0.4, 0.33, 0.06], [-0.6, 0.18, 0.046], [-0.8, 0.09, 0.034], [-0.98, 0.05, 0.024], [-1.1, 0.04, 0.012]];
  tp.slice(0, -1).forEach((p, i) => b.bone('t' + i, i ? 't' + (i - 1) : 'pelvis', [p[0], p[1], 0]));
  b.loft(tp.map(([x, y, r], i) => ({ p: [x, y, 0], r: [r, r, r], b: 't' + Math.min(i, tp.length - 2) })), { ref: [0, 1, 0], around: 10, part: 'tail', paint, cap1: 0.8 });
  const R = b.finish(), B = R.B, ofs = nextOffset();
  const pRest = B.pelvis.position.clone();
  return {
    root: R.root,
    update(t) { this.pose(t + ofs); },
    /** Pose at absolute animation time `tau` (tau = 0 is a neutral standing pose). */
    pose(tau) {
      const w = walkEnvelope(tau, 13, 6, 0.8), s = fract(tau * 1.5);
      const contact = s < 0.32, q = contact ? s / 0.32 : (s - 0.32) / 0.68;
      const y = w * (contact ? -0.06 * Math.sin(Math.PI * q) : 0.2 * Math.sin(Math.PI * q));
      const pitch = w * (contact ? -0.2 - 0.15 * q : -0.35 + 0.15 * Math.sin(Math.PI * q)) + (1 - w) * (0.12 + Math.sin(tau * 0.5) * 0.03);
      B.pelvis.position.set(pRest.x, pRest.y + y + (1 - w) * 0.03, 0);
      B.pelvis.rotation.z = pitch;
      B.spine.rotation.z = (1 - w) * 0.08;
      B.neck.rotation.z = -pitch * 0.7 - (1 - w) * 0.1;
      B.head.rotation.set(0, Math.sin(tau * 0.4) * 0.5 * (1 - w), Math.sin(tau * 0.7) * 0.05);
      for (const L of legs) {
        let gx = L.gx, gy = L.gy, foot = 0;
        if (!contact && w > 0) {             // airborne: feet trail, then swing forward to land
          const tuck = Math.sin(Math.PI * q);
          gx = L.gx + w * (-0.12 * (1 - q) + 0.06 * q) + 0; gy = L.gy + y + w * (0.12 * tuck);
          foot = w * (0.9 * (1 - q) + 0.3 * tuck);
        } else if (contact) foot = w * sstep(0.4, 1, q) * 1.0;
        const phi2 = L.pr[2] - foot;
        const tx = gx - Math.sin(phi2) * L.len[2], ty = gy + Math.cos(phi2) * L.len[2];
        const [p0, p1] = ik2(B.pelvis.position.x, B.pelvis.position.y, tx, ty, L.len[0], L.len[1], L.bend);
        const phi = [p0, p1, phi2];
        let prev = pitch;
        for (let i = 0; i < 3; i++) { const dl = phi[i] - L.pr[i]; L.bones[i].rotation.z = dl - prev; prev = dl; }
      }
      for (let i = 0; i < tp.length - 1; i++) B['t' + i].rotation.set(0, Math.sin(tau * 0.8 - i * 0.6) * 0.06 * (1 - w), -pitch * (i === 0 ? 1 : 0) + w * (contact ? 0.05 : -0.18 * Math.sin(Math.PI * q)) * (i < 2 ? 1 : 0.5));
      ['L', 'R'].forEach((k) => B['arm' + k].rotation.set(0, 0, w * Math.sin(TAU * s) * 0.2 - 0.1));
      ['earL', 'earR'].forEach((e, i) => { const f = twitch(tau, 0.17, i * 0.4, 0.05); B[e].rotation.set((i ? -1 : 1) * f * 0.4, 0, -f * 0.3); });
    },
  };
}

function koala() {
  const b = new Body('koala');
  b.bone('root', null, [0, 0, 0]);
  b.bone('body', 'root', [-0.12, 0.5, 0]);
  b.bone('chest', 'body', [-0.1, 0.85, 0]);
  b.bone('head', 'chest', [-0.06, 1.08, 0]);
  const G = '#8f8d8a', W = '#e3e1dc';
  const paint = (q) => {
    if (q.part === 'bark') return mix('#6b543c', '#4a3a2a', sstep(0.3, 0.8, fbm(q.p.x * 30, q.p.y * 4, q.p.z * 30)));
    if (q.part === 'leaf') return mix('#5f7d4a', '#7f9a5e', fbm(q.p.x * 20, q.p.y * 20, q.p.z * 20));
    if (q.part === 'nose') return '#1c1b1c';
    if (q.part === 'ear') return q.s * q.side < -0.3 || q.c > 0.6 ? W : G;
    if (q.part === 'limb') return q.u > 0.85 ? '#2f2b28' : G;
    if (q.part === 'head') return mix(G, W, sstep(-0.3, -0.8, q.n.y) * 0.6);
    return mix(G, W, sstep(0.1, 0.7, q.n.x) * 0.9);
  };
  const fluff = (q) => (fbm(q.p.x * 18, q.p.y * 18, q.p.z * 18) - 0.5) * 0.03;
  // the tree it clings to
  b.loft([{ p: [0.2, -0.05, 0], r: 0.11 }, { p: [0.19, 0.9, 0], r: 0.1 }, { p: [0.17, 1.7, 0], r: 0.085 }, { p: [0.18, 2.1, 0], r: 0.075 }], { b: 'root', part: 'bark', paint, around: 12, ref: [1, 0, 0], step: 0.12, cap0: 0, cap1: 0.4 });
  b.loft([{ p: [0.19, 1.55, 0.04], r: 0.04 }, { p: [0.33, 1.78, 0.14], r: 0.028 }, { p: [0.47, 1.95, 0.2], r: 0.02 }], { b: 'root', part: 'bark', paint, around: 8 });
  [[0.45, 1.97, 0.22], [0.38, 1.9, 0.12], [0.5, 1.9, 0.3], [0.3, 1.83, 0.24]].forEach(([x, y, z], i) => b.ell([x, y, z], [0.09, 0.025, 0.04], 'root', paint, { part: 'leaf', rot: [0.3 * i, 0.8 * i, 0.4] }));
  // body hugging the trunk
  b.loft([[-0.16, 0.36, 0.17, 0.2, 0.16], [-0.18, 0.52, 0.24, 0.24, 0.2], [-0.16, 0.72, 0.24, 0.24, 0.19], [-0.12, 0.9, 0.21, 0.2, 0.17], [-0.08, 1.02, 0.15, 0.14, 0.13]].map(([x, y, w, back, front]) => ({ p: [x, y, 0], r: [w, back, front], b: y < 0.7 ? 'body' : 'chest' })),
    { ref: [-1, 0, 0], around: 16, part: 'body', paint, cap0: 0.7, cap1: 0.5, disp: fluff });
  // head (built facing +X, turned towards the viewer by the head bone)
  const HF = frame2(-0.06, 1.12, 0);
  b.loft([[-0.12, 0.13, -0.12, 0.14], [0.0, 0.16, -0.14, 0.17], [0.1, 0.13, -0.13, 0.15], [0.17, 0.08, -0.09, 0.1]].map(([x, top, bot, w]) => ({ p: HF.pt(x, (top + bot) / 2), r: [w, (top - bot) / 2, (top - bot) / 2], b: 'head' })),
    { axis: HF.d, ref: HF.u, around: 14, part: 'head', paint, cap1: 0.5, disp: fluff });
  b.ell(HF.pt(0.17, -0.01, 0), [0.05, 0.075, 0.055], 'head', paint, { part: 'nose' });
  [1, -1].forEach((s) => {
    b.eye(HF.pt(0.13, 0.05, 0.085 * s), 0.016, 'head');
    b.ell(HF.pt(-0.02, 0.13, 0.15 * s), [0.04, 0.1, 0.1], 'head', paint, { part: 'ear', side: s, rot: [s * 0.5, 0, 0], seg: 14 });
    // arms reach up around the trunk, legs grip below
    b.bone('arm' + s, 'chest', [-0.08, 0.92, 0.17 * s]);
    b.loft([{ p: [-0.08, 0.92, 0.17 * s], r: 0.075 }, { p: [0.0, 1.05, 0.17 * s], r: 0.065 }, { p: [0.08, 1.16, 0.12 * s], r: 0.05 }, { p: [0.12, 1.2, 0.08 * s], r: 0.04 }], { b: 'arm' + s, part: 'limb', paint, around: 10, disp: fluff });
    b.bone('leg' + s, 'body', [-0.14, 0.44, 0.17 * s]);
    b.loft([{ p: [-0.14, 0.44, 0.17 * s], r: 0.09 }, { p: [-0.02, 0.4, 0.2 * s], r: 0.075 }, { p: [0.06, 0.32, 0.14 * s], r: 0.055 }, { p: [0.1, 0.3, 0.1 * s], r: 0.045 }], { b: 'leg' + s, part: 'limb', paint, around: 10, disp: fluff });
  });
  const R = b.finish(), B = R.B, ofs = nextOffset();
  return {
    root: R.root,
    update(t) { this.pose(t + ofs); },
    /** Pose at absolute animation time `tau` (tau = 0 is a neutral standing pose). */
    pose(tau) {
      const br = Math.sin(tau * 1.4) * 0.015;
      B.chest.scale.set(1 + br, 1, 1 + br);
      const look = 0.5 + 0.5 * Math.sin(tau * 0.25);
      B.head.rotation.set(0, -0.85 - look * 0.6, Math.sin(tau * 0.6) * 0.06 + (fract(tau / 9) < 0.12 ? Math.sin(tau * 12) * 0.03 : 0));
      const climb = sstep(0.8, 0.9, fract(tau / 14)) * (1 - sstep(0.9, 1, fract(tau / 14)));
      B.body.position.y = 0.5 + climb * 0.05;
      B.arm1.rotation.z = climb * 0.25; B['arm-1'].rotation.z = climb * 0.12;
      B.leg1.rotation.z = -climb * 0.1;
    },
  };
}

function butterfly() {
  // wing outlines in wing space (u: outward from the body, v: along the body, + = forward)
  const fore = [[0.0, 0.06], [0.3, 0.2], [0.7, 0.3], [0.97, 0.3], [1.0, 0.22], [0.9, 0.0], [0.72, -0.14], [0.45, -0.14], [0.15, -0.1], [0.0, -0.05]];
  const hind = [[0.0, -0.02], [0.35, -0.08], [0.6, -0.16], [0.68, -0.32], [0.56, -0.5], [0.34, -0.58], [0.14, -0.46], [0.02, -0.25]];
  const W = 512, Hh = 512, U = (u) => 8 + u * (W - 16), Vv = (v) => Hh * (0.36 - v) / 0.98;
  const cv = document.createElement('canvas'); cv.width = W; cv.height = Hh;
  const x = cv.getContext('2d');
  const path = (pts) => { x.beginPath(); pts.forEach(([u, v], i) => (i ? x.lineTo(U(u), Vv(v)) : x.moveTo(U(u), Vv(v)))); x.closePath(); };
  [fore, hind].forEach((pts, wi) => {
    x.save(); path(pts); x.clip();
    const g = x.createRadialGradient(U(0), Vv(0), 10, U(0), Vv(0), 420);
    g.addColorStop(0, '#d86a10'); g.addColorStop(1, '#f39a2a');
    x.fillStyle = g; x.fillRect(0, 0, W, Hh);
    x.strokeStyle = '#121010'; x.lineCap = 'round';
    // veins radiating from the wing root
    x.lineWidth = 5;
    pts.forEach(([u, v], i) => { if (i % 1 === 0 && u > 0.3) { x.beginPath(); x.moveTo(U(0.02), Vv(wi ? -0.08 : 0.0)); x.quadraticCurveTo(U(u * 0.5), Vv(v * 0.6 + (wi ? -0.1 : 0.02)), U(u), Vv(v)); x.stroke(); } });
    x.lineWidth = 7; x.beginPath(); x.moveTo(U(0.02), Vv(wi ? -0.04 : 0.04)); x.lineTo(U(wi ? 0.5 : 0.9), Vv(wi ? -0.3 : 0.24)); x.stroke();
    // thick black margin with two rows of white dots
    path(pts); x.lineWidth = 46; x.stroke();
    if (!wi) { x.fillStyle = '#121010'; x.beginPath(); x.moveTo(U(0.68), Vv(0.32)); x.lineTo(U(1.02), Vv(0.3)); x.lineTo(U(0.95), Vv(0.02)); x.closePath(); x.fill(); }
    x.fillStyle = '#fbf6ea';
    for (let i = 0; i < pts.length; i++) {
      const [u0, v0] = pts[i], [u1, v1] = pts[(i + 1) % pts.length];
      if (u0 < 0.25 && u1 < 0.25) continue;
      for (let k = 0; k < 3; k++) {
        const t = (k + 0.5) / 3, uu = lerp(u0, u1, t), vv = lerp(v0, v1, t), cx = U(uu), cy = Vv(vv);
        const dx = U(0.35) - cx, dy = Vv(wi ? -0.25 : 0.05) - cy, l = Math.hypot(dx, dy);
        [9, 22].forEach((d, j) => { x.beginPath(); x.arc(cx + (dx / l) * d, cy + (dy / l) * d, j ? 3.2 : 4.2, 0, TAU); x.fill(); });
      }
    }
    if (!wi) [[0.83, 0.2], [0.88, 0.14], [0.78, 0.25]].forEach(([u, v]) => { x.fillStyle = '#f7c26a'; x.beginPath(); x.arc(U(u), Vv(v), 7, 0, TAU); x.fill(); });
    x.restore();
  });
  const tex = new THREE.CanvasTexture(cv);
  tex.colorSpace = THREE.SRGBColorSpace;
  tex.anisotropy = 4;
  const wm = new THREE.MeshStandardMaterial({ map: tex, side: THREE.DoubleSide, roughness: 0.75 });
  const wingGeo = (pts, s) => {
    const shape = new THREE.Shape(pts.map(([u, v]) => new THREE.Vector2(u, v)));
    const g = new THREE.ShapeGeometry(shape, 6);
    const pos = g.attributes.position, uv = g.attributes.uv;
    for (let i = 0; i < pos.count; i++) {
      const u = pos.getX(i), v = pos.getY(i);
      uv.setXY(i, U(u) / W, 1 - Vv(v) / Hh);
      pos.setXYZ(i, v * 0.42, Math.sin(u * 2.2) * 0.02, u * 0.48 * s);   // slight camber
    }
    g.computeVertexNormals();
    return g;
  };
  const root = new THREE.Group(), body = new THREE.Group();
  body.position.y = 0.5;
  root.add(body);
  const bodyMat = new THREE.MeshStandardMaterial({ color: '#1a1716', roughness: 0.6 });
  const thorax = new THREE.Mesh(new THREE.SphereGeometry(1, 12, 8), bodyMat); thorax.scale.set(0.09, 0.045, 0.045); thorax.position.x = 0.03;
  const abdomen = new THREE.Mesh(new THREE.SphereGeometry(1, 12, 8), bodyMat); abdomen.scale.set(0.16, 0.035, 0.035); abdomen.position.set(-0.17, -0.01, 0);
  const headM = new THREE.Mesh(new THREE.SphereGeometry(0.035, 10, 8), bodyMat); headM.position.x = 0.14;
  body.add(thorax, abdomen, headM);
  [1, -1].forEach((s) => {
    const curve = new THREE.QuadraticBezierCurve3(new THREE.Vector3(0.16, 0.02, 0.012 * s), new THREE.Vector3(0.26, 0.1, 0.05 * s), new THREE.Vector3(0.34, 0.12, 0.08 * s));
    const ant = new THREE.Mesh(new THREE.TubeGeometry(curve, 8, 0.004, 4), bodyMat);
    const knob = new THREE.Mesh(new THREE.SphereGeometry(0.009, 6, 5), bodyMat); knob.position.copy(curve.getPoint(1));
    body.add(ant, knob);
  });
  const wings = [1, -1].map((s) => {
    const pv = new THREE.Group();
    pv.position.set(0.02, 0.02, 0.02 * s);
    const f = new THREE.Mesh(wingGeo(fore, s), wm), h = new THREE.Mesh(wingGeo(hind, s), wm);
    h.position.set(-0.02, -0.004, 0);
    pv.add(f, h);
    body.add(pv);
    return { pv, s, h };
  });
  const ofs = nextOffset();
  return {
    root,
    update(t) { this.pose(t + ofs); },
    /** Pose at absolute animation time `tau` (tau = 0 is a neutral standing pose). */
    pose(tau) {
      const glide = sstep(0.55, 0.62, fract(tau / 4)) * (1 - sstep(0.85, 0.92, fract(tau / 4)));
      const f = Math.sin(tau * TAU * 5.5);
      const a = lerp(0.15 + f * 0.95, 0.35 + Math.sin(tau * 3) * 0.05, glide);
      wings.forEach((w) => { w.pv.rotation.x = -w.s * a; w.h.rotation.x = -w.s * Math.sin(tau * TAU * 5.5 - 0.4) * 0.12 * (1 - glide); });
      body.position.y = 0.5 + Math.sin(tau * 1.7) * 0.08 + f * 0.02 * (1 - glide);
      body.rotation.set(Math.sin(tau * 0.9) * 0.12, Math.sin(tau * 0.5) * 0.3, 0.15 + Math.sin(tau * 1.1) * 0.12);
    },
  };
}

// ================================================================ species catalogue
const PROC = {
  'african-elephant': () => elephant(),
  'woolly-mammoth': () => mammoth(),
  lion: () => lion(),
  tiger: () => tiger(),
  giraffe: () => giraffe(),
  zebra: () => zebra(),
  cheetah: () => cheetah(),
  jaguar: () => jaguar(),
  'snow-leopard': () => snowLeopard(),
  gorilla: () => gorilla(),
  orangutan: () => orangutan(),
  'giant-panda': () => panda(),
  'polar-bear': () => polarBear(),
  'brown-bear': () => brownBear(),
  'gray-wolf': () => grayWolf(),
  thylacine: () => thylacine(),
  'american-bison': () => bison(),
  hippopotamus: () => hippo(),
  'white-rhino': () => whiteRhino(),
  dromedary: () => dromedary(),
  moose: () => moose(),
  'emperor-penguin': () => penguin(),
  'bald-eagle': () => baldEagle(),
  ostrich: () => ostrich(),
  dodo: () => dodo(),
  'blue-whale': () => blueWhale(),
  'humpback-whale': () => humpback(),
  orca: () => orca(),
  'bottlenose-dolphin': () => dolphin(),
  'great-white-shark': () => greatWhite(),
  'green-sea-turtle': () => seaTurtle(),
  'saltwater-crocodile': () => crocodile(),
  'komodo-dragon': () => komodo(),
  'king-cobra': () => kingCobra(),
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
  // measure every copy in the same neutral pose so all instances of a species share one scale and centre
  if (built.pose) built.pose(0); else built.update(0);
  const object = normalise(built.root);
  return { object, update: (t) => built.update(t), flying: FLYING.has(id), marine: MARINE.has(id) };
}

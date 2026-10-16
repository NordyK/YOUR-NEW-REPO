// Animated 3D animals: real glTF models where available, stylised procedural models otherwise.
// Convention for every model: head points +X, up is +Y, feet rest on y = 0.
import { THREE, GLTFLoader, OrbitControls, SkeletonUtils } from '../vendor/earth-libs.js';

export { THREE };

const D = Math.PI / 180;
const MODEL_DIR = new URL('../assets/models/', import.meta.url).href;

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
  lion: Q({ color: '#c99a5b', belly: '#e6c99b', bodyLen: 0.78, bodyH: 0.33, bodyW: 0.28, legLen: 0.55, legR: 0.075, paw: true, neckLen: 0.22, neckR: 0.16, neckAngle: 35, head: [0.22, 0.2, 0.19], snout: [0.13, 0.11, 0.12], snoutColor: '#d9b483', nose: '#3a2418', mane: '#7a4a1f', ears: { shape: 'round', size: 0.1 }, tail: { len: 0.7, r: 0.03, angle: 50, tuft: '#3d2610' }, speed: 4.5 }),
  tiger: Q({ color: '#e07b28', belly: '#f4ece0', pattern: { type: 'tiger', bg: '#e07b28', fg: '#1a120c' }, bodyLen: 0.85, bodyH: 0.32, bodyW: 0.28, legLen: 0.52, legR: 0.08, paw: true, neckLen: 0.2, neckR: 0.16, neckAngle: 30, head: [0.22, 0.2, 0.2], headColor: '#e07b28', snout: [0.12, 0.1, 0.13], snoutColor: '#f4ece0', nose: '#d77a7a', ears: { shape: 'round', size: 0.09 }, tail: { len: 0.75, r: 0.035, angle: 45, tip: '#1a120c' }, speed: 4.5 }),
  giraffe: Q({ color: '#d9a75c', pattern: { type: 'patches', bg: '#f1dfb8', fg: '#a8642b' }, neckPattern: true, bodyLen: 0.68, bodyH: 0.42, bodyW: 0.3, legLen: 1.6, legR: 0.06, hoof: '#3a2a1a', lowerLegColor: '#efe1c3', neckLen: 1.65, neckR: 0.11, neckAngle: 62, neckBase: 0.4, head: [0.22, 0.13, 0.12], headColor: '#c99449', snout: [0.13, 0.08, 0.08], snoutColor: '#b3824a', ears: { shape: 'pointy', size: 0.12 }, horns: 'ossicones', tail: { len: 0.7, r: 0.025, angle: 15, tuft: '#1e1408' }, speed: 3.2, stride: 20 }),
  zebra: Q({ color: '#f2f2f2', pattern: { type: 'stripes', bg: '#f4f4f2', fg: '#141414' }, neckPattern: true, bodyLen: 0.7, bodyH: 0.35, bodyW: 0.27, legLen: 0.78, legR: 0.06, hoof: '#1a1a1a', lowerLegColor: '#cfcfcf', neckLen: 0.42, neckR: 0.13, neckAngle: 50, head: [0.26, 0.14, 0.12], headColor: '#e8e8e8', snout: [0.12, 0.1, 0.1], snoutColor: '#222', mane: null, ears: { shape: 'pointy', size: 0.14 }, tail: { len: 0.55, r: 0.03, angle: 20, tuft: '#111' }, speed: 5 }),
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

// ---------------------------------------------------------------- stand-alone viewer
export class AnimalViewer {
  constructor(container) {
    this.el = container;
    this.renderer = new THREE.WebGLRenderer({ antialias: true, alpha: true });
    this.renderer.setPixelRatio(Math.min(2, window.devicePixelRatio));
    this.renderer.outputColorSpace = THREE.SRGBColorSpace;
    this.renderer.toneMapping = THREE.ACESFilmicToneMapping;
    container.appendChild(this.renderer.domElement);
    this.scene = new THREE.Scene();
    this.camera = new THREE.PerspectiveCamera(38, 1, 0.01, 100);
    this.scene.add(new THREE.HemisphereLight('#dbe9ff', '#3b2f22', 2.2));
    const sun = new THREE.DirectionalLight('#fff3e0', 2.6);
    sun.position.set(3, 5, 4);
    this.scene.add(sun);
    const rim = new THREE.DirectionalLight('#8fb8ff', 1.2);
    rim.position.set(-4, 2, -3);
    this.scene.add(rim);
    // ground disc with soft shadow
    const cv = document.createElement('canvas'); cv.width = cv.height = 128;
    const g = cv.getContext('2d'), grd = g.createRadialGradient(64, 64, 4, 64, 64, 64);
    grd.addColorStop(0, 'rgba(0,0,0,0.55)'); grd.addColorStop(1, 'rgba(0,0,0,0)');
    g.fillStyle = grd; g.fillRect(0, 0, 128, 128);
    this.shadow = new THREE.Mesh(new THREE.PlaneGeometry(1.6, 1.6), new THREE.MeshBasicMaterial({ map: new THREE.CanvasTexture(cv), transparent: true, depthWrite: false }));
    this.shadow.rotation.x = -Math.PI / 2;
    this.scene.add(this.shadow);
    this.controls = new OrbitControls(this.camera, this.renderer.domElement);
    this.controls.enableDamping = true;
    this.controls.autoRotate = true;
    this.controls.autoRotateSpeed = 1.6;
    this.controls.enablePan = false;
    this.clock = new THREE.Timer();
    this.current = null;
    this.token = 0;
    const loop = () => {
      if (this.disposed) return;
      this.raf = requestAnimationFrame(loop);
      this.resize();
      this.clock.update();
      const dt = Math.min(0.05, this.clock.getDelta()), t = this.clock.getElapsed();
      if (this.current) this.current.update(t, dt);
      this.controls.update();
      this.renderer.render(this.scene, this.camera);
    };
    loop();
  }
  resize() {
    const w = this.el.clientWidth, h = this.el.clientHeight;
    if (w && h && (w !== this._w || h !== this._h)) {
      this._w = w; this._h = h;
      this.renderer.setSize(w, h, false);
      this.camera.aspect = w / h;
      this.camera.updateProjectionMatrix();
    }
  }
  async show(id) {
    const token = ++this.token;
    const inst = await createAnimal(id);
    if (token !== this.token || this.disposed) return;
    if (this.current) this.scene.remove(this.current.object);
    this.current = inst;
    this.scene.add(inst.object);
    const size = inst.object.userData.size;
    const hover = inst.flying ? 0.25 : inst.marine ? 0.15 : 0;
    inst.object.position.y = hover;
    this.shadow.visible = !inst.marine;
    this.shadow.scale.setScalar(Math.max(size.x, size.z) * 1.1);
    const cy = hover + size.y * 0.5;
    const dist = Math.max(1.35, size.y * 1.9 + 0.7);
    this.camera.position.set(dist * 0.85, cy + dist * 0.3, dist * 0.75);
    this.controls.target.set(0, cy, 0);
    this.controls.update();
  }
  dispose() {
    this.disposed = true;
    cancelAnimationFrame(this.raf);
    this.controls.dispose();
    this.renderer.dispose();
    this.renderer.forceContextLoss?.();
    this.renderer.domElement.remove();
  }
}

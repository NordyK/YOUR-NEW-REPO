#!/usr/bin/env bash
# Rebuilds vendor/earth-libs.js: globe.gl + three.js bundled with a single three.js instance.
set -euo pipefail
cd "$(dirname "$0")"
out="$PWD/earth-libs.js"
tmp=$(mktemp -d)
cat > "$tmp/package.json" <<'JSON'
{ "private": true, "dependencies": { "globe.gl": "2.45.1", "three-globe": "2.45.2", "three-render-objects": "1.42.0", "three": "0.183.2", "esbuild": "0.25.10" } }
JSON
cat > "$tmp/entry.js" <<'JS'
export { default as Globe } from 'globe.gl';
export * as THREE from 'three';
export { GLTFLoader } from 'three/examples/jsm/loaders/GLTFLoader.js';
export { OrbitControls } from 'three/examples/jsm/controls/OrbitControls.js';
export * as SkeletonUtils from 'three/examples/jsm/utils/SkeletonUtils.js';
JS
(cd "$tmp" && npm install --no-audit --no-fund >/dev/null && npx esbuild entry.js --bundle --format=esm --minify --legal-comments=eof --outfile="$out")
rm -rf "$tmp"

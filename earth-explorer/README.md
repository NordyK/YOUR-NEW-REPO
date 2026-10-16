# 🌍 Earth Explorer

An interactive 3D globe of Earth: natural wonders, landmarks, wildlife, natural resources, country borders, and a time machine that runs from the last Ice Age (20,000 BC) to today.

## Run it

It's a static site with no build step. Serve this folder with any web server and open it in a browser:

```bash
cd earth-explorer
python3 -m http.server 8000
# open http://localhost:8000
```

(Opening `index.html` straight from disk won't work, because browsers block ES modules on `file://`.) It also works on GitHub Pages or any static host.

## Features

- 🛰️ **Zoomable globe.** Esri satellite imagery down to individual buildings, plus terrain, street, Blue Marble and city-lights styles. Shows country borders; hover or click a country for its flag, population, GDP, wonders and resources.
- 🏔️ **87 natural wonders.** Mountains, volcanoes, reefs, canyons, waterfalls and more, each with exact coordinates, facts, stats and a Wikipedia photo. Filter by type.
- 🏛️ **121 landmarks.** From ancient wonders to skyscrapers. Each has a build year (and a destruction year where relevant), so the Colossus of Rhodes, the Twin Towers or the Berlin Wall appear and disappear as you move through time. "Satellite close-up" flies down to street level.
- 🐾 **42 animals.** Tick any number of species to paint their approximate ranges and place **animated 3D models** at their real habitats. Each species opens a rotatable 3D viewer with IUCN status, population, facts and the best places to see it. Historic ranges (lions in Europe, bison across North America…) and extinct species (dodo, mammoth, thylacine) respond to the timeline.
- ⛏️ **16 natural resources.** Oil, gas, coal, gold, lithium, cobalt, rare earths and more: 272 major fields and mines, plus a map colouring the top producing countries.
- ⏳ **Time machine.** Historical borders for 53 dates (from historical-basemaps). Ice sheets, land bridges (Beringia, Doggerland, Sundaland), the Green Sahara, the shrinking Aral Sea and Lake Chad, glacier retreat, Arctic sea ice and deforestation all change with the year. 144 historical events are pinned to the globe. Live indicators show population, CO₂, temperature, sea level, sea ice and forest area, each compared with today. Press ▶ to play through history.
- 🔎 **Search** across wonders, landmarks, animals, resources, countries and events.

## Project layout

```
index.html, css/style.css
js/main.js           globe, layers, UI, timeline
js/animals3d.js      glTF + procedural animated animals, 3D viewer
js/data/*.js         curated data (wonders, landmarks, animals, resources, timeline)
assets/models/*.glb  animated glTF models
assets/data/         Natural Earth country borders
vendor/earth-libs.js globe.gl + three.js bundle (rebuild with vendor/build.sh)
```

## Data & credits

- Borders: [Natural Earth](https://www.naturalearthdata.com/) (public domain). Historical borders: [historical-basemaps](https://github.com/aourednik/historical-basemaps) by A. Ourednik (GPL-3.0), loaded at runtime.
- Imagery: Esri World Imagery, Topo and Street maps (© Esri, Maxar, Earthstar Geographics and contributors). Blue Marble and Earth at Night: NASA.
- Photos and summaries: Wikipedia REST API (images from Wikimedia Commons under their respective licences).
- Statistics: NOAA (Mauna Loa CO₂), NSIDC Sea Ice Index, HadCRUT5 / WMO, USGS Mineral Commodity Summaries, Energy Institute Statistical Review, Kimberley Process, UN and long-run population estimates, FAO forest data.
- 3D models: Horse, Flamingo, Parrot and Stork from the three.js examples (mirada / ro.me). Fox from the Khronos glTF Sample Assets (CC0 model by PixelMannen, rigging CC-BY 4.0 by @tomkranis). All other animals are generated procedurally in `js/animals3d.js`.
- Libraries: [globe.gl](https://github.com/vasturiano/globe.gl) and [three.js](https://threejs.org/).

**Accuracy:** animal ranges, ice sheets and lakes are drawn as simplified ellipses, and prehistoric figures (population, CO₂, sea level, forest area) are rough scientific estimates. Treat them as illustrations, not precise maps.

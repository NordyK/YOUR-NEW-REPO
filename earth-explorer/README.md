# Earth Explorer

An interactive 3D globe of Earth: natural wonders, landmarks, wildlife, natural resources, country borders, and a time machine that runs from the last Ice Age (20,000 BC) to today.

## Run it

It's a static site with no build step. Serve this folder with any web server and open it in a browser:

```bash
cd earth-explorer
python3 -m http.server 8000
# open http://localhost:8000
```

(Opening `index.html` straight from disk won't work, because browsers block ES modules on `file://`.)

**Publish on GitHub Pages:** the workflow in `.github/workflows/earth-explorer-pages.yml` deploys this folder whenever `main` changes. Enable it once under repository *Settings → Pages → Source: GitHub Actions*. Any other static host works too.

## Features

Everything lives on one globe and follows one timeline, from 20,000 BC to today.

| Section | What you can do |
|---|---|
| **Wonders, Landmarks** | 87 natural wonders and 121 landmarks with real photos, facts and a satellite close-up down to street level. Lost landmarks (Colossus of Rhodes, Twin Towers…) appear when you travel back in time. |
| **Animals** | 42 species: approximate ranges, animated 3D miniatures at their habitats (rigged procedural models with planted-foot gaits, grazing, swimming and flight, plus glTF models for five species), historic ranges and extinct species that follow the timeline. Cards use real photographs. |
| **Resources** | 16 natural resources, 272 major deposits, producer choropleths. |
| **Countries** | An atlas of every country: capital, languages, currency, area, neighbours (bundled from mledoze/countries), and 60 years of World Bank data (population, GDP per person, life expectancy, urbanisation, CO₂, forest, internet). Compare any two countries side by side. |
| **History** | Real historical borders for 54 dates (historical-basemaps), ice sheets, land bridges, lakes, glaciers, deforestation, 144 pinned events, eras and world indicators. |
| **News** | Real reporting tied to place and time, never invented. *Day by day*: Wikipedia's Current events pages, which cite their news outlets (2002 to today). *Year chronicle*: the dated events of any year, decade or century back to the Ice Age. *By country*: “1912 in France”-style chronicles. Stories are geolocated through the articles they link to and pinned on the globe. *Newspapers*: front pages of real American newspapers from 1770 to 1963 (Library of Congress, Chronicling America) for any date, with keyword search. *What happened here?*: on any card, the dated events, battles, buildings and treaties recorded by Wikidata near that place, grouped by era. |
| **Learn** | Guided tours that move the camera and the time machine and ask check-your-understanding questions; six quizzes generated from the data (find it on the globe, flags, capitals, name the lit-up country, when was it built, animal homes); progress saved in the browser. Tours can narrate themselves aloud, and the camera pulls back for long hops like a documentary. |
| **Live** | NASA EPIC photographs of the whole sunlit Earth from 1.5 million km (2015 to today), a real-time day/night terminator with city lights, NASA GIBS daily satellite maps for any date since 2000 that follow the timeline, science maps (seasons month by month, night lights, land surface temperature), and live layers: USGS earthquakes, NASA EONET natural events (wildfires, storms, volcanoes) and the International Space Station. |
| **Article reader** | *Read full article* on any card opens the full Wikipedia article as an encyclopedia entry, with a margin that ties it to the globe: locator, key figures, dates that set the time machine, nearby items and sources. Links inside the article open in the reader. *Listen* reads the article aloud. |

Also: a great-circle distance tool, an installable offline-capable app (PWA: the shell and recently viewed tiles, photos and articles are cached), search across everything, nearby Wikipedia places when zoomed in, shareable links (year, selections, section, open card and camera view), keyboard access, reduced-motion support, and a phone layout.

## Project layout

```
index.html, css/style.css
js/main.js           globe, layers, cards, rail, timeline, search, links
js/reader.js         article reader (Wikipedia + margin apparatus)
js/countries.js      country atlas, World Bank charts, comparison
js/news.js           news and chronicles (Wikipedia Current events, year/country chronicles)
js/learn.js          guided tours and quizzes
js/live.js           NASA EPIC, day/night terminator, GIBS imagery, USGS/EONET/ISS layers
js/data/tours.js     guided tour scripts
sw.js, manifest.webmanifest  offline cache and install metadata
js/animals3d.js      glTF + procedural animated animal miniatures
js/icons.js          Lucide icon subset
js/data/*.js         curated data (wonders, landmarks, animals, resources, timeline)
assets/models/*.glb  animated glTF models
assets/data/         Natural Earth country borders; countries-info.json (mledoze/countries, ODbL)
assets/fonts/        Public Sans + Newsreader (self-hosted, SIL OFL)
vendor/earth-libs.js globe.gl + three.js bundle (rebuild with vendor/build.sh)
```

## Data & credits

- Borders: [Natural Earth](https://www.naturalearthdata.com/) (public domain). Historical borders: [historical-basemaps](https://github.com/aourednik/historical-basemaps) by A. Ourednik (GPL-3.0), loaded at runtime.
- Imagery: Esri World Imagery, Topo and Street maps (© Esri, Maxar, Earthstar Geographics and contributors). Blue Marble and Earth at Night: NASA.
- Articles, photos, news and chronicles: Wikipedia REST/Action APIs (text CC BY-SA 4.0; news items link to the outlets Wikipedia cites) (images from Wikimedia Commons under their respective licences).
- Country facts: [mledoze/countries](https://github.com/mledoze/countries) (ODbL 1.0). Time series: World Bank Open Data (CC BY 4.0).
- Space imagery: NASA EPIC (DSCOVR) and NASA GIBS / Worldview (MODIS Terra), public domain.
- Statistics: NOAA (Mauna Loa CO₂), NSIDC Sea Ice Index, HadCRUT5 / WMO, USGS Mineral Commodity Summaries, Energy Institute Statistical Review, Kimberley Process, UN and long-run population estimates, FAO forest data.
- 3D models: Horse, Flamingo, Parrot and Stork from the three.js examples (mirada / ro.me, MIT). Fox from the Khronos glTF Sample Assets (CC0 model by PixelMannen, rigging CC-BY 4.0 by @tomkranis). All other animals are generated and rigged procedurally in `js/animals3d.js`.
- Newspapers: Library of Congress, Chronicling America (public domain scans). Events near a place: Wikidata (CC0). Earthquakes: USGS. Natural events: NASA EONET. ISS position: wheretheiss.at.
- Libraries: [globe.gl](https://github.com/vasturiano/globe.gl) and [three.js](https://threejs.org/). Icons: [Lucide](https://lucide.dev) (ISC). Fonts: Public Sans and Newsreader (SIL Open Font License).

## Design tooling

The project has [Impeccable](https://github.com/pbakaus/impeccable) design skills installed for Claude Code in `.claude/`. Its platform binary is git-ignored; run `npx impeccable install` to restore it. Running `/impeccable init` records product context in `PRODUCT.md` for future design passes.

**Accuracy:** animal ranges, ice sheets and lakes are drawn as simplified ellipses, and prehistoric figures (population, CO₂, sea level, forest area) are rough scientific estimates. Treat them as illustrations, not precise maps.

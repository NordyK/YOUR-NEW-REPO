// Time-machine data for the Earth explorer: -20000 (20,000 BC) … 2026. Negative years = BC.
// Sources: UN World Population Prospects 2024 & HYDE/McEvedy–Jones (population); EPICA/Law Dome ice cores
// and NOAA Mauna Loa annual means (CO₂); Tierney et al. 2020, IPCC AR6, HadCRUT5 & WMO (temperature);
// Lambeck et al. 2014 / IPCC AR6 (sea level); NSIDC Sea Ice Index v4 September extent (sea ice);
// FAO Forest Resources Assessment 2020 & Our World in Data (forest).

// Global indicators; the app linearly interpolates between points.
export const STATS = {
  population: {
    label: 'World population', unit: '',
    points: [
      [-20000, 1.0e6], [-10000, 4.0e6], [-8000, 5.0e6], [-5000, 5.0e6], [-4000, 7.0e6], [-3000, 14e6],
      [-2000, 27e6], [-1000, 50e6], [-500, 100e6], [-200, 150e6], [1, 230e6], [500, 210e6], [1000, 295e6],
      [1250, 400e6], [1340, 440e6], [1400, 370e6], [1500, 460e6], [1600, 560e6], [1700, 610e6],
      [1750, 790e6], [1800, 1.0e9], [1850, 1.26e9], [1900, 1.65e9], [1927, 2.0e9], [1950, 2.50e9],
      [1960, 3.02e9], [1965, 3.33e9], [1970, 3.70e9], [1975, 4.07e9], [1980, 4.44e9], [1985, 4.86e9],
      [1990, 5.33e9], [1995, 5.74e9], [2000, 6.15e9], [2005, 6.56e9], [2010, 6.99e9], [2015, 7.43e9],
      [2020, 7.84e9], [2022, 8.00e9], [2025, 8.23e9], [2026, 8.30e9]
    ]
  },
  co2: {
    label: 'Atmospheric CO₂', unit: 'ppm',
    points: [
      [-20000, 189], [-16000, 191], [-14500, 205], [-13000, 225], [-12600, 238], [-11000, 238],
      [-10500, 245], [-9700, 262], [-8000, 265], [-6000, 260], [-4000, 264], [-2000, 272], [1, 277],
      [1000, 280], [1300, 282], [1610, 272], [1750, 278], [1800, 283], [1850, 285], [1900, 296],
      [1925, 305], [1950, 311], [1959, 316.0], [1960, 316.9], [1965, 320.0], [1970, 325.7], [1975, 331.1],
      [1980, 338.8], [1985, 346.1], [1990, 354.4], [1995, 360.8], [2000, 369.7], [2005, 380.0],
      [2010, 390.1], [2015, 401.0], [2016, 404.4], [2017, 406.8], [2018, 408.7], [2019, 411.7],
      [2020, 414.2], [2021, 416.4], [2022, 418.5], [2023, 421.1], [2024, 424.6], [2025, 427.4],
      [2026, 430.0]
    ]
  },
  temp: {
    label: 'Global temperature vs 1850–1900', unit: '°C',
    points: [
      [-20000, -6.1], [-17000, -5.8], [-15000, -5.0], [-12700, -3.5], [-11000, -3.8], [-9700, -1.8],
      [-8000, -0.6], [-6000, 0.2], [-4500, 0.2], [-2000, 0.0], [1, 0.0], [1000, 0.0], [1300, -0.05],
      [1600, -0.25], [1700, -0.2], [1800, -0.15], [1850, 0], [1880, 0.05], [1900, 0.0], [1910, -0.15],
      [1920, 0.03], [1930, 0.15], [1940, 0.37], [1950, 0.24], [1960, 0.31], [1970, 0.24], [1980, 0.47],
      [1990, 0.61], [2000, 0.8], [2010, 0.92], [2015, 1.18], [2016, 1.28], [2017, 1.19], [2018, 1.11],
      [2019, 1.24], [2020, 1.27], [2021, 1.12], [2022, 1.16], [2023, 1.45], [2024, 1.55], [2025, 1.44],
      [2026, 1.42]
    ]
  },
  seaLevel: {
    label: 'Sea level vs today', unit: 'm',
    points: [
      [-20000, -120], [-17000, -118], [-14000, -105], [-12700, -98], [-12300, -78], [-11000, -70],
      [-10000, -60], [-9500, -55], [-8000, -38], [-7000, -24], [-6000, -14], [-5000, -6], [-4000, -3],
      [-2000, -1.0], [1, -0.4], [1800, -0.25], [1900, -0.21], [1950, -0.15], [1993, -0.1], [2000, -0.088],
      [2010, -0.055], [2015, -0.04], [2020, -0.018], [2025, 0], [2026, 0.004]
    ]
  },
  seaIce: {
    label: 'Arctic sea-ice minimum', unit: 'M km²',
    // NSIDC Sea Ice Index v4, September monthly mean extent
    points: [
      [1979, 7.05], [1980, 7.67], [1981, 7.14], [1982, 7.30], [1983, 7.39], [1984, 6.81], [1985, 6.70],
      [1986, 7.41], [1987, 7.28], [1988, 7.37], [1989, 7.01], [1990, 6.14], [1991, 6.47], [1992, 7.47],
      [1993, 6.40], [1994, 7.14], [1995, 6.08], [1996, 7.58], [1997, 6.69], [1998, 6.54], [1999, 6.12],
      [2000, 6.25], [2001, 6.73], [2002, 5.83], [2003, 6.12], [2004, 5.98], [2005, 5.50], [2006, 5.86],
      [2007, 4.27], [2008, 4.69], [2009, 5.26], [2010, 4.87], [2011, 4.56], [2012, 3.57], [2013, 5.21],
      [2014, 5.22], [2015, 4.62], [2016, 4.53], [2017, 4.82], [2018, 4.79], [2019, 4.36], [2020, 4.00],
      [2021, 4.95], [2022, 4.90], [2023, 4.38], [2024, 4.35], [2025, 4.75], [2026, 4.81]
    ]
  },
  forest: {
    label: 'Global forest area', unit: 'B ha',
    points: [
      [-10000, 6.0], [-3000, 5.9], [1, 5.8], [1000, 5.6], [1500, 5.5], [1700, 5.4], [1800, 5.25],
      [1900, 5.0], [1950, 4.6], [1990, 4.24], [2000, 4.16], [2010, 4.11], [2015, 4.08], [2020, 4.06],
      [2025, 4.04]
    ]
  }
};

// Eras shown as a caption for the selected year (contiguous, covering -20000..2026)
export const ERAS = [
  { from: -20000, to: -11700, title: 'Last Glacial Maximum → Late Pleistocene', text: 'Ice sheets up to 3–4 km thick cover Canada, northern Europe and Patagonia, and sea level is about 120 m lower than today. Land bridges join Siberia to Alaska, Britain to Europe and Australia to New Guinea, while mammoths and woolly rhinos roam cold, dry steppes. A few million hunter-gatherers live on every continent except Antarctica.' },
  { from: -11700, to: -9500, title: 'The Holocene begins', text: 'After the Younger Dryas cold snap, temperatures jump in just decades and the current warm interglacial — the Holocene — begins. Melting ice raises seas rapidly, flooding Beringia and the continental shelves. Many large Ice Age mammals vanish.' },
  { from: -9500, to: -6000, title: 'Neolithic revolution', text: 'People in the Fertile Crescent begin farming wheat and barley and herding sheep and goats; rice, millet, maize and squash are domesticated independently elsewhere. Permanent villages such as Jericho and Çatalhöyük appear. The Sahara is green, dotted with lakes and grasslands.' },
  { from: -6000, to: -3300, title: 'Copper Age & first cities', text: 'Sea level stabilises near today\'s level and the Holocene Climatic Optimum keeps the northern hemisphere mild. Copper smelting spreads and the first cities rise in Mesopotamia, culminating in Uruk. Towards the end of this era the Sahara begins drying into desert.' },
  { from: -3300, to: -1200, title: 'Bronze Age', text: 'Writing is invented in Sumer and Egypt; pyramids, ziggurats and the cities of the Indus Valley flourish. Bronze tools and weapons transform warfare and trade across Eurasia. Around 2200 BC a severe drought (the 4.2-kiloyear event) strains early empires.' },
  { from: -1200, to: 300, title: 'Iron Age & classical antiquity', text: 'After the Late Bronze Age collapse, iron working spreads widely. Greek city-states, the Persian, Mauryan and Han empires, and finally Rome dominate, linked by the Silk Road. Philosophy, democracy, coinage and monumental engineering flourish.' },
  { from: 300, to: 600, title: 'Late antiquity', text: 'The Western Roman Empire fragments while Byzantium endures. A sharp cooling after volcanic eruptions in 536 and 540 and the Plague of Justinian hit populations hard. Christianity and, soon, Islam reshape the Mediterranean world.' },
  { from: 600, to: 1450, title: 'Middle Ages', text: 'Islamic caliphates, Tang and Song China, and medieval Europe rise; the Medieval Warm Period lets Vikings settle Greenland. The Mongols forge the largest contiguous land empire in history. The Black Death of 1347–51 kills perhaps a third of Europe\'s population.' },
  { from: 1450, to: 1600, title: 'Age of Exploration', text: 'Printing spreads knowledge, and European ships reach the Americas, round Africa and circle the globe. The Columbian Exchange swaps crops, animals and diseases between hemispheres, devastating Indigenous American populations. The Little Ice Age brings colder conditions.' },
  { from: 1600, to: 1760, title: 'Early modern era', text: 'The Scientific Revolution — Galileo, Kepler, Newton — reshapes how people understand nature. Global trading companies and colonial empires expand, along with the Atlantic slave trade. The Little Ice Age is at its coldest during the 1600s.' },
  { from: 1760, to: 1914, title: 'Industrial Revolution', text: 'Coal-powered steam engines, factories and railways transform Britain and then the world. Atmospheric CO₂ starts to climb from about 280 ppm. World population passes one billion around 1800 and cities grow explosively.' },
  { from: 1914, to: 1945, title: 'World Wars', text: 'Two world wars and the 1918 influenza pandemic kill tens of millions. Oil, cars, aircraft and electricity spread rapidly, while the Dust Bowl shows the cost of poor land management. The era ends with the first use of nuclear weapons.' },
  { from: 1945, to: 1969, title: 'The Great Acceleration', text: 'Population, energy use, fertiliser, plastics and travel soar after 1950 — the proposed start of the "Anthropocene". The Keeling curve begins recording rising CO₂ at Mauna Loa in 1958. The Space Race begins with Sputnik in 1957.' },
  { from: 1969, to: 2000, title: 'Space age & digital age', text: 'Humans walk on the Moon and satellites reveal the whole Earth. Personal computers and the internet connect the planet, while the ozone hole and rising temperatures spark global environmental agreements. Arctic sea ice begins a steep decline.' },
  { from: 2000, to: 2026, title: 'The present', text: 'More than 8 billion people live on an Earth about 1.3–1.5 °C warmer than in the 1800s, with CO₂ above 420 ppm. Record heat, wildfires, floods and coral bleaching become frequent, while renewables and electric vehicles grow rapidly. Deforestation, sea-level rise and biodiversity loss continue.' }
];

// Point events placed on the globe.
// types: eruption | earthquake | tsunami | climate | civilization | exploration | discovery | disaster | conflict | space | environment
export const EVENTS = [
  // ---- Late Pleistocene ----
  { year: -20000, name: 'Last Glacial Maximum', type: 'climate', lat: 60.0, lng: -85.0, summary: 'Ice sheets reach their greatest extent; the Laurentide ice over Hudson Bay is more than 3 km thick and global temperature is about 6 °C below pre-industrial.', wiki: 'Last_Glacial_Maximum' },
  { year: -19500, name: 'White Sands footprints', type: 'exploration', lat: 32.88, lng: -106.3, summary: 'Human footprints at White Sands, New Mexico, dated to about 23,000–21,000 years ago, suggest people reached the Americas during the Ice Age.', wiki: 'White_Sands_fossil_footprints' },
  { year: -15000, name: 'Lascaux cave paintings', type: 'civilization', lat: 45.054, lng: 1.168, summary: 'Ice Age artists paint hundreds of horses, aurochs and stags in the Lascaux caves of south-west France, about 17,000 years ago.', wiki: 'Lascaux' },
  { year: -12700, name: 'Bølling–Allerød warming', type: 'climate', lat: 72.58, lng: -38.46, summary: 'Greenland ice cores record abrupt warming of many degrees within decades about 14,700 years ago, as Meltwater Pulse 1A raises seas ~20 m.', wiki: 'Bølling–Allerød_warming' },
  { year: -12500, name: 'Monte Verde settlement', type: 'civilization', lat: -41.5, lng: -73.2, summary: 'A camp in southern Chile occupied ~14,500 years ago overturned the idea that the Clovis people were the first Americans.', wiki: 'Monte_Verde' },
  { year: -11000, name: 'Clovis culture', type: 'civilization', lat: 34.28, lng: -103.33, summary: 'Hunters with distinctive fluted spear points spread across North America; named after finds at Blackwater Draw near Clovis, New Mexico.', wiki: 'Clovis_culture' },
  { year: -10900, name: 'Younger Dryas cold snap', type: 'climate', lat: 75.1, lng: -42.32, summary: 'About 12,900 years ago the North Atlantic plunges back into near-glacial cold for ~1,200 years, possibly after meltwater disrupted ocean circulation.', wiki: 'Younger_Dryas' },
  // ---- Holocene & Neolithic ----
  { year: -9700, name: 'Holocene begins', type: 'climate', lat: 75.1, lng: -42.32, summary: 'The Younger Dryas ends abruptly 11,700 years ago — the official start of the Holocene epoch, defined in the NGRIP Greenland ice core.', wiki: 'Holocene' },
  { year: -9500, name: 'Göbekli Tepe', type: 'civilization', lat: 37.223, lng: 38.922, summary: 'Hunter-gatherers raise massive carved T-shaped stone pillars in south-east Turkey — the oldest known monumental architecture.', wiki: 'Göbekli_Tepe' },
  { year: -9000, name: 'Beringia flooded', type: 'climate', lat: 65.7, lng: -168.9, summary: 'Rising seas finally submerge the Bering land bridge around 11,000 years ago, separating Asia and North America.', wiki: 'Beringia' },
  { year: -9000, name: 'Jericho settled', type: 'civilization', lat: 31.871, lng: 35.444, summary: 'One of the oldest continuously inhabited towns; by ~8000 BC it had a stone wall and tower.', wiki: 'Jericho' },
  { year: -8500, name: 'Farming in the Fertile Crescent', type: 'discovery', lat: 36.5, lng: 40.5, summary: 'Wheat, barley, lentils, sheep and goats are domesticated in the arc from the Levant to the Zagros — the start of agriculture.', wiki: 'Fertile_Crescent' },
  { year: -7100, name: 'Çatalhöyük', type: 'civilization', lat: 37.668, lng: 32.828, summary: 'A densely packed Neolithic town in Anatolia where up to several thousand people lived in houses entered through the roof.', wiki: 'Çatalhöyük' },
  { year: -7000, name: 'Mehrgarh', type: 'civilization', lat: 29.39, lng: 67.62, summary: 'Early farming village in Balochistan, a forerunner of the Indus Valley Civilisation.', wiki: 'Mehrgarh' },
  { year: -7000, name: 'Jiahu', type: 'civilization', lat: 33.6, lng: 113.67, summary: 'Neolithic village in China\'s Yellow River valley with early rice cultivation, bone flutes and possibly the oldest fermented drink.', wiki: 'Jiahu' },
  { year: -6500, name: 'Doggerland submerged', type: 'climate', lat: 54.5, lng: 2.5, summary: 'The low plains linking Britain to mainland Europe disappear under the rising North Sea; Britain becomes an island.', wiki: 'Doggerland' },
  { year: -6250, name: 'Lake Agassiz drains (8.2 ka event)', type: 'climate', lat: 58.0, lng: -85.0, summary: 'The giant glacial lake bursts into Hudson Bay, triggering a ~150-year cold event around 8,200 years ago.', wiki: '8.2-kiloyear_event' },
  { year: -6150, name: 'Storegga Slide tsunami', type: 'tsunami', lat: 64.0, lng: 4.0, summary: 'A huge underwater landslide off Norway sends a tsunami across the North Sea, swamping coasts of Scotland and the last of Doggerland.', wiki: 'Storegga_Slide' },
  { year: -5700, name: 'Mount Mazama collapses', type: 'eruption', lat: 42.94, lng: -122.11, summary: 'A massive eruption in Oregon forms the caldera now filled by Crater Lake, the deepest lake in the USA.', wiki: 'Mount_Mazama' },
  // ---- Copper/Bronze Age ----
  { year: -3500, name: 'Uruk and the first writing', type: 'civilization', lat: 31.322, lng: 45.636, summary: 'Uruk in Sumer becomes perhaps the largest city on Earth; cuneiform writing develops here around 3300–3200 BC.', wiki: 'Uruk' },
  { year: -3300, name: 'Ötzi the Iceman', type: 'discovery', lat: 46.779, lng: 10.840, summary: 'A Copper Age man dies in the Ötztal Alps; his glacier-mummified body was found in 1991.', wiki: 'Ötzi' },
  { year: -3100, name: 'Unification of Egypt', type: 'civilization', lat: 29.845, lng: 31.25, summary: 'Upper and Lower Egypt are united under the first pharaohs, with Memphis as capital.', wiki: 'Early_Dynastic_Period_(Egypt)' },
  { year: -3000, name: 'Stonehenge begun', type: 'civilization', lat: 51.179, lng: -1.826, summary: 'Construction of the circular earthwork begins; the great sarsen stones are raised around 2500 BC.', wiki: 'Stonehenge' },
  { year: -2600, name: 'Caral', type: 'civilization', lat: -10.893, lng: -77.52, summary: 'Caral, in Peru\'s Supe Valley, is one of the oldest cities in the Americas, with large platform mounds.', wiki: 'Caral' },
  { year: -2560, name: 'Great Pyramid of Giza', type: 'civilization', lat: 29.979, lng: 31.134, summary: 'Built for Pharaoh Khufu, it remained the tallest human-made structure for nearly 4,000 years.', wiki: 'Great_Pyramid_of_Giza' },
  { year: -2500, name: 'Mohenjo-daro', type: 'civilization', lat: 27.329, lng: 68.138, summary: 'A planned Indus Valley city with grid streets, drains and the Great Bath.', wiki: 'Mohenjo-daro' },
  { year: -2200, name: '4.2-kiloyear drought', type: 'climate', lat: 36.96, lng: 41.5, summary: 'A centuries-long megadrought coincides with the collapse of the Akkadian Empire and the Old Kingdom of Egypt.', wiki: '4.2-kiloyear_event' },
  { year: -2000, name: 'Last mammoths on Wrangel Island', type: 'environment', lat: 71.2, lng: -179.5, summary: 'The final woolly mammoths die out on this Arctic island around 4,000 years ago — after the pyramids were built.', wiki: 'Wrangel_Island' },
  { year: -1600, name: 'Thera (Santorini) eruption', type: 'eruption', lat: 36.404, lng: 25.396, summary: 'One of the largest eruptions of the Holocene blows apart the island of Thera and buries the Minoan town of Akrotiri.', wiki: 'Minoan_eruption' },
  { year: -1250, name: 'Shang capital at Yin (Anyang)', type: 'civilization', lat: 36.13, lng: 114.31, summary: 'Oracle-bone inscriptions here are the earliest known Chinese writing.', wiki: 'Yinxu' },
  { year: -1200, name: 'Olmec San Lorenzo', type: 'civilization', lat: 17.75, lng: -94.76, summary: 'The first major Olmec centre, famous for its colossal stone heads.', wiki: 'San_Lorenzo_Tenochtitlán' },
  { year: -1185, name: 'Late Bronze Age collapse', type: 'conflict', lat: 35.602, lng: 35.782, summary: 'Ugarit and many other Eastern Mediterranean cities are destroyed as Bronze Age trade networks collapse.', wiki: 'Late_Bronze_Age_collapse' },
  // ---- Iron Age & antiquity ----
  { year: -776, name: 'First Olympic Games', type: 'civilization', lat: 37.638, lng: 21.63, summary: 'The traditional date of the first Olympic Games, held at Olympia in honour of Zeus.', wiki: 'Ancient_Olympic_Games' },
  { year: -753, name: 'Founding of Rome', type: 'civilization', lat: 41.893, lng: 12.483, summary: 'The legendary founding of Rome by Romulus on the Palatine Hill.', wiki: 'Founding_of_Rome' },
  { year: -550, name: 'Persian Empire founded', type: 'civilization', lat: 30.2, lng: 53.18, summary: 'Cyrus the Great founds the Achaemenid Empire, ruled from Pasargadae and later Persepolis.', wiki: 'Achaemenid_Empire' },
  { year: -447, name: 'Parthenon built', type: 'civilization', lat: 37.971, lng: 23.726, summary: 'Athens at its classical height builds the Parthenon on the Acropolis.', wiki: 'Parthenon' },
  { year: -331, name: 'Alexandria founded', type: 'civilization', lat: 31.2, lng: 29.92, summary: 'Alexander the Great founds Alexandria, later home of the Great Library and the Pharos lighthouse.', wiki: 'Alexandria' },
  { year: -323, name: 'Death of Alexander the Great', type: 'conflict', lat: 32.54, lng: 44.42, summary: 'Alexander dies in Babylon aged 32, his empire stretching from Greece to India.', wiki: 'Alexander_the_Great' },
  { year: -221, name: 'Qin unifies China', type: 'civilization', lat: 34.38, lng: 109.25, summary: 'Qin Shi Huang becomes the first emperor of China; his tomb holds the Terracotta Army.', wiki: 'Qin_dynasty' },
  { year: -214, name: 'Great Wall of Qin', type: 'civilization', lat: 40.43, lng: 116.57, summary: 'The Qin link earlier walls into a frontier against the Xiongnu; most of today\'s wall is from the Ming dynasty.', wiki: 'Great_Wall_of_China' },
  { year: -146, name: 'Destruction of Carthage', type: 'conflict', lat: 36.853, lng: 10.323, summary: 'Rome razes Carthage at the end of the Third Punic War.', wiki: 'Battle_of_Carthage_(c._149_BC)' },
  { year: -130, name: 'Silk Road opens', type: 'exploration', lat: 40.14, lng: 94.66, summary: 'After Zhang Qian\'s missions, Han China opens trade routes west through Dunhuang to Central Asia and beyond.', wiki: 'Silk_Road' },
  { year: 79, name: 'Eruption of Mount Vesuvius', type: 'eruption', lat: 40.821, lng: 14.426, summary: 'Pompeii and Herculaneum are buried under ash and pyroclastic flows, preserving a Roman town in remarkable detail.', wiki: 'Eruption_of_Mount_Vesuvius_in_79_AD' },
  { year: 200, name: 'Teotihuacan at its height', type: 'civilization', lat: 19.692, lng: -98.844, summary: 'With perhaps 100,000 people, Teotihuacan is one of the largest cities in the world; the Pyramid of the Sun dominates it.', wiki: 'Teotihuacan' },
  { year: 232, name: 'Taupō Hatepe eruption', type: 'eruption', lat: -38.82, lng: 175.9, summary: 'One of the most violent eruptions of the last 5,000 years devastates New Zealand\'s North Island (then uninhabited).', wiki: 'Hatepe_eruption' },
  // ---- Late antiquity & Middle Ages ----
  { year: 476, name: 'Fall of the Western Roman Empire', type: 'conflict', lat: 44.418, lng: 12.2, summary: 'Odoacer deposes the last western emperor, Romulus Augustulus, at Ravenna.', wiki: 'Fall_of_the_Western_Roman_Empire' },
  { year: 536, name: 'Volcanic winter of 536', type: 'climate', lat: 41.008, lng: 28.98, summary: 'A dust veil dims the sun for over a year; Byzantine historian Procopius describes a sun "without brightness". It begins the Late Antique Little Ice Age.', wiki: 'Volcanic_winter_of_536' },
  { year: 541, name: 'Plague of Justinian', type: 'disaster', lat: 41.008, lng: 28.98, summary: 'The first recorded bubonic plague pandemic strikes Constantinople and the Mediterranean world.', wiki: 'Plague_of_Justinian' },
  { year: 622, name: 'Hijra to Medina', type: 'civilization', lat: 24.47, lng: 39.61, summary: 'Muhammad\'s migration from Mecca to Medina marks year 1 of the Islamic calendar.', wiki: 'Hijrah' },
  { year: 762, name: 'Baghdad founded', type: 'civilization', lat: 33.315, lng: 44.366, summary: 'The Abbasid "Round City" becomes a centre of the Islamic Golden Age and its House of Wisdom.', wiki: 'Baghdad' },
  { year: 900, name: 'Classic Maya collapse', type: 'civilization', lat: 17.222, lng: -89.623, summary: 'Great lowland cities such as Tikal are abandoned amid repeated droughts, warfare and political breakdown.', wiki: 'Classic_Maya_collapse' },
  { year: 1000, name: 'Vikings reach North America', type: 'exploration', lat: 51.596, lng: -55.533, summary: 'Norse sailors led by Leif Erikson build a camp at L\'Anse aux Meadows, Newfoundland — 500 years before Columbus.', wiki: "L'Anse_aux_Meadows" },
  { year: 1150, name: 'Angkor Wat completed', type: 'civilization', lat: 13.412, lng: 103.867, summary: 'The Khmer capital of Angkor, the world\'s largest religious monument, sits at the heart of a vast water-managed city.', wiki: 'Angkor_Wat' },
  { year: 1200, name: 'Rapa Nui settled', type: 'exploration', lat: -27.12, lng: -109.35, summary: 'Polynesian voyagers reach Easter Island, later carving the famous moai statues.', wiki: 'Easter_Island' },
  { year: 1206, name: 'Mongol Empire founded', type: 'conflict', lat: 47.2, lng: 102.8, summary: 'Genghis Khan unites the Mongol tribes; the empire becomes the largest contiguous land empire in history.', wiki: 'Mongol_Empire' },
  { year: 1257, name: 'Samalas eruption', type: 'eruption', lat: -8.41, lng: 116.47, summary: 'A colossal eruption on Lombok, Indonesia, one of the largest of the Holocene, cools the global climate.', wiki: '1257_Samalas_eruption' },
  { year: 1280, name: 'Māori settle New Zealand', type: 'exploration', lat: -41.5, lng: 174.05, summary: 'Polynesians reach Aotearoa, the last major landmass settled by humans; Wairau Bar is among the earliest sites.', wiki: 'Wairau_Bar' },
  { year: 1300, name: 'Great Zimbabwe', type: 'civilization', lat: -20.27, lng: 30.93, summary: 'A powerful southern African kingdom builds massive dry-stone walls, trading gold to the Indian Ocean coast.', wiki: 'Great_Zimbabwe' },
  { year: 1325, name: 'Tenochtitlan founded', type: 'civilization', lat: 19.435, lng: -99.131, summary: 'The Aztecs found their island capital in Lake Texcoco, site of today\'s Mexico City.', wiki: 'Tenochtitlan' },
  { year: 1347, name: 'Black Death reaches Europe', type: 'disaster', lat: 38.19, lng: 15.55, summary: 'Plague arrives at Messina, Sicily, aboard Genoese ships, and kills perhaps a third to half of Europe\'s population by 1351.', wiki: 'Black_Death' },
  { year: 1405, name: 'Zheng He\'s treasure fleets', type: 'exploration', lat: 32.06, lng: 118.8, summary: 'Ming admiral Zheng He sails from Nanjing on the first of seven voyages that reach Arabia and East Africa.', wiki: 'Zheng_He' },
  { year: 1450, name: 'Gutenberg printing press', type: 'discovery', lat: 49.99, lng: 8.27, summary: 'Johannes Gutenberg\'s movable-type press in Mainz transforms the spread of knowledge.', wiki: 'Printing_press' },
  { year: 1450, name: 'Machu Picchu built', type: 'civilization', lat: -13.163, lng: -72.545, summary: 'The Inca build a royal estate high on an Andean ridge, probably for the emperor Pachacuti.', wiki: 'Machu_Picchu' },
  { year: 1453, name: 'Fall of Constantinople', type: 'conflict', lat: 41.008, lng: 28.98, summary: 'The Ottomans capture Constantinople, ending the Byzantine Empire.', wiki: 'Fall_of_Constantinople' },
  // ---- Age of Exploration & early modern ----
  { year: 1492, name: 'Columbus reaches the Americas', type: 'exploration', lat: 24.05, lng: -74.5, summary: 'Columbus makes landfall in the Bahamas (probably San Salvador Island), beginning sustained contact between Europe and the Americas.', wiki: 'Voyages_of_Christopher_Columbus' },
  { year: 1498, name: 'Vasco da Gama reaches India', type: 'exploration', lat: 11.25, lng: 75.78, summary: 'The Portuguese reach Calicut by sailing round Africa, opening a sea route to Asia.', wiki: 'Vasco_da_Gama' },
  { year: 1520, name: 'Magellan crosses into the Pacific', type: 'exploration', lat: -53.5, lng: -70.9, summary: 'Magellan\'s fleet threads the strait now named after him; the survivors complete the first circumnavigation in 1522.', wiki: 'Magellan_expedition' },
  { year: 1556, name: 'Shaanxi earthquake', type: 'earthquake', lat: 34.5, lng: 109.7, summary: 'The deadliest earthquake in recorded history kills about 830,000 people, many in collapsed loess cave dwellings.', wiki: '1556_Shaanxi_earthquake' },
  { year: 1600, name: 'Huaynaputina eruption', type: 'eruption', lat: -16.61, lng: -70.85, summary: 'The largest eruption in South American recorded history; it contributed to the Russian famine of 1601–03.', wiki: '1600_Huaynaputina_eruption' },
  { year: 1607, name: 'Jamestown founded', type: 'exploration', lat: 37.21, lng: -76.78, summary: 'The first permanent English settlement in the Americas is founded in Virginia.', wiki: 'Jamestown,_Virginia' },
  { year: 1610, name: 'Galileo\'s telescope', type: 'discovery', lat: 45.4, lng: 11.88, summary: 'From Padua, Galileo discovers Jupiter\'s moons and the phases of Venus, supporting a Sun-centred cosmos.', wiki: 'Sidereus_Nuncius' },
  { year: 1687, name: 'Newton\'s Principia', type: 'discovery', lat: 52.205, lng: 0.118, summary: 'Isaac Newton publishes the laws of motion and universal gravitation.', wiki: 'Philosophiæ_Naturalis_Principia_Mathematica' },
  { year: 1709, name: 'Coke-fired iron at Coalbrookdale', type: 'discovery', lat: 52.64, lng: -2.48, summary: 'Abraham Darby smelts iron with coke instead of charcoal, a key step towards the Industrial Revolution.', wiki: 'Coalbrookdale' },
  { year: 1755, name: 'Lisbon earthquake', type: 'earthquake', lat: 38.71, lng: -9.14, summary: 'A great earthquake, tsunami and fires destroy Lisbon on All Saints\' Day, killing tens of thousands.', wiki: '1755_Lisbon_earthquake' },
  // ---- Industrial Revolution ----
  { year: 1770, name: 'Cook reaches Australia', type: 'exploration', lat: -34.0, lng: 151.2, summary: 'James Cook lands at Botany Bay during his first Pacific voyage aboard the Endeavour.', wiki: "James_Cook's_first_voyage" },
  { year: 1783, name: 'Laki eruption', type: 'eruption', lat: 64.07, lng: -18.23, summary: 'An 8-month fissure eruption in Iceland releases poisonous gases, killing about a fifth of Iceland\'s people and causing a deadly haze across Europe.', wiki: 'Laki' },
  { year: 1815, name: 'Tambora eruption', type: 'eruption', lat: -8.25, lng: 118.0, summary: 'The largest eruption in recorded history leads to 1816, the "Year Without a Summer", with crop failures across the Northern Hemisphere.', wiki: '1815_eruption_of_Mount_Tambora' },
  { year: 1835, name: 'Darwin in the Galápagos', type: 'discovery', lat: -0.8, lng: -91.0, summary: 'HMS Beagle visits the Galápagos; observations here help Darwin develop the theory of evolution by natural selection.', wiki: 'Second_voyage_of_HMS_Beagle' },
  { year: 1859, name: 'Drake Well — oil age begins', type: 'discovery', lat: 41.61, lng: -79.66, summary: 'Edwin Drake strikes oil at Titusville, Pennsylvania, launching the modern petroleum industry.', wiki: 'Drake_Well_Museum' },
  { year: 1869, name: 'Suez Canal opens', type: 'civilization', lat: 30.6, lng: 32.3, summary: 'The canal links the Mediterranean and Red Sea, cutting the voyage from Europe to Asia by thousands of kilometres.', wiki: 'Suez_Canal' },
  { year: 1883, name: 'Krakatoa eruption', type: 'eruption', lat: -6.102, lng: 105.423, summary: 'Explosions heard 4,800 km away and tsunamis kill over 36,000 people; vivid red sunsets follow worldwide.', wiki: '1883_eruption_of_Krakatoa' },
  { year: 1902, name: 'Mount Pelée eruption', type: 'eruption', lat: 14.81, lng: -61.17, summary: 'A pyroclastic flow destroys the town of Saint-Pierre on Martinique, killing about 28,000 people.', wiki: '1902_eruption_of_Mount_Pelée' },
  { year: 1903, name: 'First powered flight', type: 'discovery', lat: 36.019, lng: -75.668, summary: 'The Wright brothers make the first controlled, powered aeroplane flight at Kitty Hawk, North Carolina.', wiki: 'Wright_Flyer' },
  { year: 1906, name: 'San Francisco earthquake', type: 'earthquake', lat: 37.75, lng: -122.55, summary: 'A magnitude 7.9 rupture of the San Andreas Fault and the fires that followed destroy much of San Francisco.', wiki: '1906_San_Francisco_earthquake' },
  { year: 1908, name: 'Tunguska event', type: 'disaster', lat: 60.886, lng: 101.894, summary: 'An exploding meteoroid or comet fragment flattens about 80 million trees over 2,150 km² of Siberian forest.', wiki: 'Tunguska_event' },
  { year: 1911, name: 'Amundsen reaches the South Pole', type: 'exploration', lat: -89.99, lng: 0, summary: 'Roald Amundsen\'s Norwegian party is first to the South Pole on 14 December 1911.', wiki: 'Amundsen\'s_South_Pole_expedition' },
  { year: 1912, name: 'Sinking of the Titanic', type: 'disaster', lat: 41.726, lng: -49.948, summary: 'The "unsinkable" liner strikes an iceberg on its maiden voyage; more than 1,500 people die.', wiki: 'Titanic' },
  // ---- World Wars ----
  { year: 1916, name: 'Battle of the Somme', type: 'conflict', lat: 50.0, lng: 2.7, summary: 'One of the bloodiest battles in history, with over a million casualties on the Western Front.', wiki: 'Battle_of_the_Somme' },
  { year: 1918, name: '1918 influenza pandemic', type: 'disaster', lat: 39.06, lng: -96.77, summary: 'The "Spanish flu" infects about a third of humanity and kills an estimated 17–50 million people; early cases were recorded at Camp Funston, Kansas.', wiki: 'Spanish_flu' },
  { year: 1923, name: 'Great Kantō earthquake', type: 'earthquake', lat: 35.33, lng: 139.14, summary: 'A magnitude 7.9 quake and firestorms devastate Tokyo and Yokohama, killing over 100,000 people.', wiki: '1923_Great_Kantō_earthquake' },
  { year: 1928, name: 'Penicillin discovered', type: 'discovery', lat: 51.517, lng: -0.174, summary: 'Alexander Fleming notices mould killing bacteria at St Mary\'s Hospital, London — the dawn of antibiotics.', wiki: 'Penicillin' },
  { year: 1931, name: 'China floods', type: 'disaster', lat: 30.59, lng: 114.3, summary: 'Yangtze and Huai river floods are among the deadliest natural disasters ever recorded.', wiki: '1931_China_floods' },
  { year: 1935, name: 'Dust Bowl', type: 'environment', lat: 36.5, lng: -101.5, summary: 'Drought and ploughed-up prairie produce giant dust storms such as "Black Sunday" (14 April 1935) across the southern Great Plains.', wiki: 'Dust_Bowl' },
  { year: 1943, name: 'Parícutin is born', type: 'eruption', lat: 19.493, lng: -102.251, summary: 'A volcano erupts from a Mexican cornfield and grows over 300 m in a year — the first volcano observed from birth.', wiki: 'Parícutin' },
  { year: 1945, name: 'Atomic bombing of Hiroshima', type: 'conflict', lat: 34.395, lng: 132.454, summary: 'The first nuclear weapon used in war destroys Hiroshima on 6 August 1945; Nagasaki follows three days later.', wiki: 'Atomic_bombings_of_Hiroshima_and_Nagasaki' },
  // ---- Great Acceleration ----
  { year: 1957, name: 'Sputnik 1', type: 'space', lat: 45.92, lng: 63.34, summary: 'The Soviet Union launches the first artificial satellite from Baikonur, starting the Space Age.', wiki: 'Sputnik_1' },
  { year: 1958, name: 'Keeling Curve begins', type: 'discovery', lat: 19.536, lng: -155.576, summary: 'Charles David Keeling starts continuous CO₂ measurements at Mauna Loa, Hawaii, revealing the steady rise of greenhouse gases.', wiki: 'Keeling_Curve' },
  { year: 1960, name: 'Great Chilean earthquake', type: 'earthquake', lat: -38.24, lng: -73.05, summary: 'The most powerful earthquake ever recorded (magnitude 9.5) strikes near Valdivia, sending tsunamis across the Pacific.', wiki: '1960_Valdivia_earthquake' },
  { year: 1960, name: 'Aral Sea begins to shrink', type: 'environment', lat: 45.0, lng: 59.8, summary: 'Soviet irrigation schemes divert the Amu Darya and Syr Darya rivers; the world\'s fourth-largest lake starts to dry up.', wiki: 'Aral_Sea' },
  { year: 1961, name: 'First human in space', type: 'space', lat: 45.92, lng: 63.34, summary: 'Yuri Gagarin orbits Earth aboard Vostok 1, launched from Baikonur.', wiki: 'Vostok_1' },
  { year: 1963, name: 'Surtsey emerges', type: 'eruption', lat: 63.30, lng: -20.60, summary: 'A new volcanic island rises from the sea off Iceland and becomes a natural laboratory for colonisation by life.', wiki: 'Surtsey' },
  { year: 1964, name: 'Great Alaska earthquake', type: 'earthquake', lat: 61.02, lng: -147.65, summary: 'A magnitude 9.2 quake — the strongest in North American history — devastates Anchorage and Valdez.', wiki: '1964_Alaska_earthquake' },
  { year: 1969, name: 'Apollo 11 Moon landing', type: 'space', lat: 28.573, lng: -80.649, summary: 'Launched from Kennedy Space Center, Neil Armstrong and Buzz Aldrin become the first people to walk on the Moon.', wiki: 'Apollo_11' },
  // ---- Space & digital age ----
  { year: 1970, name: 'Bhola cyclone', type: 'disaster', lat: 22.3, lng: 90.7, summary: 'The deadliest tropical cyclone on record kills an estimated 300,000–500,000 people in East Pakistan (now Bangladesh).', wiki: '1970_Bhola_cyclone' },
  { year: 1972, name: 'Sahel drought', type: 'climate', lat: 14.0, lng: 2.0, summary: 'Decades of below-average rain cause famine across the Sahel in the 1970s and 1980s.', wiki: 'Sahel_drought' },
  { year: 1980, name: 'Mount St. Helens eruption', type: 'eruption', lat: 46.191, lng: -122.194, summary: 'A lateral blast after the largest landslide in recorded history removes the volcano\'s summit in Washington State.', wiki: '1980_eruption_of_Mount_St._Helens' },
  { year: 1985, name: 'Ozone hole discovered', type: 'environment', lat: -75.58, lng: -26.66, summary: 'British Antarctic Survey scientists at Halley report severe springtime ozone depletion over Antarctica.', wiki: 'Ozone_depletion' },
  { year: 1985, name: 'Lake Chad near its minimum', type: 'environment', lat: 13.3, lng: 14.1, summary: 'Drought and irrigation shrink Lake Chad by about 90% from its 1963 extent.', wiki: 'Lake_Chad' },
  { year: 1986, name: 'Chernobyl disaster', type: 'disaster', lat: 51.389, lng: 30.099, summary: 'Reactor 4 explodes, spreading radioactive fallout across Europe; a 2,600 km² exclusion zone remains.', wiki: 'Chernobyl_disaster' },
  { year: 1987, name: 'Montreal Protocol', type: 'environment', lat: 45.5, lng: -73.57, summary: 'Nations agree to phase out ozone-depleting CFCs — the first treaty ratified by every UN member state.', wiki: 'Montreal_Protocol' },
  { year: 1989, name: 'Exxon Valdez oil spill', type: 'environment', lat: 60.84, lng: -146.88, summary: 'A tanker runs aground in Prince William Sound, Alaska, spilling about 41,000 m³ of crude oil.', wiki: 'Exxon_Valdez_oil_spill' },
  { year: 1989, name: 'Fall of the Berlin Wall', type: 'conflict', lat: 52.516, lng: 13.377, summary: 'The wall opens on 9 November 1989, heralding the end of the Cold War.', wiki: 'Fall_of_the_Berlin_Wall' },
  { year: 1990, name: 'Hubble Space Telescope', type: 'space', lat: 28.608, lng: -80.604, summary: 'Space Shuttle Discovery launches Hubble from Kennedy Space Center.', wiki: 'Hubble_Space_Telescope' },
  { year: 1991, name: 'Mount Pinatubo eruption', type: 'eruption', lat: 15.13, lng: 120.35, summary: 'The second-largest eruption of the 20th century injects sulfur aerosols that cool global temperature by about 0.5 °C.', wiki: '1991_eruption_of_Mount_Pinatubo' },
  { year: 1991, name: 'Kuwaiti oil fires', type: 'environment', lat: 29.0, lng: 47.9, summary: 'Retreating Iraqi forces set over 600 oil wells ablaze; the fires burn for months.', wiki: 'Kuwaiti_oil_fires' },
  { year: 1992, name: 'Rio Earth Summit', type: 'environment', lat: -22.9, lng: -43.2, summary: 'The UN Framework Convention on Climate Change and the Convention on Biological Diversity open for signature.', wiki: 'Earth_Summit' },
  { year: 1995, name: 'Amazon deforestation peak', type: 'environment', lat: -10.0, lng: -63.0, summary: 'Brazil records its worst year of Amazon clearing — about 29,000 km² — concentrated in the "arc of deforestation".', wiki: 'Deforestation_of_the_Amazon_rainforest' },
  { year: 1997, name: 'Kyoto Protocol', type: 'environment', lat: 35.01, lng: 135.77, summary: 'The first treaty with binding greenhouse-gas cuts for industrialised nations is adopted in Kyoto.', wiki: 'Kyoto_Protocol' },
  { year: 1998, name: 'First global coral bleaching', type: 'environment', lat: -18.0, lng: 147.0, summary: 'A strong El Niño triggers the first recorded global mass coral bleaching event.', wiki: 'Coral_bleaching' },
  // ---- Present ----
  { year: 2003, name: 'European heatwave', type: 'climate', lat: 48.86, lng: 2.35, summary: 'Europe\'s hottest summer in centuries kills an estimated 70,000 people.', wiki: '2003_European_heat_wave' },
  { year: 2004, name: 'Indian Ocean earthquake & tsunami', type: 'tsunami', lat: 3.316, lng: 95.854, summary: 'A magnitude 9.1 quake off Sumatra sends tsunamis across the Indian Ocean, killing about 230,000 people in 14 countries.', wiki: '2004_Indian_Ocean_earthquake_and_tsunami' },
  { year: 2004, name: 'Amazon deforestation surge', type: 'environment', lat: -11.5, lng: -55.5, summary: 'Over 27,000 km² is cleared, largely for cattle and soy in Mato Grosso; enforcement then cut rates by ~80% by 2012.', wiki: 'Deforestation_in_Brazil' },
  { year: 2005, name: 'Hurricane Katrina', type: 'disaster', lat: 29.95, lng: -90.07, summary: 'Levee failures flood 80% of New Orleans; over 1,300 people die.', wiki: 'Hurricane_Katrina' },
  { year: 2010, name: 'Haiti earthquake', type: 'earthquake', lat: 18.457, lng: -72.533, summary: 'A magnitude 7.0 quake near Port-au-Prince kills well over 100,000 people.', wiki: '2010_Haiti_earthquake' },
  { year: 2010, name: 'Eyjafjallajökull eruption', type: 'eruption', lat: 63.63, lng: -19.62, summary: 'An ash cloud from Iceland shuts down European airspace for days, grounding about 100,000 flights.', wiki: '2010_eruptions_of_Eyjafjallajökull' },
  { year: 2010, name: 'Deepwater Horizon oil spill', type: 'environment', lat: 28.738, lng: -88.366, summary: 'A blowout in the Gulf of Mexico kills 11 workers and spills about 4.9 million barrels — the largest marine oil spill.', wiki: 'Deepwater_Horizon_oil_spill' },
  { year: 2011, name: 'Tōhoku earthquake & tsunami', type: 'tsunami', lat: 38.297, lng: 142.373, summary: 'A magnitude 9.0–9.1 quake off Japan sends a tsunami up to 40 m high, killing about 18,000 people and causing the Fukushima nuclear disaster.', wiki: '2011_Tōhoku_earthquake_and_tsunami' },
  { year: 2012, name: 'Record-low Arctic sea ice', type: 'climate', lat: 85.0, lng: -150.0, summary: 'September sea-ice extent falls to 3.57 million km², about half the 1979–1990 average — still the lowest on record.', wiki: 'Arctic_sea_ice_decline' },
  { year: 2015, name: 'Paris Agreement', type: 'environment', lat: 48.95, lng: 2.43, summary: 'Nearly every nation agrees to limit warming to well below 2 °C, pursuing 1.5 °C.', wiki: 'Paris_Agreement' },
  { year: 2016, name: 'Great Barrier Reef mass bleaching', type: 'environment', lat: -14.5, lng: 145.5, summary: 'Marine heatwaves kill about 30% of the reef\'s shallow-water corals, worst in the northern third; further bleaching follows in 2017, 2020, 2022, 2024 and 2025.', wiki: 'Great_Barrier_Reef' },
  { year: 2019, name: 'COVID-19 pandemic begins', type: 'disaster', lat: 30.59, lng: 114.3, summary: 'A novel coronavirus first identified in Wuhan spreads worldwide, causing millions of deaths.', wiki: 'COVID-19_pandemic' },
  { year: 2020, name: 'Australian Black Summer bushfires', type: 'environment', lat: -35.7, lng: 150.0, summary: 'The 2019–20 fires burn about 24 million hectares and kill or displace an estimated 3 billion animals.', wiki: '2019–20_Australian_bushfire_season' },
  { year: 2020, name: 'Siberian heat record', type: 'climate', lat: 67.55, lng: 133.39, summary: 'Verkhoyansk reaches 38 °C on 20 June 2020 — the highest temperature recorded north of the Arctic Circle.', wiki: 'Verkhoyansk' },
  { year: 2021, name: 'James Webb Space Telescope', type: 'space', lat: 5.236, lng: -52.77, summary: 'JWST launches on an Ariane 5 from Kourou, French Guiana, on 25 December 2021.', wiki: 'James_Webb_Space_Telescope' },
  { year: 2022, name: 'Hunga Tonga eruption', type: 'eruption', lat: -20.536, lng: -175.382, summary: 'The largest atmospheric explosion in modern records sends a plume 57 km high and pressure waves around the globe several times.', wiki: '2022_Hunga_Tonga–Hunga_Haʻapai_eruption_and_tsunami' },
  { year: 2022, name: 'Pakistan floods', type: 'disaster', lat: 26.5, lng: 68.0, summary: 'Record monsoon rains put about a third of Pakistan under water, affecting 33 million people.', wiki: '2022_Pakistan_floods' },
  { year: 2023, name: 'Turkey–Syria earthquake', type: 'earthquake', lat: 37.226, lng: 37.014, summary: 'A magnitude 7.8 quake and major aftershock kill over 55,000 people.', wiki: '2023_Turkey–Syria_earthquakes' },
  { year: 2023, name: 'Canada\'s record wildfire season', type: 'environment', lat: 50.0, lng: -75.0, summary: 'More than 15 million hectares burn — far above the previous record — sending smoke as far as New York and Europe.', wiki: '2023_Canadian_wildfires' },
  { year: 2023, name: 'Chandrayaan-3 lands near the Moon\'s south pole', type: 'space', lat: 13.72, lng: 80.23, summary: 'India becomes the first nation to land near the lunar south pole; launched from Sriharikota.', wiki: 'Chandrayaan-3' },
  { year: 2024, name: 'Hottest year on record', type: 'climate', lat: 21.42, lng: 39.83, summary: '2024 is the first calendar year about 1.55 °C above pre-industrial; extreme heat during the Hajj in Mecca kills over 1,300 pilgrims.', wiki: '2024_in_climate_change' },
  { year: 2025, name: 'Los Angeles wildfires', type: 'environment', lat: 34.07, lng: -118.54, summary: 'Wind-driven fires in January 2025 destroy more than 16,000 structures in Pacific Palisades and Altadena.', wiki: 'January_2025_Southern_California_wildfires' },
  { year: 2025, name: 'Myanmar earthquake', type: 'earthquake', lat: 22.0, lng: 95.92, summary: 'A magnitude 7.7 quake on the Sagaing Fault near Mandalay kills thousands and is felt as far as Bangkok.', wiki: '2025_Myanmar_earthquake' },
  { year: 2025, name: 'Kamchatka earthquake', type: 'earthquake', lat: 52.5, lng: 160.3, summary: 'A magnitude 8.8 quake off Kamchatka — among the strongest ever recorded — triggers Pacific-wide tsunami warnings.', wiki: '2025_Kamchatka_earthquake' },
  { year: 2025, name: 'Hurricane Melissa', type: 'disaster', lat: 18.1, lng: -78.0, summary: 'One of the strongest Atlantic hurricanes at landfall strikes Jamaica as a Category 5 storm.', wiki: 'Hurricane_Melissa' }
];

// Landscape features whose size changes through time.
// zones: [lat, lng, rLat, rLng] ellipses (degrees) at the feature's full size; keys: [year, factor] (0 = gone, 1 = full size).
export const CHANGES = [
  // ---- Ice sheets ----
  { id: 'laurentide', name: 'Laurentide Ice Sheet', kind: 'ice', color: '#e8f4ff',
    zones: [[60, -92, 16, 30], [50, -75, 8, 15], [71, -85, 6, 22]],
    keys: [[-20000, 1], [-16000, 0.95], [-14000, 0.85], [-12000, 0.65], [-10000, 0.5], [-9000, 0.32], [-8000, 0.15], [-7000, 0.04], [-6500, 0]],
    summary: 'At its peak this ice sheet buried most of Canada and the northern USA under up to 3–4 km of ice, reaching south past today\'s Chicago and New York. It collapsed through the deglaciation, its last remnants vanishing around 6,500 BC.' },
  { id: 'cordilleran', name: 'Cordilleran Ice Sheet', kind: 'ice', color: '#e8f4ff',
    zones: [[54, -124, 8, 8], [61, -140, 4, 11]],
    keys: [[-20000, 1], [-17000, 0.95], [-14000, 0.6], [-13000, 0.3], [-12000, 0.1], [-11000, 0.03], [-10000, 0]],
    summary: 'This ice sheet covered the mountains of British Columbia, southern Alaska and Washington State. Its retreat may have opened a coastal route for the first people entering the Americas.' },
  { id: 'fennoscandian', name: 'Fennoscandian (Scandinavian) Ice Sheet', kind: 'ice', color: '#e8f4ff',
    zones: [[63, 18, 9, 16], [72, 45, 5, 18]],
    keys: [[-20000, 1], [-17000, 0.9], [-15000, 0.7], [-13000, 0.5], [-11700, 0.35], [-10500, 0.15], [-9000, 0.03], [-8500, 0]],
    summary: 'Centred on the Gulf of Bothnia and joined to the Barents–Kara ice, it covered Scandinavia, Finland and the Baltic and reached northern Germany and Poland. The land beneath is still rebounding by up to ~1 cm a year.' },
  { id: 'british-irish', name: 'British-Irish Ice Sheet', kind: 'ice', color: '#e8f4ff',
    zones: [[55.5, -4.5, 4, 6.5]],
    keys: [[-20000, 1], [-18000, 0.8], [-16000, 0.45], [-14500, 0.1], [-13000, 0], [-10800, 0.04], [-9700, 0]],
    summary: 'Ice covered most of Britain and Ireland north of a line from the Severn to the Thames. A brief readvance during the Younger Dryas (Loch Lomond Stadial) rebuilt glaciers in the Scottish Highlands.' },
  { id: 'patagonian', name: 'Patagonian Ice Sheet', kind: 'ice', color: '#e8f4ff',
    zones: [[-46, -72.5, 9, 2.5]],
    keys: [[-20000, 1], [-17000, 0.8], [-15000, 0.4], [-12000, 0.2], [-10000, 0.08], [-8000, 0.05], [1850, 0.05], [2026, 0.035]],
    summary: 'At the LGM a continuous ice cap ran ~1,800 km along the southern Andes. Its remnants — the Northern and Southern Patagonian Icefields — are now among the fastest-shrinking ice masses on Earth.' },
  // ---- Land bridges ----
  { id: 'beringia', name: 'Beringia land bridge', kind: 'land', color: '#a1887f',
    zones: [[65.5, -170, 5, 18]],
    keys: [[-20000, 1], [-14000, 0.9], [-12000, 0.6], [-10500, 0.35], [-9500, 0.1], [-9000, 0]],
    summary: 'With sea level ~120 m lower, a plain up to 1,000 km wide joined Siberia and Alaska. People and animals crossed it into the Americas before it flooded around 11,000 years ago.' },
  { id: 'doggerland', name: 'Doggerland', kind: 'land', color: '#a1887f',
    zones: [[54.5, 3, 3, 6]],
    keys: [[-20000, 1], [-12000, 1], [-10000, 0.8], [-8000, 0.5], [-6800, 0.2], [-6200, 0.05], [-6000, 0]],
    summary: 'A rich lowland of rivers, marshes and hunting grounds linked Britain to continental Europe. Rising seas and the Storegga tsunami finished it off around 6200 BC; trawlers still dredge up its tools and mammoth bones.' },
  { id: 'sundaland', name: 'Sundaland', kind: 'land', color: '#a1887f',
    zones: [[3, 108, 8, 12]],
    keys: [[-20000, 1], [-14000, 0.9], [-12500, 0.6], [-10000, 0.35], [-8000, 0.15], [-6000, 0]],
    summary: 'The exposed Sunda Shelf joined Sumatra, Java, Borneo and the Malay Peninsula into a subcontinent of rainforest and savanna. Its flooding created the shallow Java Sea and South China Sea shelf.' },
  { id: 'sahul', name: 'Sahul shelf (Australia–New Guinea)', kind: 'land', color: '#a1887f',
    zones: [[-10, 137, 5, 8]],
    keys: [[-20000, 1], [-12000, 0.75], [-10000, 0.4], [-8000, 0.12], [-7000, 0.03], [-6000, 0]],
    summary: 'Australia and New Guinea formed one continent, Sahul, joined across today\'s Arafura Sea and Gulf of Carpentaria. The Torres Strait flooded roughly 8,000 years ago.' },
  { id: 'bass-strait', name: 'Bassian land bridge (Australia–Tasmania)', kind: 'land', color: '#a1887f',
    zones: [[-39.8, 146, 1.5, 3]],
    keys: [[-20000, 1], [-14000, 0.8], [-12000, 0.5], [-10500, 0.2], [-10000, 0.05], [-9500, 0]],
    summary: 'Tasmania was joined to mainland Australia by a low plain. Rising seas cut it off between about 14,000 and 11,500 years ago, isolating Aboriginal Tasmanians for millennia.' },
  // ---- Vegetation & lakes ----
  { id: 'green-sahara', name: 'Green Sahara', kind: 'vegetation', color: '#7cb342',
    zones: [[19, 10, 7, 24]],
    keys: [[-20000, 0], [-10000, 0], [-9000, 0.6], [-8000, 1], [-5000, 1], [-4000, 0.6], [-3500, 0.2], [-3000, 0]],
    summary: 'During the African Humid Period stronger monsoons turned the Sahara into grassland and savanna with lakes, hippos and crocodiles, recorded in rock art. It dried into desert between about 3500 and 2500 BC.' },
  { id: 'mega-chad', name: 'Mega-Lake Chad', kind: 'water', color: '#4fc3f7',
    zones: [[16, 17.5, 3.5, 5]],
    keys: [[-20000, 0], [-12000, 0], [-9000, 0.6], [-7000, 1], [-5000, 0.9], [-4000, 0.4], [-3000, 0]],
    summary: 'At its height some 7,000 years ago Lake Chad covered around 350,000 km², comparable to today\'s Caspian Sea. It shrank as the African Humid Period ended.' },
  { id: 'lake-agassiz', name: 'Lake Agassiz', kind: 'water', color: '#4fc3f7',
    zones: [[52, -97, 6, 10]],
    keys: [[-20000, 0], [-11500, 0], [-11000, 0.3], [-10000, 0.6], [-8000, 0.8], [-6600, 1], [-6300, 0.6], [-6200, 0]],
    summary: 'This vast proglacial lake, at times larger than all of today\'s Great Lakes combined, was dammed by the retreating Laurentide ice. Its final drainage into Hudson Bay is linked to the 8.2-kiloyear cold event.' },
  { id: 'aral-sea', name: 'Aral Sea', kind: 'water', color: '#4fc3f7',
    zones: [[45.0, 59.8, 1.6, 1.8]],
    keys: [[-20000, 1], [1960, 1], [1970, 0.88], [1980, 0.75], [1987, 0.6], [1990, 0.54], [2000, 0.36], [2007, 0.15], [2010, 0.14], [2014, 0.1], [2026, 0.1]],
    summary: 'Once the world\'s fourth-largest lake (~68,000 km²), it shrank by about 90% after Soviet irrigation diverted its rivers from the 1960s. The Kok-Aral dam (2005) has partly restored the small northern Aral.' },
  { id: 'lake-chad', name: 'Lake Chad', kind: 'water', color: '#4fc3f7',
    zones: [[13.2, 14.2, 1.0, 1.2]],
    keys: [[-20000, 0.2], [-12000, 0.6], [-3000, 1], [1963, 1], [1973, 0.6], [1985, 0.08], [2000, 0.1], [2026, 0.1]],
    summary: 'Lake Chad covered about 25,000 km² in 1963 but shrank by roughly 90% amid Sahel droughts and irrigation. Its decline affects millions who depend on it for water, fish and farming.' },
  // ---- Deforestation ----
  { id: 'amazon-deforestation', name: 'Amazon arc of deforestation', kind: 'loss', color: '#e65100',
    zones: [[-10.5, -62.5, 2.5, 2.5], [-11.5, -55, 3, 4], [-5, -50, 3, 3]],
    keys: [[-20000, 0], [1970, 0], [1978, 0.18], [1988, 0.45], [1995, 0.6], [2000, 0.7], [2005, 0.84], [2010, 0.89], [2015, 0.92], [2020, 0.96], [2025, 1]],
    summary: 'Roads, cattle ranching and soy have cleared roughly a fifth of Brazil\'s Amazon rainforest since 1970, concentrated in Rondônia, Mato Grosso and Pará. Clearing peaked at about 29,000 km² in 1995 and 27,800 km² in 2004.' },
  { id: 'borneo-deforestation', name: 'Borneo deforestation', kind: 'loss', color: '#e65100',
    zones: [[1, 113, 3, 4], [-1.5, 111.5, 1.5, 3]],
    keys: [[-20000, 0], [1973, 0], [1985, 0.2], [1990, 0.3], [2000, 0.55], [2010, 0.8], [2016, 0.95], [2025, 1]],
    summary: 'Logging and oil-palm and pulpwood plantations removed about a third of Borneo\'s old-growth forest after 1973. The loss threatens orangutans and released huge amounts of carbon from drained peatlands.' },
  // ---- Glaciers ----
  { id: 'alaska-glaciers', name: 'Alaska & Columbia Icefield glaciers', kind: 'ice', color: '#e8f4ff',
    zones: [[61, -146, 1.5, 5], [52.2, -117.3, 0.4, 0.6]],
    keys: [[-20000, 1], [-10000, 0.8], [-6000, 0.6], [1000, 0.75], [1850, 1], [1900, 0.95], [1950, 0.85], [1980, 0.78], [2000, 0.7], [2020, 0.6], [2026, 0.57]],
    summary: 'Coastal Alaskan icefields and the Columbia Icefield in the Canadian Rockies reached their recent peak during the Little Ice Age. The Athabasca Glacier has retreated more than 1.5 km since 1844, and Alaska\'s glaciers are among the largest contributors to sea-level rise.' },
  { id: 'alps-glaciers', name: 'Alpine glaciers', kind: 'ice', color: '#e8f4ff',
    zones: [[46.3, 8.5, 0.6, 2.5], [46.5, 10.8, 0.3, 1.5]],
    keys: [[-20000, 1], [-10000, 0.5], [-6000, 0.3], [1000, 0.6], [1850, 1], [1900, 0.9], [1950, 0.75], [1975, 0.7], [2000, 0.55], [2015, 0.45], [2022, 0.4], [2026, 0.37]],
    summary: 'At the LGM the Alps were buried under an ice cap; glaciers last peaked around 1850 at the end of the Little Ice Age. They have since lost more than half of their volume, with record losses in 2022.' },
  { id: 'kilimanjaro', name: 'Kilimanjaro ice cap', kind: 'ice', color: '#e8f4ff',
    zones: [[-3.07, 37.35, 0.1, 0.1]],
    keys: [[-20000, 1], [1880, 1], [1912, 0.6], [1953, 0.33], [1976, 0.21], [1989, 0.165], [2000, 0.13], [2011, 0.09], [2026, 0.05]],
    summary: 'Africa\'s highest summit lost about 85% of its ice between 1912 and 2011 (from ~12 km² to ~1.8 km²). Scientists expect the remaining ice fields to disappear within decades.' },
  // ---- Sea ice ----
  { id: 'arctic-sea-ice', name: 'Arctic summer sea ice', kind: 'ice', color: '#e8f4ff',
    zones: [[90, 0, 20, 180]],
    keys: [
      [-20000, 1.1], [-8000, 1.0], [1900, 1.05], [1950, 1.03],
      [1979, 1.000], [1980, 1.088], [1981, 1.013], [1982, 1.035], [1983, 1.048], [1984, 0.966], [1985, 0.950],
      [1986, 1.051], [1987, 1.033], [1988, 1.045], [1989, 0.994], [1990, 0.871], [1991, 0.918], [1992, 1.060],
      [1993, 0.908], [1994, 1.013], [1995, 0.862], [1996, 1.075], [1997, 0.949], [1998, 0.928], [1999, 0.868],
      [2000, 0.887], [2001, 0.955], [2002, 0.827], [2003, 0.868], [2004, 0.848], [2005, 0.780], [2006, 0.831],
      [2007, 0.606], [2008, 0.665], [2009, 0.746], [2010, 0.691], [2011, 0.647], [2012, 0.506], [2013, 0.739],
      [2014, 0.740], [2015, 0.655], [2016, 0.643], [2017, 0.684], [2018, 0.679], [2019, 0.618], [2020, 0.567],
      [2021, 0.702], [2022, 0.695], [2023, 0.621], [2024, 0.617], [2025, 0.674], [2026, 0.682]
    ],
    summary: 'September (minimum) Arctic sea-ice extent relative to 1979, from NSIDC satellite records. The summer ice cover has shrunk by roughly 40% since 1979, with a record low of 3.57 million km² in 2012.' }
];

export const HIST_MAPS = [-123000,-10000,-8000,-5000,-4000,-3000,-2000,-1500,-1000,-700,-500,-400,-323,-300,-200,-100,-1,100,200,300,400,500,600,700,800,900,1000,1100,1200,1279,1300,1400,1492,1500,1530,1600,1650,1700,1715,1783,1800,1815,1880,1900,1914,1920,1930,1938,1945,1960,1994,2000,2010];

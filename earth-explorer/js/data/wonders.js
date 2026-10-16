// Curated natural wonders of the world.
// formed: approximate year the feature formed (negative = years BC / years ago), or null.
// unesco: year inscribed as (part of) a UNESCO World Heritage Site, or null.
// wiki: exact English Wikipedia article title (used to fetch a photo).

export const WONDERS = [
  // ───────────── Classic Seven Natural Wonders ─────────────
  {
    id: "grand-canyon",
    name: "Grand Canyon",
    type: "Canyon",
    emoji: "🏜️",
    country: "United States",
    lat: 36.10, lng: -112.11,
    summary: "A mile-deep gash carved by the Colorado River, exposing nearly two billion years of Earth's history in bands of red, orange and cream rock.",
    facts: [
      "The oldest rocks at the canyon floor, the Vishnu Schist, are around 1.7 billion years old.",
      "The canyon is up to 29 km wide from rim to rim.",
      "John Wesley Powell led the first known expedition through the canyon by boat in 1869.",
      "It became a U.S. National Park in 1919.",
      "The North Rim is roughly 300 m higher than the South Rim and much cooler."
    ],
    stats: { "Length": "446 km", "Max depth": "1,857 m", "Age of carving": "5–6 million years" },
    formed: -5500000,
    unesco: 1979,
    wiki: "Grand_Canyon"
  },
  {
    id: "great-barrier-reef",
    name: "Great Barrier Reef",
    type: "Reef",
    emoji: "🐠",
    country: "Australia",
    lat: -18.29, lng: 147.70,
    summary: "The largest coral reef system on Earth, a living structure built by billions of tiny polyps and visible from space.",
    facts: [
      "It is made up of about 2,900 individual reefs and 900 islands.",
      "It covers roughly 344,400 km², an area larger than Italy.",
      "It is home to more than 1,500 species of fish and around 400 types of hard coral.",
      "Mass coral bleaching events, driven by marine heatwaves, struck the reef in 2016, 2017, 2020, 2022 and 2024.",
      "The present living reef began growing on older foundations around 20,000 years ago."
    ],
    stats: { "Length": "~2,300 km", "Area": "344,400 km²", "Reefs": "~2,900" },
    formed: -20000,
    unesco: 1981,
    wiki: "Great_Barrier_Reef"
  },
  {
    id: "harbour-of-rio",
    name: "Harbour of Rio de Janeiro (Guanabara Bay)",
    type: "Coast & Fjord",
    emoji: "⛵",
    country: "Brazil",
    lat: -22.80, lng: -43.15,
    summary: "A vast natural bay ringed by granite peaks like Sugarloaf Mountain and Corcovado, framing one of the world's most dramatic city settings.",
    facts: [
      "Portuguese explorers arrived in January 1502 and mistook the bay for a river mouth, hence the name Rio de Janeiro, 'River of January'.",
      "Sugarloaf Mountain rises 396 m straight out of the water at the bay's entrance.",
      "The Rio–Niterói Bridge, opened in 1974, crosses the bay for about 13 km.",
      "It is one of the classic Seven Natural Wonders of the World.",
      "Rio's 'Carioca Landscapes between the Mountain and the Sea' were inscribed by UNESCO in 2012."
    ],
    stats: { "Area": "~412 km²", "Sugarloaf": "396 m" },
    formed: null,
    unesco: 2012,
    wiki: "Guanabara_Bay"
  },
  {
    id: "mount-everest",
    name: "Mount Everest",
    type: "Mountain",
    emoji: "🏔️",
    country: "Nepal / China",
    lat: 27.9881, lng: 86.9250,
    summary: "Earth's highest mountain above sea level, straddling the border of Nepal and Tibet in the Himalaya.",
    facts: [
      "Edmund Hillary and Tenzing Norgay made the first confirmed ascent on 29 May 1953.",
      "China and Nepal jointly announced its height as 8,848.86 m in December 2020.",
      "The summit rock is marine limestone containing fossils of sea creatures from an ancient ocean floor.",
      "It is known as Sagarmatha in Nepali and Chomolungma in Tibetan.",
      "The British named it in 1865 after Sir George Everest, former Surveyor General of India."
    ],
    stats: { "Height": "8,848.86 m", "First ascent": "1953", "Range": "Himalaya" },
    formed: -50000000,
    unesco: 1979,
    wiki: "Mount_Everest"
  },
  {
    id: "aurora-borealis-tromso",
    name: "Aurora Borealis (Tromsø)",
    type: "Natural Phenomenon",
    emoji: "🌌",
    country: "Norway",
    lat: 69.6492, lng: 18.9553,
    summary: "Curtains of green and violet light ripple across Arctic skies as charged particles from the Sun collide with Earth's upper atmosphere.",
    facts: [
      "Auroras typically glow 100–300 km above the ground.",
      "The common green colour comes from oxygen atoms emitting light at 557.7 nm; red auroras come from oxygen at higher altitudes.",
      "Tromsø lies beneath the auroral oval, making it one of the best places on Earth to see the lights.",
      "Galileo coined the name in 1619, after Aurora, the Roman goddess of dawn.",
      "During the polar night, roughly late November to mid-January, the Sun does not rise above Tromsø's horizon."
    ],
    stats: { "Altitude": "100–300 km", "Season": "Sep–Mar" },
    formed: null,
    unesco: null,
    wiki: "Aurora"
  },
  {
    id: "paricutin",
    name: "Parícutin",
    type: "Volcano",
    emoji: "🌋",
    country: "Mexico",
    lat: 19.4931, lng: -102.2511,
    summary: "The volcano that was born in a farmer's cornfield in 1943, letting scientists witness a volcano's entire life cycle for the first time.",
    facts: [
      "It burst from Dionisio Pulido's cornfield on 20 February 1943.",
      "It erupted for nine years, until 1952.",
      "Its lava buried the villages of Parícutin and San Juan Parangaricutiro; the old church tower still pokes out of the lava field.",
      "Its cone rose 424 m above the original field.",
      "It is one of the classic Seven Natural Wonders of the World."
    ],
    stats: { "Height above field": "424 m", "Eruption": "1943–1952" },
    formed: 1943,
    unesco: null,
    wiki: "Parícutin"
  },
  {
    id: "victoria-falls",
    name: "Victoria Falls",
    type: "Waterfall",
    emoji: "💦",
    country: "Zambia / Zimbabwe",
    lat: -17.9243, lng: 25.8572,
    summary: "The Zambezi River plunges into a narrow chasm, creating the largest single sheet of falling water on Earth and a spray seen from kilometres away.",
    facts: [
      "Its local name, Mosi-oa-Tunya, means 'the smoke that thunders'.",
      "David Livingstone was the first European to see the falls, in November 1855, naming them after Queen Victoria.",
      "The falls are about 1,708 m wide and 108 m tall.",
      "Devil's Pool, at the very lip of the falls, can be swum in during the dry season.",
      "The Victoria Falls Bridge across the gorge opened in 1905."
    ],
    stats: { "Width": "1,708 m", "Height": "108 m" },
    formed: null,
    unesco: 1989,
    wiki: "Victoria_Falls"
  },

  // ───────────── New 7 Wonders of Nature ─────────────
  {
    id: "amazon-rainforest",
    name: "Amazon Rainforest",
    type: "Rainforest",
    emoji: "🌳",
    country: "Brazil / Peru / Colombia + 6 more",
    lat: -3.47, lng: -62.22,
    summary: "The largest tropical rainforest on the planet, a green ocean that holds an astonishing share of Earth's species.",
    facts: [
      "The rainforest covers about 5.5 million km² across nine countries.",
      "Around 60% of it lies in Brazil.",
      "A 2013 study estimated it holds about 390 billion trees from some 16,000 species.",
      "It is thought to have existed for at least 55 million years.",
      "It was named one of the New 7 Wonders of Nature in 2011."
    ],
    stats: { "Area": "~5.5 million km²", "Countries": "9", "Tree species": "~16,000" },
    formed: -55000000,
    unesco: null,
    wiki: "Amazon_rainforest"
  },
  {
    id: "ha-long-bay",
    name: "Ha Long Bay",
    type: "Coast & Fjord",
    emoji: "🛶",
    country: "Vietnam",
    lat: 20.91, lng: 107.18,
    summary: "Nearly two thousand limestone islets and pillars rise from emerald water in the Gulf of Tonkin.",
    facts: [
      "Its name means 'descending dragon' in Vietnamese, after a legend of dragons forming the islands.",
      "The bay contains about 1,600 limestone islands and islets, many with caves and grottoes.",
      "It was inscribed by UNESCO in 1994; in 2023 the site was extended to include the Cat Ba Archipelago.",
      "Many islands are hollow, containing huge caves like Sung Sot (Surprise) Cave.",
      "It was named one of the New 7 Wonders of Nature in 2011."
    ],
    stats: { "Area": "~1,553 km²", "Islands": "~1,600" },
    formed: null,
    unesco: 1994,
    wiki: "Hạ_Long_Bay"
  },
  {
    id: "iguazu-falls",
    name: "Iguazu Falls",
    type: "Waterfall",
    emoji: "🌊",
    country: "Argentina / Brazil",
    lat: -25.6953, lng: -54.4367,
    summary: "A horseshoe of about 275 cascades thundering through subtropical rainforest on the Argentina–Brazil border.",
    facts: [
      "The falls stretch for about 2.7 km along the Iguazu River.",
      "The largest cascade, the Devil's Throat, is about 82 m high.",
      "On seeing them, Eleanor Roosevelt reportedly exclaimed, 'Poor Niagara!'",
      "Argentina's national park was inscribed by UNESCO in 1984 and Brazil's in 1986.",
      "It was named one of the New 7 Wonders of Nature in 2011."
    ],
    stats: { "Width": "2.7 km", "Cascades": "~275", "Max drop": "82 m" },
    formed: null,
    unesco: 1984,
    wiki: "Iguazu_Falls"
  },
  {
    id: "jeju-island",
    name: "Jeju Island",
    type: "Island",
    emoji: "🏝️",
    country: "South Korea",
    lat: 33.36, lng: 126.53,
    summary: "A volcanic island crowned by Hallasan, South Korea's highest peak, and riddled with lava tubes and sea cliffs.",
    facts: [
      "Hallasan, a shield volcano at the island's centre, is 1,947 m high.",
      "The Manjanggul lava tube is one of the finest lava caves in the world.",
      "Seongsan Ilchulbong ('Sunrise Peak') is a tuff cone that rose from the sea in an eruption.",
      "Jeju's haenyeo, women free-divers who harvest seafood, are recognised by UNESCO as intangible cultural heritage.",
      "It was named one of the New 7 Wonders of Nature in 2011."
    ],
    stats: { "Highest point": "1,947 m", "Area": "~1,850 km²" },
    formed: null,
    unesco: 2007,
    wiki: "Jeju_Island"
  },
  {
    id: "komodo-island",
    name: "Komodo Island",
    type: "Island",
    emoji: "🦎",
    country: "Indonesia",
    lat: -8.55, lng: 119.45,
    summary: "A rugged volcanic island that is home to the Komodo dragon, the largest living lizard on Earth.",
    facts: [
      "Komodo dragons can grow to about 3 m long and weigh around 70 kg.",
      "Komodo National Park was established in 1980 to protect the dragons.",
      "The park's Pink Beach gets its colour from fragments of red coral.",
      "The surrounding waters are among the richest marine environments in the world.",
      "It was named one of the New 7 Wonders of Nature in 2011."
    ],
    stats: { "Area": "~390 km²", "Dragon length": "up to 3 m" },
    formed: null,
    unesco: 1991,
    wiki: "Komodo_(island)"
  },
  {
    id: "puerto-princesa-underground-river",
    name: "Puerto Princesa Underground River",
    type: "River",
    emoji: "🦇",
    country: "Philippines",
    lat: 10.19, lng: 118.93,
    summary: "A river that winds through a vast limestone cave before flowing directly into the sea, navigable by boat for kilometres underground.",
    facts: [
      "About 8.2 km of the river runs underground through the cave.",
      "The lower part of the river is tidal, because it empties straight into the South China Sea.",
      "The cave contains huge chambers decorated with stalactites and home to bats and swiftlets.",
      "It was inscribed by UNESCO in 1999.",
      "It was named one of the New 7 Wonders of Nature in 2011."
    ],
    stats: { "Underground length": "8.2 km", "Province": "Palawan" },
    formed: null,
    unesco: 1999,
    wiki: "Puerto_Princesa_Subterranean_River_National_Park"
  },
  {
    id: "table-mountain",
    name: "Table Mountain",
    type: "Mountain",
    emoji: "⛰️",
    country: "South Africa",
    lat: -33.9628, lng: 18.4098,
    summary: "A flat-topped sandstone massif looming over Cape Town, often draped in its famous 'tablecloth' of cloud.",
    facts: [
      "Its highest point, Maclear's Beacon, is 1,086 m above sea level.",
      "The level plateau stretches for about 3 km.",
      "The mountain hosts roughly 2,200 plant species, more than the whole of the United Kingdom.",
      "The aerial cableway opened in 1929; its rotating cars were added in 1997.",
      "It was named one of the New 7 Wonders of Nature in 2011."
    ],
    stats: { "Height": "1,086 m", "Plateau": "~3 km" },
    formed: null,
    unesco: 2004,
    wiki: "Table_Mountain"
  },

  // ───────────── Mountains ─────────────
  {
    id: "k2",
    name: "K2",
    type: "Mountain",
    emoji: "🏔️",
    country: "Pakistan / China",
    lat: 35.8825, lng: 76.5133,
    summary: "The world's second-highest mountain, a steep pyramid in the Karakoram nicknamed the 'Savage Mountain'.",
    facts: [
      "Italians Lino Lacedelli and Achille Compagnoni made the first ascent on 31 July 1954.",
      "Its name comes from survey notation: it was peak 2 of the Karakoram ('K2') in an 1856 survey.",
      "A team of ten Nepali climbers made the first winter ascent on 16 January 2021.",
      "It is considered one of the deadliest 8,000-metre peaks to climb."
    ],
    stats: { "Height": "8,611 m", "First ascent": "1954" },
    formed: null,
    unesco: null,
    wiki: "K2"
  },
  {
    id: "kilimanjaro",
    name: "Mount Kilimanjaro",
    type: "Mountain",
    emoji: "🗻",
    country: "Tanzania",
    lat: -3.0758, lng: 37.3533,
    summary: "Africa's highest peak, a snow-capped dormant volcano rising alone above the savanna.",
    facts: [
      "Its summit, Uhuru Peak on the Kibo cone, stands 5,895 m above sea level.",
      "It is the world's highest free-standing mountain, not part of a range.",
      "Kilimanjaro has three volcanic cones: Kibo, Mawenzi and Shira.",
      "Hans Meyer and Ludwig Purtscheller made the first recorded ascent in 1889.",
      "Its glaciers have shrunk by more than 80% since the early 20th century."
    ],
    stats: { "Height": "5,895 m", "First ascent": "1889" },
    formed: -1000000,
    unesco: 1987,
    wiki: "Mount_Kilimanjaro"
  },
  {
    id: "denali",
    name: "Denali",
    type: "Mountain",
    emoji: "🏔️",
    country: "United States",
    lat: 63.0692, lng: -151.0070,
    summary: "North America's highest mountain, rising about 5,500 m from its base in the Alaska Range.",
    facts: [
      "Its summit is 6,190 m above sea level.",
      "Its base-to-peak rise is one of the largest of any mountain on land.",
      "The name Denali comes from the Koyukon Athabaskan word for 'the high one'.",
      "Walter Harper, an Alaska Native, was the first person to reach the summit, on 7 June 1913.",
      "Its official U.S. federal name has switched between Mount McKinley and Denali (2015, and back to McKinley in 2025); Alaska still uses Denali."
    ],
    stats: { "Height": "6,190 m", "First ascent": "1913" },
    formed: null,
    unesco: null,
    wiki: "Denali"
  },
  {
    id: "aconcagua",
    name: "Aconcagua",
    type: "Mountain",
    emoji: "🏔️",
    country: "Argentina",
    lat: -32.6532, lng: -70.0109,
    summary: "The highest mountain outside Asia, towering over the Andes near the Chilean border.",
    facts: [
      "At 6,961 m it is the highest peak in both the Western and Southern Hemispheres.",
      "Swiss guide Matthias Zurbriggen made the first recorded ascent in January 1897.",
      "An Inca child mummy was discovered on its slopes in 1985, evidence of ancient high-altitude rituals.",
      "Its normal route is non-technical, but altitude and fierce winds make it dangerous."
    ],
    stats: { "Height": "6,961 m", "First ascent": "1897" },
    formed: null,
    unesco: null,
    wiki: "Aconcagua"
  },
  {
    id: "mont-blanc",
    name: "Mont Blanc",
    type: "Mountain",
    emoji: "🏔️",
    country: "France / Italy",
    lat: 45.8326, lng: 6.8652,
    summary: "The 'White Mountain', the highest summit of the Alps, whose first ascent in 1786 is often seen as the birth of modern mountaineering.",
    facts: [
      "Jacques Balmat and Michel-Gabriel Paccard made the first ascent on 8 August 1786.",
      "Its height varies with the snow cap; a 2021 survey measured 4,805.59 m.",
      "The 11.6 km Mont Blanc Tunnel beneath it opened in 1965.",
      "Marie Paradis became the first woman to reach the summit in 1808."
    ],
    stats: { "Height": "~4,806 m", "First ascent": "1786" },
    formed: null,
    unesco: null,
    wiki: "Mont_Blanc"
  },
  {
    id: "matterhorn",
    name: "Matterhorn",
    type: "Mountain",
    emoji: "🏔️",
    country: "Switzerland / Italy",
    lat: 45.9763, lng: 7.6586,
    summary: "A near-perfect pyramid of rock above Zermatt, one of the most recognisable mountains on Earth.",
    facts: [
      "Edward Whymper's party made the first ascent on 14 July 1865; four of the seven climbers died on the descent when a rope broke.",
      "Jean-Antoine Carrel reached the summit from the Italian side just three days later.",
      "Its four faces point roughly toward the four compass directions.",
      "In Italian it is called Monte Cervino."
    ],
    stats: { "Height": "4,478 m", "First ascent": "1865" },
    formed: null,
    unesco: null,
    wiki: "Matterhorn"
  },
  {
    id: "mount-kailash",
    name: "Mount Kailash",
    type: "Mountain",
    emoji: "🕉️",
    country: "China (Tibet)",
    lat: 31.0675, lng: 81.3119,
    summary: "A sacred, black-rock peak in western Tibet revered by four religions and never climbed.",
    facts: [
      "It is sacred in Hinduism (the abode of Shiva), Buddhism, Jainism and the Bon religion.",
      "Pilgrims walk a roughly 52 km circuit (kora) around its base instead of climbing it.",
      "No recorded ascent has ever been made; climbing is not permitted.",
      "Sources of the Indus, Sutlej, Brahmaputra and Karnali rivers lie nearby."
    ],
    stats: { "Height": "6,638 m", "Kora": "~52 km" },
    formed: null,
    unesco: null,
    wiki: "Mount_Kailash"
  },
  {
    id: "mount-roraima",
    name: "Mount Roraima",
    type: "Mountain",
    emoji: "🪨",
    country: "Venezuela / Brazil / Guyana",
    lat: 5.1433, lng: -60.7625,
    summary: "A colossal flat-topped tepui with sheer 400 m cliffs, whose isolated summit inspired 'The Lost World'.",
    facts: [
      "Its summit plateau marks the tripoint of Venezuela, Brazil and Guyana.",
      "Its sandstone is among the oldest exposed rock formations on Earth, around two billion years old.",
      "Arthur Conan Doyle's 1912 novel 'The Lost World' was inspired by accounts of tepuis like Roraima.",
      "Everard im Thurn and Harry Perkins made the first recorded ascent in 1884."
    ],
    stats: { "Height": "2,810 m", "Plateau area": "~31 km²" },
    formed: null,
    unesco: 1994,
    wiki: "Mount_Roraima"
  },
  {
    id: "vinson-massif",
    name: "Vinson Massif",
    type: "Mountain",
    emoji: "🧊",
    country: "Antarctica",
    lat: -78.5254, lng: -85.6171,
    summary: "The roof of Antarctica, a remote ice-clad massif in the Ellsworth Mountains just 1,200 km from the South Pole.",
    facts: [
      "Mount Vinson, the massif's summit, is 4,892 m high.",
      "It was first spotted from a U.S. Navy aircraft in 1958.",
      "An American expedition led by Nicholas Clinch made the first ascent on 18 December 1966.",
      "It is named after U.S. Congressman Carl Vinson, a supporter of Antarctic exploration."
    ],
    stats: { "Height": "4,892 m", "First ascent": "1966" },
    formed: null,
    unesco: null,
    wiki: "Vinson_Massif"
  },
  {
    id: "dolomites",
    name: "Dolomites",
    type: "Mountain",
    emoji: "⛰️",
    country: "Italy",
    lat: 46.41, lng: 11.84,
    summary: "Pale, jagged peaks that glow pink at sunset, built from fossilised coral reefs of a vanished tropical sea.",
    facts: [
      "The range is named after French geologist Déodat de Dolomieu, who described the mineral dolomite.",
      "Its highest peak, Marmolada, reaches 3,343 m.",
      "The rocks formed as reefs and lagoons in the Triassic period, more than 200 million years ago.",
      "During World War I, Italian and Austro-Hungarian troops fought here and dug tunnels into the peaks."
    ],
    stats: { "Highest peak": "3,343 m", "Peaks over 3,000 m": "18" },
    formed: null,
    unesco: 2009,
    wiki: "Dolomites"
  },

  // ───────────── Volcanoes ─────────────
  {
    id: "mount-fuji",
    name: "Mount Fuji",
    type: "Volcano",
    emoji: "🗻",
    country: "Japan",
    lat: 35.3606, lng: 138.7274,
    summary: "Japan's highest and most iconic mountain, a near-symmetrical snow-capped stratovolcano.",
    facts: [
      "Fuji stands 3,776 m high.",
      "It last erupted in 1707, the Hōei eruption, which dusted Edo (Tokyo) in ash.",
      "Hokusai's 'Thirty-six Views of Mount Fuji' made it a global symbol of Japan.",
      "UNESCO inscribed it as a cultural site in 2013, as a sacred place and source of artistic inspiration.",
      "The official climbing season runs from July to early September."
    ],
    stats: { "Height": "3,776 m", "Last eruption": "1707" },
    formed: -100000,
    unesco: 2013,
    wiki: "Mount_Fuji"
  },
  {
    id: "mount-etna",
    name: "Mount Etna",
    type: "Volcano",
    emoji: "🌋",
    country: "Italy",
    lat: 37.751, lng: 14.9934,
    summary: "Europe's largest and most active volcano, erupting frequently above the Sicilian city of Catania.",
    facts: [
      "Its height changes with eruptions, hovering around 3,300–3,400 m.",
      "It covers about 1,190 km² of Sicily.",
      "The 1669 eruption sent lava flows to the walls of Catania.",
      "Volcanic activity at Etna began about 500,000 years ago.",
      "It was inscribed as a UNESCO World Heritage Site in 2013."
    ],
    stats: { "Height": "~3,350 m", "Area": "1,190 km²" },
    formed: -500000,
    unesco: 2013,
    wiki: "Mount_Etna"
  },
  {
    id: "mount-vesuvius",
    name: "Mount Vesuvius",
    type: "Volcano",
    emoji: "🌋",
    country: "Italy",
    lat: 40.8213, lng: 14.4261,
    summary: "The volcano that buried Pompeii and Herculaneum in AD 79, still brooding over the Bay of Naples.",
    facts: [
      "The AD 79 eruption was described in letters by Pliny the Younger, whose uncle Pliny the Elder died during it.",
      "Its last eruption was in March 1944, during World War II, destroying dozens of Allied aircraft.",
      "It is the only volcano on mainland Europe to have erupted in the last hundred years.",
      "Around 600,000 people live in the official 'red zone' around it."
    ],
    stats: { "Height": "1,281 m", "Last eruption": "1944" },
    formed: -25000,
    unesco: null,
    wiki: "Mount_Vesuvius"
  },
  {
    id: "kilauea",
    name: "Kīlauea",
    type: "Volcano",
    emoji: "🌋",
    country: "United States (Hawaiʻi)",
    lat: 19.421, lng: -155.287,
    summary: "One of the most active volcanoes on Earth, whose glowing lava has been reshaping Hawaiʻi's Big Island for decades.",
    facts: [
      "In Hawaiian tradition, Halemaʻumaʻu crater is the home of Pele, goddess of volcanoes.",
      "The Puʻu ʻŌʻō eruption ran almost continuously from 1983 to 2018.",
      "The 2018 lower Puna eruption destroyed around 700 homes.",
      "Kīlauea is roughly 210,000–280,000 years old and emerged above sea level about 100,000 years ago.",
      "Hawaiʻi Volcanoes National Park was inscribed by UNESCO in 1987."
    ],
    stats: { "Height": "1,247 m", "Type": "Shield volcano" },
    formed: -250000,
    unesco: 1987,
    wiki: "Kīlauea"
  },
  {
    id: "mauna-kea",
    name: "Mauna Kea",
    type: "Volcano",
    emoji: "🔭",
    country: "United States (Hawaiʻi)",
    lat: 19.8207, lng: -155.4681,
    summary: "Measured from its base on the ocean floor, this dormant volcano is taller than Mount Everest.",
    facts: [
      "Its summit is 4,207 m above sea level, but about 10,200 m above its base on the seafloor.",
      "Its name means 'White Mountain', for the snow that caps it in winter.",
      "The summit hosts some of the world's major astronomical observatories.",
      "It is sacred in Native Hawaiian tradition.",
      "It last erupted roughly 4,600 years ago."
    ],
    stats: { "Height (sea level)": "4,207 m", "Base to peak": "~10,200 m" },
    formed: -1000000,
    unesco: null,
    wiki: "Mauna_Kea"
  },
  {
    id: "mount-erebus",
    name: "Mount Erebus",
    type: "Volcano",
    emoji: "🌋",
    country: "Antarctica",
    lat: -77.53, lng: 167.15,
    summary: "The southernmost active volcano on Earth, holding a rare persistent lava lake amid Antarctic ice.",
    facts: [
      "It stands 3,794 m high on Ross Island.",
      "James Clark Ross discovered it in 1841 and named it after his ship, HMS Erebus.",
      "A party from Shackleton's Nimrod expedition made the first ascent in 1908.",
      "Its summit crater has contained a convecting lava lake since at least 1972.",
      "In 1979, Air New Zealand Flight 901 crashed into its slopes, killing all 257 people aboard."
    ],
    stats: { "Height": "3,794 m", "First ascent": "1908" },
    formed: null,
    unesco: null,
    wiki: "Mount_Erebus"
  },
  {
    id: "kawah-ijen",
    name: "Kawah Ijen",
    type: "Volcano",
    emoji: "💙",
    country: "Indonesia",
    lat: -8.0583, lng: 114.2417,
    summary: "A volcano famed for eerie electric-blue flames at night and a turquoise crater lake of acid.",
    facts: [
      "The blue flames come from sulfuric gases that ignite as they escape the vents.",
      "Its crater lake, about 1 km wide, is one of the most acidic bodies of water on Earth.",
      "Miners carry heavy baskets of solidified sulfur out of the crater by hand.",
      "It lies in eastern Java, part of the Ijen volcano complex."
    ],
    stats: { "Crater lake width": "~1 km", "Lake pH": "<1" },
    formed: null,
    unesco: null,
    wiki: "Ijen"
  },
  {
    id: "ngorongoro-crater",
    name: "Ngorongoro Crater",
    type: "Volcano",
    emoji: "🦏",
    country: "Tanzania",
    lat: -3.175, lng: 35.5875,
    summary: "The world's largest intact, unflooded volcanic caldera, a natural enclosure teeming with wildlife.",
    facts: [
      "It formed when a large volcano collapsed in on itself roughly 2.5 million years ago.",
      "The crater is about 610 m deep with a floor of around 260 km².",
      "Tens of thousands of large animals, including endangered black rhinos, live inside it.",
      "Maasai pastoralists share the wider conservation area with the wildlife.",
      "The Ngorongoro Conservation Area was inscribed by UNESCO in 1979."
    ],
    stats: { "Depth": "610 m", "Floor area": "~260 km²" },
    formed: -2500000,
    unesco: 1979,
    wiki: "Ngorongoro_Conservation_Area"
  },

  // ───────────── Geysers & hot springs ─────────────
  {
    id: "old-faithful",
    name: "Old Faithful (Yellowstone)",
    type: "Geyser & Hot Spring",
    emoji: "♨️",
    country: "United States",
    lat: 44.4605, lng: -110.8281,
    summary: "Yellowstone's most famous geyser, shooting scalding water into the sky with remarkable regularity.",
    facts: [
      "It erupts roughly every 60 to 110 minutes.",
      "Eruptions reach 32–56 m high and last 1.5 to 5 minutes.",
      "The 1870 Washburn Expedition named it for its predictable eruptions.",
      "Yellowstone, established on 1 March 1872, was the world's first national park.",
      "It sits atop the Yellowstone supervolcano, whose last caldera-forming eruption was about 640,000 years ago."
    ],
    stats: { "Eruption height": "32–56 m", "Interval": "60–110 min" },
    formed: null,
    unesco: 1978,
    wiki: "Old_Faithful"
  },
  {
    id: "grand-prismatic-spring",
    name: "Grand Prismatic Spring",
    type: "Geyser & Hot Spring",
    emoji: "🌈",
    country: "United States",
    lat: 44.5251, lng: -110.8382,
    summary: "The largest hot spring in the United States, ringed with bands of colour like a giant rainbow eye.",
    facts: [
      "It is about 110 m in diameter and 50 m deep.",
      "Its vivid orange, yellow and green rings are made by heat-loving microbial mats.",
      "The deep centre is a sterile blue because it is too hot for most microbes.",
      "It lies in Yellowstone's Midway Geyser Basin."
    ],
    stats: { "Diameter": "~110 m", "Depth": "~50 m" },
    formed: null,
    unesco: 1978,
    wiki: "Grand_Prismatic_Spring"
  },
  {
    id: "valley-of-geysers",
    name: "Valley of Geysers",
    type: "Geyser & Hot Spring",
    emoji: "♨️",
    country: "Russia",
    lat: 54.4306, lng: 160.1397,
    summary: "A remote Kamchatka valley with the second-largest concentration of geysers in the world.",
    facts: [
      "It was discovered in 1941 by geologist Tatyana Ustinova.",
      "Around 90 geysers are packed into a valley about 6 km long.",
      "A huge mudflow in June 2007 buried part of the valley.",
      "It lies within the Kronotsky Nature Reserve and is reachable mainly by helicopter.",
      "It is part of the 'Volcanoes of Kamchatka' UNESCO site, inscribed in 1996."
    ],
    stats: { "Geysers": "~90", "Length": "~6 km" },
    formed: null,
    unesco: 1996,
    wiki: "Valley_of_Geysers"
  },
  {
    id: "pamukkale",
    name: "Pamukkale",
    type: "Geyser & Hot Spring",
    emoji: "🤍",
    country: "Türkiye",
    lat: 37.9214, lng: 29.1206,
    summary: "Snow-white travertine terraces filled with warm, turquoise pools cascade down a hillside.",
    facts: [
      "Its name means 'cotton castle' in Turkish.",
      "The terraces are built from calcium carbonate deposited by thermal spring water.",
      "The ancient Greco-Roman spa city of Hierapolis was built on top of it.",
      "Visitors must walk barefoot on the terraces to protect them.",
      "Hierapolis-Pamukkale was inscribed by UNESCO in 1988."
    ],
    stats: { "Length": "~2,700 m", "Height": "~160 m" },
    formed: null,
    unesco: 1988,
    wiki: "Pamukkale"
  },

  // ───────────── Waterfalls ─────────────
  {
    id: "niagara-falls",
    name: "Niagara Falls",
    type: "Waterfall",
    emoji: "🌊",
    country: "Canada / United States",
    lat: 43.0799, lng: -79.0747,
    summary: "Three thundering waterfalls on the Canada–US border with the highest flow rate of any waterfall in North America.",
    facts: [
      "It is made up of Horseshoe Falls, American Falls and Bridal Veil Falls.",
      "The falls formed about 12,000 years ago as glaciers retreated at the end of the last ice age.",
      "On 24 October 1901, 63-year-old Annie Edson Taylor became the first person to survive going over in a barrel.",
      "Engineers temporarily 'turned off' the American Falls in 1969 to study erosion.",
      "Much of the river's flow is diverted to generate hydroelectric power."
    ],
    stats: { "Horseshoe drop": "~51 m", "Horseshoe width": "~790 m" },
    formed: -10000,
    unesco: null,
    wiki: "Niagara_Falls"
  },
  {
    id: "angel-falls",
    name: "Angel Falls",
    type: "Waterfall",
    emoji: "💧",
    country: "Venezuela",
    lat: 5.9701, lng: -62.5362,
    summary: "The world's tallest uninterrupted waterfall, plunging from the edge of Auyán-tepui in the Venezuelan jungle.",
    facts: [
      "It drops 979 m in total, with an uninterrupted plunge of 807 m.",
      "It is named after American aviator Jimmie Angel, who flew over it in 1933.",
      "Its Pemón name is Kerepakupai Merú, 'waterfall of the deepest place'.",
      "It inspired Paradise Falls in Pixar's film 'Up'.",
      "It lies within Canaima National Park, inscribed by UNESCO in 1994."
    ],
    stats: { "Height": "979 m", "Longest plunge": "807 m" },
    formed: null,
    unesco: 1994,
    wiki: "Angel_Falls"
  },

  // ───────────── Rivers, lakes & wetlands ─────────────
  {
    id: "amazon-river",
    name: "Amazon River",
    type: "River",
    emoji: "🏞️",
    country: "Brazil / Peru / Colombia",
    lat: -3.136, lng: -59.903,
    summary: "The world's largest river by volume, carrying about a fifth of all river water that flows into the oceans.",
    facts: [
      "It discharges an average of about 209,000 m³ of water per second.",
      "No bridge crosses its main stem.",
      "At the 'Meeting of Waters' near Manaus, the dark Rio Negro and sandy Solimões flow side by side for kilometres without mixing.",
      "Spanish explorer Francisco de Orellana made the first European descent of the river in 1542.",
      "Its length of roughly 6,400 km rivals the Nile's."
    ],
    stats: { "Length": "~6,400 km", "Discharge": "~209,000 m³/s" },
    formed: null,
    unesco: null,
    wiki: "Amazon_River"
  },
  {
    id: "nile",
    name: "Nile River",
    type: "River",
    emoji: "🐊",
    country: "Egypt / Sudan + 9 more",
    lat: 15.62, lng: 32.51,
    summary: "The lifeblood of ancient Egypt, traditionally counted as the world's longest river; its two great branches meet at Khartoum.",
    facts: [
      "It is about 6,650 km long and its basin covers 11 countries.",
      "The White Nile and Blue Nile converge at Khartoum, Sudan.",
      "The Blue Nile supplies most of the water and fertile silt during the flood season.",
      "Egypt's Aswan High Dam, completed in 1970, ended the river's annual flooding downstream and created Lake Nasser.",
      "Ancient Egyptian civilisation depended on its yearly flood to grow crops."
    ],
    stats: { "Length": "~6,650 km", "Basin": "~3.4 million km²" },
    formed: null,
    unesco: null,
    wiki: "Nile"
  },
  {
    id: "li-river",
    name: "Li River (Guilin)",
    type: "River",
    emoji: "🎋",
    country: "China",
    lat: 24.92, lng: 110.51,
    summary: "A gentle river gliding between misty karst peaks, the landscape that defined Chinese scroll painting.",
    facts: [
      "The classic 83 km cruise runs from Guilin to Yangshuo.",
      "The scene near Xingping appears on the back of the Chinese 20-yuan banknote.",
      "Fishermen have traditionally used trained cormorants to fish its waters.",
      "Guilin's karst landscape became part of the South China Karst UNESCO site in 2014."
    ],
    stats: { "Guilin–Yangshuo": "83 km", "Landscape": "Tower karst" },
    formed: null,
    unesco: 2014,
    wiki: "Li_River"
  },
  {
    id: "okavango-delta",
    name: "Okavango Delta",
    type: "River",
    emoji: "🐘",
    country: "Botswana",
    lat: -19.28, lng: 22.90,
    summary: "A vast inland delta where a river spills into the Kalahari Desert instead of the sea, creating a seasonal wildlife paradise.",
    facts: [
      "The Okavango River never reaches the sea; its water evaporates or sinks into the Kalahari sands.",
      "Floodwater from the Angolan highlands takes months to arrive, so the delta floods in the dry season.",
      "It supports large populations of elephants, lions, hippos and African wild dogs.",
      "In 2014 it became the 1,000th site inscribed on the UNESCO World Heritage List."
    ],
    stats: { "Area": "6,000–15,000 km²", "Type": "Endorheic delta" },
    formed: null,
    unesco: 2014,
    wiki: "Okavango_Delta"
  },
  {
    id: "lake-baikal",
    name: "Lake Baikal",
    type: "Lake",
    emoji: "🧊",
    country: "Russia",
    lat: 53.5587, lng: 108.165,
    summary: "The deepest and oldest lake on Earth, holding more fresh water than all of North America's Great Lakes combined.",
    facts: [
      "It is 1,642 m deep at its deepest point.",
      "It holds about 23,600 km³ of water, roughly 22–23% of the world's fresh surface water.",
      "It is estimated to be 25–30 million years old.",
      "It is home to the nerpa, one of the world's only freshwater seals.",
      "Over 300 rivers flow into it, but only one, the Angara, flows out."
    ],
    stats: { "Max depth": "1,642 m", "Length": "636 km", "Age": "25–30 million years" },
    formed: -25000000,
    unesco: 1996,
    wiki: "Lake_Baikal"
  },
  {
    id: "dead-sea",
    name: "Dead Sea",
    type: "Lake",
    emoji: "🧂",
    country: "Jordan / Israel / Palestine",
    lat: 31.559, lng: 35.4732,
    summary: "A hypersaline lake at the lowest point on dry land, so salty that swimmers float effortlessly.",
    facts: [
      "Its shore lies more than 430 m below sea level, the lowest land elevation on Earth.",
      "Its water is about 34% salt, nearly ten times saltier than the ocean.",
      "The lake level has been falling by more than a metre a year.",
      "The Dead Sea Scrolls were found in caves near its shore at Qumran, beginning in 1947.",
      "Almost no fish or plants can live in it, hence its name."
    ],
    stats: { "Surface": "~-430 m", "Salinity": "~34%", "Max depth": "~300 m" },
    formed: null,
    unesco: null,
    wiki: "Dead_Sea"
  },
  {
    id: "lake-titicaca",
    name: "Lake Titicaca",
    type: "Lake",
    emoji: "🛶",
    country: "Peru / Bolivia",
    lat: -15.9254, lng: -69.3354,
    summary: "A sapphire lake high in the Andes, often called the highest navigable lake in the world.",
    facts: [
      "Its surface lies about 3,812 m above sea level.",
      "It is the largest lake in South America by volume.",
      "The Uros people live on floating islands woven from totora reeds.",
      "In Inca legend, the first Inca, Manco Cápac, emerged from its waters.",
      "Landlocked Bolivia keeps naval vessels on the lake."
    ],
    stats: { "Elevation": "3,812 m", "Area": "~8,370 km²" },
    formed: null,
    unesco: null,
    wiki: "Lake_Titicaca"
  },
  {
    id: "lake-natron",
    name: "Lake Natron",
    type: "Lake",
    emoji: "🦩",
    country: "Tanzania",
    lat: -2.4167, lng: 36.0,
    summary: "A blood-red alkaline lake in the Rift Valley that is deadly to most life but a breeding haven for flamingos.",
    facts: [
      "Its water can reach a pH of 10.5 or higher and temperatures up to 60 °C.",
      "Its red colour comes from salt-loving microorganisms.",
      "It is the main breeding ground for East Africa's lesser flamingos.",
      "Its mineral-rich water can calcify the bodies of animals that die in it.",
      "The active volcano Ol Doinyo Lengai rises to its south."
    ],
    stats: { "pH": "up to ~12", "Water temp": "up to 60 °C" },
    formed: null,
    unesco: null,
    wiki: "Lake_Natron"
  },
  {
    id: "plitvice-lakes",
    name: "Plitvice Lakes",
    type: "Lake",
    emoji: "💚",
    country: "Croatia",
    lat: 44.8654, lng: 15.582,
    summary: "Sixteen terraced lakes linked by waterfalls, their colours shifting from azure to emerald through the day.",
    facts: [
      "The lakes are separated by natural dams of travertine that are still growing.",
      "Veliki slap ('Big Waterfall'), at about 78 m, is the tallest waterfall in Croatia.",
      "It became a national park in 1949, the oldest in south-east Europe.",
      "It was among the first sites on the UNESCO World Heritage List, in 1979.",
      "The 1960s Winnetou films were shot here."
    ],
    stats: { "Main lakes": "16", "Tallest fall": "~78 m" },
    formed: null,
    unesco: 1979,
    wiki: "Plitvice_Lakes_National_Park"
  },
  {
    id: "jiuzhaigou",
    name: "Jiuzhaigou",
    type: "Lake",
    emoji: "💎",
    country: "China",
    lat: 33.26, lng: 103.92,
    summary: "A fairy-tale valley of crystal-clear, multicoloured lakes, terraced waterfalls and snow-capped peaks.",
    facts: [
      "Its name means 'Nine Village Valley', after the Tibetan villages along it.",
      "Five Flower Lake is so clear that fallen tree trunks are visible on its bed.",
      "Nuorilang Falls is one of the widest waterfalls in China.",
      "A magnitude 7.0 earthquake in August 2017 damaged the park and drained Sparkling Lake.",
      "It was inscribed by UNESCO in 1992."
    ],
    stats: { "Elevation": "2,000–4,800 m", "Area": "~720 km²" },
    formed: null,
    unesco: 1992,
    wiki: "Jiuzhaigou"
  },
  {
    id: "lake-louise",
    name: "Lake Louise (Banff)",
    type: "Lake",
    emoji: "🏞️",
    country: "Canada",
    lat: 51.4167, lng: -116.2167,
    summary: "A turquoise glacial lake beneath the Victoria Glacier in Banff, Canada's first national park.",
    facts: [
      "Its vivid colour comes from 'rock flour', fine silt ground by glaciers and suspended in the water.",
      "It is named after Princess Louise Caroline Alberta, daughter of Queen Victoria.",
      "The Stoney Nakoda people called it 'Lake of the Little Fishes'.",
      "Banff, created in 1885, is Canada's oldest national park.",
      "It is part of the Canadian Rocky Mountain Parks UNESCO site, inscribed in 1984."
    ],
    stats: { "Elevation": "~1,750 m", "Length": "~2.4 km" },
    formed: null,
    unesco: 1984,
    wiki: "Lake_Louise_(Alberta)"
  },
  {
    id: "lake-hillier",
    name: "Lake Hillier",
    type: "Lake",
    emoji: "🩷",
    country: "Australia",
    lat: -34.0947, lng: 123.2028,
    summary: "A bubblegum-pink lake on Middle Island, separated from the deep-blue Southern Ocean by a thin strip of forest.",
    facts: [
      "Its pink colour is caused by the alga Dunaliella salina and salt-loving archaea and bacteria.",
      "The water stays pink even when collected in a glass.",
      "It is about 600 m long.",
      "It lies on Middle Island in Western Australia's Recherche Archipelago.",
      "Explorer Matthew Flinders recorded the lake in 1802."
    ],
    stats: { "Length": "~600 m", "Width": "~250 m" },
    formed: null,
    unesco: null,
    wiki: "Lake_Hillier"
  },
  {
    id: "sundarbans",
    name: "Sundarbans",
    type: "Rainforest",
    emoji: "🐅",
    country: "Bangladesh / India",
    lat: 21.95, lng: 89.18,
    summary: "The largest mangrove forest in the world, a maze of tidal channels where Bengal tigers swim between islands.",
    facts: [
      "It covers about 10,000 km² of the Ganges–Brahmaputra–Meghna delta.",
      "It is home to the Royal Bengal tiger, adapted to life among mangroves and tides.",
      "Its name is thought to come from the sundari tree, a common mangrove species there.",
      "India's portion was inscribed by UNESCO in 1987 and Bangladesh's in 1997."
    ],
    stats: { "Area": "~10,000 km²", "Type": "Mangrove forest" },
    formed: null,
    unesco: 1987,
    wiki: "Sundarbans"
  },

  // ───────────── Deserts ─────────────
  {
    id: "sahara",
    name: "Sahara Desert",
    type: "Desert",
    emoji: "🐪",
    country: "Algeria / Libya / Egypt + 8 more",
    lat: 23.4, lng: 13.0,
    summary: "The largest hot desert on Earth, a sea of sand, rock and wind stretching across North Africa.",
    facts: [
      "It covers about 9.2 million km², roughly the size of the United States.",
      "Between about 11,000 and 5,000 years ago, the 'Green Sahara' had lakes, grasslands and hippos.",
      "Some of its sand dunes reach heights of around 180 m.",
      "Its highest point is Emi Koussi, a volcano in Chad, at 3,415 m."
    ],
    stats: { "Area": "~9.2 million km²", "Highest point": "3,415 m" },
    formed: -7000000,
    unesco: null,
    wiki: "Sahara"
  },
  {
    id: "atacama-desert",
    name: "Atacama Desert",
    type: "Desert",
    emoji: "🌵",
    country: "Chile",
    lat: -24.5, lng: -69.25,
    summary: "The driest non-polar desert on Earth, so Mars-like that NASA tests rovers there.",
    facts: [
      "Some weather stations in the Atacama have never recorded rain.",
      "It has been hyperarid for at least 3 million years.",
      "Its high, dry skies host the ALMA radio telescope at about 5,000 m.",
      "After rare heavy rains, it erupts in a 'desierto florido' (flowering desert)."
    ],
    stats: { "Area": "~105,000 km²", "Rainfall": "as little as ~1 mm/yr" },
    formed: -3000000,
    unesco: null,
    wiki: "Atacama_Desert"
  },
  {
    id: "sossusvlei",
    name: "Namib Desert (Sossusvlei)",
    type: "Desert",
    emoji: "🏜️",
    country: "Namibia",
    lat: -24.733, lng: 15.367,
    summary: "Towering apricot-red dunes surround white clay pans in the Namib, possibly the world's oldest desert.",
    facts: [
      "The dunes get their red colour from iron oxide coating the sand grains.",
      "'Big Daddy', one of the tallest dunes in the area, rises about 325 m.",
      "In nearby Deadvlei, dead camel thorn trees have stood for centuries, preserved by the dry air.",
      "The Namib Desert is thought to be 55–80 million years old.",
      "The Namib Sand Sea was inscribed by UNESCO in 2013."
    ],
    stats: { "Tallest dune": "~325 m", "Desert age": "55–80 million years" },
    formed: -55000000,
    unesco: 2013,
    wiki: "Namib"
  },
  {
    id: "salar-de-uyuni",
    name: "Salar de Uyuni",
    type: "Desert",
    emoji: "🪞",
    country: "Bolivia",
    lat: -20.1338, lng: -67.4891,
    summary: "The world's largest salt flat, which turns into a vast mirror reflecting the sky after rain.",
    facts: [
      "It covers about 10,582 km² at an altitude of around 3,656 m.",
      "Its surface varies in elevation by less than one metre, making it useful for calibrating satellite altimeters.",
      "It formed from the drying of prehistoric lakes.",
      "It holds one of the world's largest lithium deposits beneath its crust.",
      "Hotels on its edge are built from blocks of salt."
    ],
    stats: { "Area": "10,582 km²", "Elevation": "3,656 m" },
    formed: null,
    unesco: null,
    wiki: "Salar_de_Uyuni"
  },
  {
    id: "danakil-depression",
    name: "Danakil Depression",
    type: "Desert",
    emoji: "🌡️",
    country: "Ethiopia",
    lat: 14.2417, lng: 40.2997,
    summary: "One of the hottest and lowest places on Earth, where acidic yellow-green springs bubble at Dallol.",
    facts: [
      "Parts of the depression lie more than 100 m below sea level.",
      "Dallol recorded an average annual temperature of 34.6 °C between 1960 and 1966, a record for an inhabited place.",
      "The Dallol hydrothermal field has pools so acidic their pH is near zero.",
      "Afar salt miners still cut slabs of salt and carry them out by camel caravan.",
      "It lies in the Afar Triangle, where three tectonic plates are pulling apart."
    ],
    stats: { "Lowest point": "~-125 m", "Avg temp (Dallol)": "34.6 °C" },
    formed: null,
    unesco: null,
    wiki: "Danakil_Depression"
  },
  {
    id: "lencois-maranhenses",
    name: "Lençóis Maranhenses",
    type: "Desert",
    emoji: "🏝️",
    country: "Brazil",
    lat: -2.53, lng: -43.12,
    summary: "Rolling white dunes that fill with thousands of blue and green rainwater lagoons each wet season.",
    facts: [
      "Despite looking like a desert, the area receives around 1,200 mm of rain a year.",
      "Lagoons form between the dunes during the rainy season and peak around mid-year.",
      "Some lagoons contain fish that survive the dry season buried in mud.",
      "The national park was created in 1981 and inscribed by UNESCO in 2024."
    ],
    stats: { "Area": "~1,550 km²", "Dune height": "up to ~40 m" },
    formed: null,
    unesco: 2024,
    wiki: "Lençóis_Maranhenses_National_Park"
  },

  // ───────────── Ice ─────────────
  {
    id: "perito-moreno-glacier",
    name: "Perito Moreno Glacier",
    type: "Glacier & Ice",
    emoji: "🧊",
    country: "Argentina",
    lat: -50.4967, lng: -73.1377,
    summary: "A wall of blue ice 5 km wide that periodically dams Lake Argentino and then bursts in a spectacular rupture.",
    facts: [
      "Its terminal face is about 5 km wide and averages 74 m above the water.",
      "The glacier covers about 250 km² and is 30 km long.",
      "Unlike most glaciers, it has remained roughly stable instead of retreating.",
      "It is named after explorer Francisco 'Perito' Moreno.",
      "It lies in Los Glaciares National Park, inscribed by UNESCO in 1981."
    ],
    stats: { "Length": "30 km", "Front width": "~5 km", "Area": "250 km²" },
    formed: null,
    unesco: 1981,
    wiki: "Perito_Moreno_Glacier"
  },
  {
    id: "greenland-ice-sheet",
    name: "Greenland Ice Sheet",
    type: "Glacier & Ice",
    emoji: "🧊",
    country: "Greenland (Denmark)",
    lat: 72.0, lng: -40.0,
    summary: "The second-largest body of ice on Earth, a dome up to 3 km thick covering about 80% of Greenland.",
    facts: [
      "It covers about 1.71 million km².",
      "If it melted completely, global sea levels would rise by about 7 m.",
      "Its ice is more than 3 km thick at its deepest point.",
      "Sermeq Kujalleq (Jakobshavn Glacier) is one of the fastest-moving glaciers in the world.",
      "Ice cores drilled here preserve a climate record stretching back over 100,000 years."
    ],
    stats: { "Area": "1.71 million km²", "Max thickness": ">3 km" },
    formed: null,
    unesco: null,
    wiki: "Greenland_ice_sheet"
  },
  {
    id: "jokulsarlon",
    name: "Jökulsárlón",
    type: "Glacier & Ice",
    emoji: "🧊",
    country: "Iceland",
    lat: 64.0784, lng: -16.2306,
    summary: "A glacier lagoon filled with drifting icebergs that wash up as glittering 'diamonds' on a black-sand beach.",
    facts: [
      "The lagoon began forming in the 1930s as the Breiðamerkurjökull glacier retreated.",
      "At over 280 m deep, it is the deepest lake in Iceland.",
      "It has appeared in James Bond films 'A View to a Kill' and 'Die Another Day'.",
      "It is part of Vatnajökull National Park, inscribed by UNESCO in 2019."
    ],
    stats: { "Max depth": "~284 m", "Area": "~18 km²" },
    formed: 1935,
    unesco: 2019,
    wiki: "Jökulsárlón"
  },

  // ───────────── Ocean & reef ─────────────
  {
    id: "challenger-deep",
    name: "Challenger Deep (Mariana Trench)",
    type: "Canyon",
    emoji: "🌊",
    country: "Pacific Ocean (near Guam)",
    lat: 11.3733, lng: 142.5917,
    summary: "The deepest known point in Earth's oceans, nearly 11 km down at the bottom of the Mariana Trench.",
    facts: [
      "It is about 10,935 m deep; Mount Everest would fit inside with over 2 km to spare.",
      "Water pressure at the bottom is more than 1,000 times that at the surface.",
      "Jacques Piccard and Don Walsh first reached the bottom in the bathyscaphe Trieste on 23 January 1960.",
      "Filmmaker James Cameron made the first solo dive in 2012.",
      "It is named after HMS Challenger, whose 1875 expedition first sounded the trench."
    ],
    stats: { "Depth": "~10,935 m", "Trench length": "~2,550 km" },
    formed: null,
    unesco: null,
    wiki: "Challenger_Deep"
  },
  {
    id: "great-blue-hole",
    name: "Great Blue Hole",
    type: "Cave",
    emoji: "🔵",
    country: "Belize",
    lat: 17.3157, lng: -87.5348,
    summary: "A near-perfect circle of deep blue in the middle of a turquoise atoll, a giant marine sinkhole.",
    facts: [
      "It is about 318 m across and 124 m deep.",
      "It formed as a limestone cave during ice ages, then flooded when sea levels rose.",
      "Stalactites hang inside it, proof that it was once above water.",
      "Jacques Cousteau brought it to fame in 1971 aboard the Calypso.",
      "It is part of the Belize Barrier Reef Reserve System, inscribed by UNESCO in 1996."
    ],
    stats: { "Diameter": "~318 m", "Depth": "~124 m" },
    formed: null,
    unesco: 1996,
    wiki: "Great_Blue_Hole"
  },
  {
    id: "raja-ampat",
    name: "Raja Ampat",
    type: "Reef",
    emoji: "🐟",
    country: "Indonesia",
    lat: -0.23, lng: 130.51,
    summary: "An archipelago at the heart of the Coral Triangle with the richest marine biodiversity on Earth.",
    facts: [
      "It consists of over 1,500 islands, cays and shoals around four main islands.",
      "Its name means 'Four Kings', after a local legend.",
      "Surveys have recorded more than 550 species of hard coral, about three-quarters of the world's known species.",
      "Its reefs host well over a thousand species of reef fish."
    ],
    stats: { "Islands": "1,500+", "Coral species": "550+" },
    formed: null,
    unesco: null,
    wiki: "Raja_Ampat_Islands"
  },
  {
    id: "bay-of-fundy",
    name: "Bay of Fundy",
    type: "Coast & Fjord",
    emoji: "🌊",
    country: "Canada",
    lat: 45.0, lng: -65.8,
    summary: "Home to the highest tides in the world, where the sea can rise and fall by over 15 m twice a day.",
    facts: [
      "The tidal range at Burntcoat Head in the Minas Basin can exceed 16 m.",
      "At low tide, visitors can walk around the sea-carved 'flowerpot' rocks at Hopewell Rocks.",
      "The tides make the Saint John River's Reversing Falls flow backwards.",
      "The bay is a key feeding ground for endangered North Atlantic right whales."
    ],
    stats: { "Max tidal range": "~16 m", "Length": "~270 km" },
    formed: null,
    unesco: null,
    wiki: "Bay_of_Fundy"
  },
  {
    id: "galapagos-islands",
    name: "Galápagos Islands",
    type: "Island",
    emoji: "🐢",
    country: "Ecuador",
    lat: -0.67, lng: -90.55,
    summary: "Volcanic islands straddling the equator whose unique wildlife helped inspire Darwin's theory of evolution.",
    facts: [
      "Charles Darwin visited aboard HMS Beagle in 1835.",
      "They lie about 1,000 km off the coast of Ecuador.",
      "The marine iguana, found only here, is the world's only sea-going lizard.",
      "Lonesome George, the last Pinta Island tortoise, died in 2012.",
      "In 1978 they were among the very first sites inscribed on the UNESCO World Heritage List."
    ],
    stats: { "Distance from mainland": "~1,000 km", "Oldest islands": "~4 million years" },
    formed: -4000000,
    unesco: 1978,
    wiki: "Galápagos_Islands"
  },
  {
    id: "socotra",
    name: "Socotra",
    type: "Island",
    emoji: "🌲",
    country: "Yemen",
    lat: 12.4634, lng: 53.8237,
    summary: "An isolated Arabian Sea island of otherworldly plants, including umbrella-shaped dragon's blood trees.",
    facts: [
      "About 37% of its plant species are found nowhere else on Earth.",
      "The dragon's blood tree produces a red resin used since ancient times as dye and medicine.",
      "About 90% of its reptile species are endemic.",
      "It was inscribed by UNESCO in 2008."
    ],
    stats: { "Area": "~3,800 km²", "Endemic plants": "~37%" },
    formed: null,
    unesco: 2008,
    wiki: "Socotra"
  },

  // ───────────── Caves ─────────────
  {
    id: "son-doong-cave",
    name: "Sơn Đoòng Cave",
    type: "Cave",
    emoji: "🕳️",
    country: "Vietnam",
    lat: 17.4566, lng: 106.2876,
    summary: "The world's largest known cave passage, big enough to contain its own jungle, river and clouds.",
    facts: [
      "Local man Hồ Khanh found its entrance around 1990, but it was not explored until 2009.",
      "A British Cave Research Association team led by Howard and Deb Limbert surveyed it in 2009.",
      "Parts of the main passage are over 200 m high and 150 m wide.",
      "Collapsed ceilings let sunlight in, allowing a rainforest to grow inside.",
      "It lies in Phong Nha-Kẻ Bàng National Park, inscribed by UNESCO in 2003."
    ],
    stats: { "Volume": "~38.5 million m³", "Max height": "~200 m" },
    formed: null,
    unesco: 2003,
    wiki: "Hang_Sơn_Đoòng"
  },
  {
    id: "waitomo-glowworm-caves",
    name: "Waitomo Glowworm Caves",
    type: "Cave",
    emoji: "✨",
    country: "New Zealand",
    lat: -38.2609, lng: 175.1036,
    summary: "Boats drift silently beneath a ceiling of thousands of glowing larvae, like a starry night underground.",
    facts: [
      "The lights are made by Arachnocampa luminosa, a gnat larva found only in New Zealand.",
      "The larvae glow to lure insects into sticky silk threads.",
      "Māori chief Tane Tinorau and English surveyor Fred Mace explored the caves in 1887.",
      "'Waitomo' comes from the Māori words wai (water) and tomo (hole)."
    ],
    stats: { "Discovered": "1887", "Rock": "Limestone" },
    formed: null,
    unesco: null,
    wiki: "Waitomo_Glowworm_Cave"
  },
  {
    id: "mammoth-cave",
    name: "Mammoth Cave",
    type: "Cave",
    emoji: "🦇",
    country: "United States",
    lat: 37.187, lng: -86.10,
    summary: "The longest known cave system in the world, a labyrinth of limestone passages beneath Kentucky.",
    facts: [
      "More than 670 km of passages have been surveyed, far more than any other known cave.",
      "Saltpetre mined in the cave was used to make gunpowder during the War of 1812.",
      "Stephen Bishop, an enslaved guide, explored and mapped much of it in the 1830s–1850s.",
      "It became a national park in 1941 and a UNESCO World Heritage Site in 1981."
    ],
    stats: { "Surveyed length": "670+ km", "National park": "1941" },
    formed: null,
    unesco: 1981,
    wiki: "Mammoth_Cave_National_Park"
  },

  // ───────────── Rock formations, canyons, valleys ─────────────
  {
    id: "uluru",
    name: "Uluru",
    type: "Rock Formation",
    emoji: "🪨",
    country: "Australia",
    lat: -25.3444, lng: 131.0369,
    summary: "A vast sandstone monolith rising from the red centre of Australia, sacred to the Anangu people.",
    facts: [
      "It rises 348 m above the surrounding plain and is about 9.4 km around.",
      "It glows deep red at sunrise and sunset because of iron oxide in the rock.",
      "Explorer William Gosse named it Ayers Rock in 1873.",
      "Ownership was handed back to the Anangu in 1985.",
      "Climbing Uluru was permanently banned from 26 October 2019."
    ],
    stats: { "Height": "348 m", "Circumference": "9.4 km" },
    formed: -550000000,
    unesco: 1987,
    wiki: "Uluru"
  },
  {
    id: "giants-causeway",
    name: "Giant's Causeway",
    type: "Rock Formation",
    emoji: "🪨",
    country: "United Kingdom (Northern Ireland)",
    lat: 55.2408, lng: -6.5116,
    summary: "About 40,000 interlocking basalt columns stepping into the sea, mostly in neat hexagons.",
    facts: [
      "The columns formed when a thick lava flow cooled and cracked about 50–60 million years ago.",
      "Legend says the giant Finn McCool built it to reach his Scottish rival Benandonner.",
      "The tallest columns are about 12 m high.",
      "Similar columns appear across the sea at Fingal's Cave on Staffa, Scotland.",
      "It was inscribed by UNESCO in 1986."
    ],
    stats: { "Columns": "~40,000", "Age": "50–60 million years" },
    formed: -55000000,
    unesco: 1986,
    wiki: "Giant's_Causeway"
  },
  {
    id: "cappadocia",
    name: "Cappadocia",
    type: "Rock Formation",
    emoji: "🎈",
    country: "Türkiye",
    lat: 38.6431, lng: 34.8289,
    summary: "A surreal landscape of 'fairy chimneys', cave churches and underground cities, best seen from a hot-air balloon.",
    facts: [
      "The fairy chimneys were eroded from thick volcanic tuff laid down by ancient eruptions.",
      "The underground city of Derinkuyu reaches around 85 m deep and could shelter thousands of people.",
      "Early Christians carved churches into the rock and decorated them with frescoes.",
      "Göreme National Park and the Rock Sites of Cappadocia were inscribed by UNESCO in 1985."
    ],
    stats: { "Region": "Central Anatolia", "Derinkuyu depth": "~85 m" },
    formed: null,
    unesco: 1985,
    wiki: "Cappadocia"
  },
  {
    id: "zhangjiajie",
    name: "Zhangjiajie",
    type: "Rock Formation",
    emoji: "🗿",
    country: "China",
    lat: 29.3249, lng: 110.4343,
    summary: "Thousands of sheer sandstone pillars rise from misty forests, inspiring the floating mountains of 'Avatar'.",
    facts: [
      "The park contains over 3,000 quartz-sandstone pillars, many over 200 m tall.",
      "It became China's first national forest park in 1982.",
      "One pillar was renamed 'Avatar Hallelujah Mountain' in 2010 after the film.",
      "The Bailong Elevator carries visitors 326 m up a cliff face.",
      "It is part of the Wulingyuan Scenic Area, inscribed by UNESCO in 1992."
    ],
    stats: { "Pillars": "3,000+", "Established": "1982" },
    formed: null,
    unesco: 1992,
    wiki: "Zhangjiajie_National_Forest_Park"
  },
  {
    id: "tsingy-de-bemaraha",
    name: "Tsingy de Bemaraha",
    type: "Rock Formation",
    emoji: "🪨",
    country: "Madagascar",
    lat: -18.67, lng: 44.75,
    summary: "A 'stone forest' of razor-sharp limestone pinnacles hiding canyons and rare lemurs.",
    facts: [
      "'Tsingy' roughly means 'where one cannot walk barefoot' in Malagasy.",
      "The pinnacles were carved by rainwater dissolving ancient limestone.",
      "Visitors cross the formations on suspension bridges and via ferrata routes.",
      "It was inscribed by UNESCO in 1990."
    ],
    stats: { "Rock": "Limestone karst", "Area": "~1,520 km²" },
    formed: null,
    unesco: 1990,
    wiki: "Tsingy_de_Bemaraha_Strict_Nature_Reserve"
  },
  {
    id: "richat-structure",
    name: "Richat Structure (Eye of the Sahara)",
    type: "Rock Formation",
    emoji: "👁️",
    country: "Mauritania",
    lat: 21.124, lng: -11.402,
    summary: "A 40 km bullseye of concentric rock rings in the Sahara, so large it is best appreciated from orbit.",
    facts: [
      "It is about 40 km in diameter.",
      "It is an eroded geological dome, not an impact crater.",
      "Gemini astronauts used it as a landmark in the 1960s.",
      "Its rings are layers of rock of differing hardness exposed by erosion."
    ],
    stats: { "Diameter": "~40 km", "Origin": "Eroded dome" },
    formed: -100000000,
    unesco: null,
    wiki: "Richat_Structure"
  },
  {
    id: "antelope-canyon",
    name: "Antelope Canyon",
    type: "Canyon",
    emoji: "🌅",
    country: "United States",
    lat: 36.8619, lng: -111.3743,
    summary: "A slot canyon of flowing, wave-like sandstone walls lit by shafts of sunlight.",
    facts: [
      "It was carved by flash floods eroding Navajo Sandstone.",
      "It lies on Navajo Nation land near Page, Arizona, and can only be visited with a Navajo guide.",
      "Upper Antelope Canyon is famous for light beams that reach the floor around midday in summer.",
      "A flash flood in August 1997 killed 11 tourists in Lower Antelope Canyon."
    ],
    stats: { "Rock": "Navajo Sandstone", "Sections": "Upper & Lower" },
    formed: null,
    unesco: null,
    wiki: "Antelope_Canyon"
  },
  {
    id: "monument-valley",
    name: "Monument Valley",
    type: "Rock Formation",
    emoji: "🤠",
    country: "United States",
    lat: 36.998, lng: -110.0985,
    summary: "Towering red sandstone buttes on the Arizona–Utah border, the backdrop of countless Western films.",
    facts: [
      "Its buttes rise up to about 300 m above the valley floor.",
      "It lies within the Navajo Nation, managed as a Navajo Tribal Park.",
      "John Ford filmed 'Stagecoach' (1939) and many other Westerns here.",
      "Forrest Gump ends his cross-country run on the road leading into the valley."
    ],
    stats: { "Butte height": "up to ~300 m", "Elevation": "~1,700 m" },
    formed: null,
    unesco: null,
    wiki: "Monument_Valley"
  },
  {
    id: "bryce-canyon",
    name: "Bryce Canyon",
    type: "Rock Formation",
    emoji: "🧱",
    country: "United States",
    lat: 37.593, lng: -112.1871,
    summary: "Not a canyon at all but a series of amphitheatres filled with thousands of orange rock spires called hoodoos.",
    facts: [
      "It has the largest concentration of hoodoos on Earth.",
      "Hoodoos are shaped by frost wedging, as water freezes and thaws in cracks on many nights a year.",
      "Mormon pioneer Ebenezer Bryce reportedly called it 'a hell of a place to lose a cow'.",
      "It became a national park in 1928."
    ],
    stats: { "Elevation": "2,400–2,700 m", "National park": "1928" },
    formed: null,
    unesco: null,
    wiki: "Bryce_Canyon_National_Park"
  },
  {
    id: "vinicunca",
    name: "Rainbow Mountain (Vinicunca)",
    type: "Mountain",
    emoji: "🌈",
    country: "Peru",
    lat: -13.8694, lng: -71.3028,
    summary: "A ridge in the Andes striped in red, gold, turquoise and lavender by layers of mineral sediments.",
    facts: [
      "Its summit is about 5,200 m above sea level.",
      "The colours come from different minerals, such as iron oxide (red) and chlorite (green).",
      "It was long covered by snow and ice and only became a major attraction in the mid-2010s.",
      "It lies in the Cusco Region, near the sacred mountain Ausangate."
    ],
    stats: { "Elevation": "~5,200 m", "Region": "Cusco" },
    formed: null,
    unesco: null,
    wiki: "Vinicunca"
  },
  {
    id: "yosemite-valley",
    name: "Yosemite Valley",
    type: "Valley & Plateau",
    emoji: "🧗",
    country: "United States",
    lat: 37.7456, lng: -119.5936,
    summary: "A glacier-carved granite valley guarded by El Capitan and Half Dome, cradle of the American national park idea.",
    facts: [
      "El Capitan rises about 900 m from the valley floor.",
      "Yosemite Falls drops 739 m in total, among the tallest waterfalls in North America.",
      "Abraham Lincoln signed the Yosemite Grant in 1864, protecting the valley for public use.",
      "Alex Honnold climbed El Capitan without ropes on 3 June 2017.",
      "Yosemite National Park was inscribed by UNESCO in 1984."
    ],
    stats: { "El Capitan": "~900 m", "Yosemite Falls": "739 m" },
    formed: null,
    unesco: 1984,
    wiki: "Yosemite_Valley"
  },
  {
    id: "great-rift-valley",
    name: "Great Rift Valley",
    type: "Valley & Plateau",
    emoji: "🌍",
    country: "Kenya (East Africa)",
    lat: -0.95, lng: 36.6,
    summary: "A giant crack across East Africa where the continent is slowly being pulled apart.",
    facts: [
      "The East African Rift is splitting the African Plate into the Nubian and Somali plates.",
      "The rift began forming roughly 25 million years ago.",
      "It holds some of Africa's great lakes, including Tanganyika, Malawi and Turkana.",
      "Many of the most important early human fossils were found in or near the rift."
    ],
    stats: { "Spreading rate": "a few mm/yr", "Age": "~25 million years" },
    formed: -25000000,
    unesco: null,
    wiki: "Great_Rift_Valley"
  },
  {
    id: "serengeti",
    name: "Serengeti",
    type: "Valley & Plateau",
    emoji: "🦓",
    country: "Tanzania",
    lat: -2.3333, lng: 34.8333,
    summary: "Endless grassy plains that host the Great Migration, the largest overland movement of mammals on Earth.",
    facts: [
      "More than 1.5 million wildebeest, along with zebras and gazelles, migrate in a yearly loop.",
      "Its name comes from the Maasai word 'siringet', meaning 'endless plains'.",
      "Serengeti National Park covers about 14,750 km².",
      "The park was established in 1951 and inscribed by UNESCO in 1981."
    ],
    stats: { "Area": "~14,750 km²", "Migrating wildebeest": "1.5 million+" },
    formed: null,
    unesco: 1981,
    wiki: "Serengeti_National_Park"
  },
  {
    id: "avenue-of-the-baobabs",
    name: "Avenue of the Baobabs",
    type: "Natural Phenomenon",
    emoji: "🌴",
    country: "Madagascar",
    lat: -20.2508, lng: 44.4183,
    summary: "A dirt road lined with towering, bottle-shaped Grandidier's baobabs, glowing at sunset.",
    facts: [
      "The trees are Grandidier's baobabs, which grow only in Madagascar.",
      "Some trees are around 800 years old and up to 30 m tall.",
      "They are survivors of a dense tropical forest that once covered the area.",
      "It lies near the town of Morondava in western Madagascar."
    ],
    stats: { "Tree height": "up to 30 m", "Tree age": "up to ~800 years" },
    formed: null,
    unesco: null,
    wiki: "Avenue_of_the_Baobabs"
  },

  // ───────────── Coasts & fjords ─────────────
  {
    id: "cliffs-of-moher",
    name: "Cliffs of Moher",
    type: "Coast & Fjord",
    emoji: "🌊",
    country: "Ireland",
    lat: 52.9715, lng: -9.4309,
    summary: "Sheer cliffs plunge more than 200 m into the Atlantic on Ireland's wild west coast.",
    facts: [
      "They reach a maximum height of 214 m and run for about 14 km.",
      "The cliffs are built of shale and sandstone laid down about 320 million years ago.",
      "They host large colonies of seabirds, including Atlantic puffins.",
      "O'Brien's Tower was built on the cliffs in 1835 as a viewing point."
    ],
    stats: { "Height": "214 m", "Length": "~14 km" },
    formed: -320000000,
    unesco: null,
    wiki: "Cliffs_of_Moher"
  },
  {
    id: "geirangerfjord",
    name: "Geirangerfjord",
    type: "Coast & Fjord",
    emoji: "🛳️",
    country: "Norway",
    lat: 62.1049, lng: 7.0943,
    summary: "A deep, narrow fjord framed by sheer cliffs, waterfalls and abandoned mountainside farms.",
    facts: [
      "The fjord is about 15 km long.",
      "The Seven Sisters waterfall faces the Suitor waterfall across the fjord.",
      "Farms such as Skageflå cling to ledges hundreds of metres above the water.",
      "It was inscribed by UNESCO in 2005 together with the Nærøyfjord."
    ],
    stats: { "Length": "~15 km", "Max depth": "~260 m" },
    formed: null,
    unesco: 2005,
    wiki: "Geirangerfjord"
  },
  {
    id: "milford-sound",
    name: "Milford Sound",
    type: "Coast & Fjord",
    emoji: "🏞️",
    country: "New Zealand",
    lat: -44.6414, lng: 167.8974,
    summary: "A glacier-carved fjord where Mitre Peak and waterfalls plunge straight into the sea.",
    facts: [
      "Despite its name, it is a fjord carved by glaciers, not a river-formed sound.",
      "Mitre Peak rises 1,692 m straight out of the water.",
      "It receives around 6,700 mm of rain a year, one of the wettest inhabited places on Earth.",
      "Rudyard Kipling reportedly called it the 'eighth wonder of the world'.",
      "It lies in Fiordland National Park, part of the Te Wāhipounamu UNESCO site (1990)."
    ],
    stats: { "Length": "~15 km", "Mitre Peak": "1,692 m" },
    formed: null,
    unesco: 1990,
    wiki: "Milford_Sound"
  },
  {
    id: "bay-of-kotor",
    name: "Bay of Kotor",
    type: "Coast & Fjord",
    emoji: "⛵",
    country: "Montenegro",
    lat: 42.45, lng: 18.69,
    summary: "A winding bay hemmed in by steep mountains and medieval walled towns on the Adriatic.",
    facts: [
      "Often called Europe's southernmost fjord, it is actually a ria, a drowned river canyon.",
      "The walls of Kotor's old town climb about 4.5 km up the mountainside.",
      "Our Lady of the Rocks is an artificial islet built up by sailors dropping stones.",
      "The Natural and Culturo-Historical Region of Kotor was inscribed by UNESCO in 1979."
    ],
    stats: { "Type": "Ria (drowned river canyon)", "Kotor walls": "~4.5 km" },
    formed: null,
    unesco: 1979,
    wiki: "Bay_of_Kotor"
  }
];

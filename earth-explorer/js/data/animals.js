// Curated species data for the Earth Explorer globe.
// Ranges are approximations drawn as ellipses: [centerLat, centerLng, radiusLatDeg, radiusLngDeg].
// Population figures are best recent published estimates (IUCN, census reports, conservation bodies).

export const ANIMALS = [
  {
    id: 'african-elephant',
    name: 'African Bush Elephant',
    scientific: 'Loxodonta africana',
    group: 'Mammals',
    emoji: '🐘',
    status: 'EN',
    population: '~415,000 (all African elephants)',
    color: '#e8a33d',
    range: [
      [-2, 36, 6, 5],
      [-9, 33, 4, 6],
      [-18, 25, 6, 7],
      [-24, 31.5, 2.5, 1.5],
      [-13, 37.5, 3, 2.5],
      [11.5, 1.5, 1.5, 3],
      [11, 19.5, 1.5, 2.5],
      [15.5, -1.5, 1, 2],
      [3.5, 32.5, 2.5, 3]
    ],
    pastRange: {
      until: 1800,
      zones: [
        [12, 0, 5, 17],
        [11, 25, 5, 10],
        [-3, 30, 12, 12],
        [-20, 25, 10, 10],
        [-30, 24, 4, 7]
      ]
    },
    extinct: null,
    hotspots: [
      { name: 'Amboseli National Park, Kenya', lat: -2.65, lng: 37.26 },
      { name: 'Chobe Riverfront, Botswana', lat: -17.82, lng: 25.15 },
      { name: 'Kruger National Park, South Africa', lat: -23.99, lng: 31.55 },
      { name: 'Hwange National Park, Zimbabwe', lat: -18.73, lng: 26.95 }
    ],
    habitat: 'Savannas, grasslands, woodlands, semi-deserts',
    diet: 'Herbivore – grasses, leaves, bark, roots, fruit',
    size: '3–4 m at the shoulder, bulls up to ~6,000 kg',
    lifespan: '60–70 years',
    speed: '40 km/h',
    summary: 'The largest land animal on Earth, a matriarch-led giant that reshapes whole landscapes by toppling trees, digging waterholes and spreading seeds.',
    facts: [
      'Its pregnancy lasts about 22 months – the longest of any land mammal.',
      'An adult can eat around 150 kg of vegetation in a single day.',
      'Elephants talk with low-frequency rumbles (infrasound) that can be detected kilometres away, partly through the ground via their feet.',
      'Tusks are elongated incisor teeth that keep growing throughout life.',
      'In Mozambique’s Gorongosa, decades of ivory poaching during the civil war left many females born naturally tuskless – a rapid evolutionary response confirmed in a 2021 study.',
      'The trunk is a fusion of nose and upper lip, controlled by tens of thousands of muscle units.'
    ],
    wiki: 'African_bush_elephant'
  },
  {
    id: 'lion',
    name: 'Lion',
    scientific: 'Panthera leo',
    group: 'Mammals',
    emoji: '🦁',
    status: 'VU',
    population: '~20,000–25,000',
    color: '#ffd54f',
    range: [
      [-2.5, 35, 5, 4.5],
      [-11, 33, 4, 5],
      [-19, 24, 5, 6],
      [-24, 31.5, 2.5, 1.5],
      [11.5, 1.5, 1.5, 2.5],
      [13, -13, 0.8, 1],
      [8.5, 20, 2.5, 6],
      [6.5, 35, 2.5, 4],
      [21.2, 70.8, 0.7, 1]
    ],
    pastRange: {
      until: 1850,
      zones: [
        [33, 2, 3, 10],
        [12, 5, 5, 22],
        [-3, 30, 10, 12],
        [-24, 24, 9, 10],
        [32, 40, 6, 10],
        [32, 53, 4, 6],
        [24, 77, 6, 8]
      ]
    },
    extinct: null,
    hotspots: [
      { name: 'Serengeti National Park, Tanzania', lat: -2.33, lng: 34.83 },
      { name: 'Maasai Mara National Reserve, Kenya', lat: -1.49, lng: 35.14 },
      { name: 'Okavango Delta, Botswana', lat: -19.3, lng: 22.9 },
      { name: 'Gir National Park, India', lat: 21.13, lng: 70.79 }
    ],
    habitat: 'Savannas, grasslands, open woodlands, scrub',
    diet: 'Carnivore – zebra, wildebeest, buffalo, antelope',
    size: '~1.2 m at the shoulder, males 150–250 kg',
    lifespan: '10–14 years in the wild',
    speed: '80 km/h',
    summary: 'The only truly social big cat, living in prides whose roars carry across the savanna at night.',
    facts: [
      'A lion’s roar can be heard up to about 8 km away.',
      'Lions rest or sleep for up to 20 hours a day.',
      'Lions have vanished from over 90% of their historic range, which once included North Africa, the Middle East and most of India.',
      'The last wild Asiatic lions live only in and around Gir, India – the 2025 census counted 891, up from 674 in 2020.',
      'Studies in the Serengeti found that darker manes signal a healthier, more testosterone-rich male, and females prefer them.',
      'Lionesses in a pride often give birth at the same time and will nurse each other’s cubs.'
    ],
    wiki: 'Lion'
  },
  {
    id: 'tiger',
    name: 'Tiger',
    scientific: 'Panthera tigris',
    group: 'Mammals',
    emoji: '🐅',
    status: 'EN',
    population: '~5,500',
    color: '#ff6f00',
    range: [
      [22, 79, 4.5, 5],
      [12, 76, 2.5, 1.5],
      [28.5, 82, 1.5, 4],
      [21.9, 89, 0.7, 1],
      [27, 92.5, 1.5, 3],
      [46, 135, 4, 4],
      [-1.5, 102, 4, 3],
      [15, 99, 2, 1],
      [4.8, 102.3, 1.5, 1]
    ],
    pastRange: {
      until: 1900,
      zones: [
        [39, 55, 5, 15],
        [42, 78, 3, 8],
        [28, 110, 8, 10],
        [45, 128, 6, 8],
        [22, 80, 9, 9],
        [15, 102, 8, 5],
        [-7.5, 110, 1, 4],
        [-8.4, 115.1, 0.3, 0.5]
      ]
    },
    extinct: null,
    hotspots: [
      { name: 'Ranthambore National Park, India', lat: 26.02, lng: 76.5 },
      { name: 'Bandhavgarh National Park, India', lat: 23.7, lng: 81.03 },
      { name: 'Kanha National Park, India', lat: 22.33, lng: 80.61 },
      { name: 'Jim Corbett National Park, India', lat: 29.53, lng: 78.77 }
    ],
    habitat: 'Tropical forests, mangroves, grasslands, taiga',
    diet: 'Carnivore – deer, wild boar, gaur, buffalo',
    size: 'Up to ~3.3 m long incl. tail, males 180–300 kg',
    lifespan: '10–15 years in the wild',
    speed: '60 km/h',
    summary: 'The largest cat species, a solitary striped ambush hunter whose wild range has shrunk to small pockets of South and Southeast Asia and the Russian Far East.',
    facts: [
      'No two tigers share the same stripe pattern, and the stripes are on the skin as well as the fur.',
      'India is home to roughly three-quarters of the world’s wild tigers.',
      'Unlike most cats, tigers love water and are strong swimmers – Sundarbans tigers swim between mangrove islands.',
      'Tigers once ranged from eastern Turkey across Central Asia to Bali; the Caspian, Javan and Bali subspecies were all lost in the 20th century.',
      'Genetic studies show the extinct Caspian tiger was almost identical to today’s Amur (Siberian) tiger.',
      'A hungry tiger can eat more than 30 kg of meat in one sitting.'
    ],
    wiki: 'Tiger'
  },
  {
    id: 'giraffe',
    name: 'Giraffe',
    scientific: 'Giraffa camelopardalis',
    group: 'Mammals',
    emoji: '🦒',
    status: 'VU',
    population: '~117,000–140,000',
    color: '#a1662f',
    range: [
      [-2, 36, 5, 3.5],
      [2, 39, 2.5, 2],
      [-19.5, 24, 4, 6],
      [-24, 31.5, 2.5, 1.5],
      [-13, 31.7, 1, 1],
      [-19.5, 15.5, 2, 2],
      [13.4, 2.6, 0.6, 0.9],
      [10.5, 20, 2, 3],
      [2.2, 31.8, 0.7, 0.7]
    ],
    pastRange: {
      until: 1850,
      zones: [
        [13, 0, 4, 15],
        [10, 25, 4, 10],
        [-3, 35, 8, 6],
        [-20, 23, 8, 10]
      ]
    },
    extinct: null,
    hotspots: [
      { name: 'Etosha National Park, Namibia', lat: -18.85, lng: 16.33 },
      { name: 'Murchison Falls National Park, Uganda', lat: 2.25, lng: 31.8 },
      { name: 'Maasai Mara National Reserve, Kenya', lat: -1.49, lng: 35.14 },
      { name: 'Kouré Giraffe Zone, Niger', lat: 13.31, lng: 2.58 }
    ],
    habitat: 'Savannas and open woodlands',
    diet: 'Herbivore – acacia and other tree leaves, shoots, fruit',
    size: 'Up to ~5.7 m tall, 800–1,900 kg',
    lifespan: '~25 years in the wild',
    speed: '60 km/h',
    summary: 'The tallest animal alive, browsing treetops other herbivores can’t reach with a dark prehensile tongue.',
    facts: [
      'Despite its length, a giraffe’s neck has just seven vertebrae – the same number as a human’s.',
      'Its tongue is about 45–50 cm long and dark-coloured, possibly to protect it from sunburn.',
      'A giraffe’s heart weighs around 11 kg and produces roughly twice the blood pressure of a human’s to pump blood up the neck.',
      'Calves drop about 1.5–2 m to the ground at birth and can stand within an hour.',
      'Each giraffe’s coat pattern is unique, like a fingerprint.',
      'In 2025 the IUCN recognised giraffes as four separate species: northern, reticulated, Masai and southern.'
    ],
    wiki: 'Giraffe'
  },
  {
    id: 'zebra',
    name: 'Plains Zebra',
    scientific: 'Equus quagga',
    group: 'Mammals',
    emoji: '🦓',
    status: 'NT',
    population: '~500,000',
    color: '#eceff1',
    range: [
      [-3, 35.5, 6, 4.5],
      [5, 37, 2.5, 2.5],
      [-10, 32, 4, 5],
      [-18, 25, 5, 7],
      [-25, 30, 3, 3],
      [-18.5, 15.5, 2.5, 3]
    ],
    pastRange: {
      until: 1880,
      zones: [
        [-31, 23, 4, 7]
      ]
    },
    extinct: null,
    hotspots: [
      { name: 'Serengeti National Park, Tanzania', lat: -2.33, lng: 34.83 },
      { name: 'Makgadikgadi Pans National Park, Botswana', lat: -20.7, lng: 25.3 },
      { name: 'Etosha National Park, Namibia', lat: -18.85, lng: 16.33 },
      { name: 'Ngorongoro Crater, Tanzania', lat: -3.18, lng: 35.56 }
    ],
    habitat: 'Grasslands, savannas, open woodlands',
    diet: 'Herbivore – mainly grasses',
    size: '1.1–1.45 m at the shoulder, 175–385 kg',
    lifespan: '~20 years in the wild',
    speed: '65 km/h',
    summary: 'The most common zebra, whose bold stripes ripple across East and southern Africa in great migrating herds alongside wildebeest.',
    facts: [
      'No two zebras have exactly the same stripe pattern.',
      'Experiments with horses wearing striped coats show stripes deter biting horseflies from landing.',
      'Plains zebras in Namibia and Botswana make a ~500 km round-trip migration – one of the longest land migrations in Africa.',
      'The quagga, a half-striped form from South Africa, was a plains zebra subspecies; the last one died in Amsterdam Zoo in 1883.',
      'Zebras sleep standing up and rely on herd members to watch for predators.',
      'When attacked, family groups bunch together so the mass of stripes confuses predators.'
    ],
    wiki: 'Plains_zebra'
  },
  {
    id: 'cheetah',
    name: 'Cheetah',
    scientific: 'Acinonyx jubatus',
    group: 'Mammals',
    emoji: '🐆',
    status: 'VU',
    population: '~6,500–7,100',
    color: '#c6ff00',
    range: [
      [-21, 20, 4.5, 6],
      [-2, 36, 3.5, 3.5],
      [-17, 28, 3, 4],
      [-24.5, 31, 2, 1.5],
      [6, 36, 2.5, 3],
      [23, 7, 3, 4],
      [11.5, 1.5, 1.5, 2.5],
      [34.5, 56, 2.5, 3],
      [25.6, 77.4, 0.6, 0.6]
    ],
    pastRange: {
      until: 1900,
      zones: [
        [12, 10, 7, 25],
        [-12, 28, 12, 12],
        [24, 45, 7, 10],
        [36, 58, 5, 10],
        [23, 77, 7, 7]
      ]
    },
    extinct: null,
    hotspots: [
      { name: 'Maasai Mara National Reserve, Kenya', lat: -1.49, lng: 35.14 },
      { name: 'Serengeti National Park, Tanzania', lat: -2.33, lng: 34.83 },
      { name: 'Kgalagadi Transfrontier Park, South Africa/Botswana', lat: -25.77, lng: 20.39 },
      { name: 'Kuno National Park, India', lat: 25.6, lng: 77.4 }
    ],
    habitat: 'Open savannas, grasslands, semi-deserts',
    diet: 'Carnivore – gazelles, impala, small antelope, hares',
    size: '67–94 cm at the shoulder, 21–72 kg',
    lifespan: '10–12 years in the wild',
    speed: '~100 km/h',
    summary: 'The fastest land animal, a lightweight sprinter built for explosive daytime chases across open plains.',
    facts: [
      'A captive cheetah named Sarah ran 100 m in 5.95 seconds in 2012, hitting about 98 km/h.',
      'Cheetahs cannot roar – they purr, chirp and yelp instead.',
      'Semi-retractable claws work like running spikes, giving grip during high-speed turns.',
      'The dark “tear marks” under the eyes are thought to reduce sun glare.',
      'Fewer than 30 critically endangered Asiatic cheetahs are thought to survive, all in Iran.',
      'In 2022 cheetahs from Namibia were released in India’s Kuno National Park, 70 years after the species was declared extinct there.'
    ],
    wiki: 'Cheetah'
  },
  {
    id: 'gorilla',
    name: 'Mountain Gorilla',
    scientific: 'Gorilla beringei beringei',
    group: 'Mammals',
    emoji: '🦍',
    status: 'EN',
    population: '~1,063',
    color: '#5d4037',
    range: [
      [-1.43, 29.52, 0.25, 0.35],
      [-1.02, 29.67, 0.13, 0.13]
    ],
    pastRange: null,
    extinct: null,
    hotspots: [
      { name: 'Volcanoes National Park, Rwanda', lat: -1.48, lng: 29.53 },
      { name: 'Bwindi Impenetrable National Park, Uganda', lat: -1.05, lng: 29.7 },
      { name: 'Mgahinga Gorilla National Park, Uganda', lat: -1.37, lng: 29.65 },
      { name: 'Virunga National Park (Bukima), DR Congo', lat: -1.38, lng: 29.43 }
    ],
    habitat: 'Montane and bamboo forests, 2,200–4,300 m',
    diet: 'Herbivore – leaves, stems, shoots, roots, occasional insects',
    size: 'Silverbacks ~1.5–1.8 m upright, up to ~200 kg',
    lifespan: '35–40 years',
    speed: '~30–40 km/h (charging)',
    summary: 'A gentle, family-living great ape that survives only on the misty volcanic slopes of the Virungas and in Uganda’s Bwindi forest.',
    facts: [
      'It is the only great ape whose population is growing: about 1,063 were counted in the 2018 censuses, up from roughly 620 in 1989.',
      'In 2018 it was moved from Critically Endangered to Endangered on the IUCN Red List.',
      'Its fur is longer and thicker than other gorillas’, an adaptation to cold mountain forests.',
      'Every gorilla builds a fresh nest of leaves and branches to sleep in each night.',
      'An adult male can eat around 30 kg of vegetation a day.',
      'Dian Fossey began her pioneering study of mountain gorillas at Karisoke, Rwanda, in 1967.'
    ],
    wiki: 'Mountain_gorilla'
  },
  {
    id: 'orangutan',
    name: 'Bornean Orangutan',
    scientific: 'Pongo pygmaeus',
    group: 'Mammals',
    emoji: '🦧',
    status: 'CR',
    population: '~104,700',
    color: '#ff8a65',
    range: [
      [5.2, 117.6, 0.8, 1.1],
      [1.3, 112, 1, 1.5],
      [-2, 113, 1.3, 2],
      [0.8, 116.8, 1.2, 0.9]
    ],
    pastRange: null,
    extinct: null,
    hotspots: [
      { name: 'Tanjung Puting National Park, Indonesia', lat: -2.75, lng: 111.95 },
      { name: 'Danum Valley, Sabah, Malaysia', lat: 4.96, lng: 117.79 },
      { name: 'Kinabatangan River, Sabah, Malaysia', lat: 5.55, lng: 118.2 },
      { name: 'Sepilok Orangutan Rehabilitation Centre, Malaysia', lat: 5.86, lng: 117.95 }
    ],
    habitat: 'Lowland and peat-swamp rainforests',
    diet: 'Omnivore – mostly fruit, plus leaves, bark, insects',
    size: 'Males up to ~1 m head-body, 50–100 kg',
    lifespan: '35–45 years in the wild',
    speed: 'Slow – moves mainly through the treetops',
    summary: 'The world’s largest tree-dwelling mammal, a thoughtful red ape whose name means “person of the forest” in Malay.',
    facts: [
      'Mothers give birth only about every 6–8 years – one of the longest gaps between births of any mammal.',
      'Borneo lost roughly 100,000 orangutans between 1999 and 2015, mainly to logging, palm-oil expansion and hunting.',
      'Dominant males grow large cheek pads (flanges) and a throat sac to make “long calls” heard up to a kilometre away.',
      'Orangutans use leaves as umbrellas and rain hats and use sticks as tools.',
      'Their arms can span over 2 m, longer than their body height.',
      'Young orangutans stay with their mothers for about eight years, learning which forest foods to eat.'
    ],
    wiki: 'Bornean_orangutan'
  },
  {
    id: 'giant-panda',
    name: 'Giant Panda',
    scientific: 'Ailuropoda melanoleuca',
    group: 'Mammals',
    emoji: '🐼',
    status: 'VU',
    population: '~1,864',
    color: '#64dd17',
    range: [
      [32.9, 104, 1, 1],
      [30.8, 103.1, 1, 0.6],
      [33.7, 107.8, 0.4, 1.2],
      [28.7, 102.9, 1, 0.8]
    ],
    pastRange: null,
    extinct: null,
    hotspots: [
      { name: 'Wolong National Nature Reserve, Sichuan', lat: 31.03, lng: 103.2 },
      { name: 'Foping National Nature Reserve, Shaanxi', lat: 33.68, lng: 107.85 },
      { name: 'Tangjiahe National Nature Reserve, Sichuan', lat: 32.6, lng: 104.8 }
    ],
    habitat: 'Cool, wet mountain bamboo forests, 1,200–3,400 m',
    diet: 'Herbivore – about 99% bamboo',
    size: '1.2–1.9 m long, 70–125 kg',
    lifespan: '~20 years in the wild',
    speed: '~30 km/h',
    summary: 'A bamboo-munching bear of China’s misty mountains and a global symbol of conservation success.',
    facts: [
      'A panda eats roughly 12–38 kg of bamboo a day, spending up to 14 hours feeding.',
      'It has a “pseudo-thumb” – an enlarged wrist bone – for gripping bamboo stalks.',
      'Newborn cubs weigh only about 100 g, roughly 1/900 of their mother’s weight.',
      'China’s fourth national survey counted 1,864 wild pandas, and in 2016 the IUCN moved them from Endangered to Vulnerable.',
      'Despite its diet, it has the short gut of a carnivore and digests only about a fifth of what it eats.',
      'China opened Giant Panda National Park in 2021, linking many reserves across Sichuan, Shaanxi and Gansu.'
    ],
    wiki: 'Giant_panda'
  },
  {
    id: 'polar-bear',
    name: 'Polar Bear',
    scientific: 'Ursus maritimus',
    group: 'Mammals',
    emoji: '🐻‍❄️',
    status: 'VU',
    population: '~26,000',
    color: '#b3e5fc',
    range: [
      [72, -150, 4, 14],
      [74, -100, 7, 22],
      [60, -86, 4, 7],
      [74, -42, 8, 20],
      [78, 25, 4, 22],
      [76, 85, 4, 30],
      [72, -170, 3, 9],
      [73, 165, 3, 13]
    ],
    pastRange: null,
    extinct: null,
    hotspots: [
      { name: 'Churchill, Manitoba, Canada', lat: 58.77, lng: -94.16 },
      { name: 'Svalbard, Norway', lat: 78.22, lng: 15.65 },
      { name: 'Wrangel Island, Russia', lat: 71.2, lng: -179.4 },
      { name: 'Kaktovik, Alaska, USA', lat: 70.13, lng: -143.6 }
    ],
    habitat: 'Arctic sea ice and coasts',
    diet: 'Carnivore – mainly ringed and bearded seals',
    size: 'Males 2.4–3 m long, 350–700 kg',
    lifespan: '15–25 years in the wild',
    speed: '40 km/h',
    summary: 'The largest land carnivore, a sea-ice hunter so tied to the ocean that it is legally classified as a marine mammal.',
    facts: [
      'Under its white-looking fur, a polar bear’s skin is black; the hairs themselves are translucent.',
      'One radio-collared female swam 687 km non-stop over nine days in the Beaufort Sea.',
      'They hunt mainly by waiting at seal breathing holes in the sea ice.',
      'There are 19 recognised subpopulations around the Arctic.',
      'Shrinking summer sea ice is the main threat – it forces bears ashore where food is scarce.',
      'Only pregnant females den through winter; other bears stay active.'
    ],
    wiki: 'Polar_bear'
  },
  {
    id: 'brown-bear',
    name: 'Brown Bear',
    scientific: 'Ursus arctos',
    group: 'Mammals',
    emoji: '🐻',
    status: 'LC',
    population: '~200,000',
    color: '#8d6e63',
    range: [
      [62, -140, 8, 20],
      [47, -112, 4, 5],
      [64, 22, 4, 8],
      [62, 75, 8, 38],
      [60, 140, 9, 24],
      [46, 23, 3, 8],
      [41, 44, 2.5, 5],
      [34, 85, 4, 12],
      [56, 160, 4, 4],
      [43.5, 143, 1, 1.5]
    ],
    pastRange: {
      until: 1850,
      zones: [
        [48, 5, 7, 14],
        [34, -3, 2, 6],
        [40, -112, 9, 13],
        [28, -106, 4, 4]
      ]
    },
    extinct: null,
    hotspots: [
      { name: 'Brooks Falls, Katmai National Park, Alaska', lat: 58.55, lng: -155.79 },
      { name: 'Kurile Lake, Kamchatka, Russia', lat: 51.45, lng: 156.98 },
      { name: 'Hayden Valley, Yellowstone, USA', lat: 44.65, lng: -110.46 },
      { name: 'Kuusamo, Finland', lat: 65.97, lng: 29.18 }
    ],
    habitat: 'Forests, tundra, mountains, coasts',
    diet: 'Omnivore – roots, berries, salmon, carrion, mammals',
    size: '1.5–2.8 m long, 80–600+ kg',
    lifespan: '20–30 years',
    speed: '56 km/h',
    summary: 'The world’s most widespread bear, from Alaskan salmon rivers to the forests of Finland and the Tibetan Plateau.',
    facts: [
      'Kodiak brown bears rival the polar bear as the largest land carnivore, with big males topping 600 kg.',
      'In late summer they enter hyperphagia, eating up to around 20,000 calories a day before hibernation.',
      'During hibernation they don’t eat, drink, urinate or defecate for months, recycling waste into protein.',
      'Katmai National Park’s online “Fat Bear Week” lets millions of people vote for the fattest salmon-fed bear each autumn.',
      'The grizzly on California’s state flag is extinct in the state; the last one was seen in the 1920s.',
      'The Atlas bear of North Africa, the continent’s only native bear, was wiped out in the 19th century.'
    ],
    wiki: 'Brown_bear'
  },
  {
    id: 'gray-wolf',
    name: 'Gray Wolf',
    scientific: 'Canis lupus',
    group: 'Mammals',
    emoji: '🐺',
    status: 'LC',
    population: '~200,000–250,000',
    color: '#90a4ae',
    range: [
      [60, -115, 12, 40],
      [46, -110, 3, 5],
      [47, -90, 2, 4],
      [60, 90, 12, 58],
      [45, 92, 5, 18],
      [42, -6, 1.5, 2.5],
      [43, 12, 3, 4],
      [50, 20, 5, 10],
      [22, 76, 5, 5],
      [31, 42, 5, 8]
    ],
    pastRange: {
      until: 1900,
      zones: [
        [40, -98, 12, 25],
        [25, -103, 5, 6],
        [50, 5, 9, 14],
        [55, 25, 8, 15],
        [36, 138, 4, 4]
      ]
    },
    extinct: null,
    hotspots: [
      { name: 'Lamar Valley, Yellowstone, USA', lat: 44.9, lng: -110.2 },
      { name: 'Denali National Park, Alaska', lat: 63.73, lng: -148.9 },
      { name: 'Sierra de la Culebra, Spain', lat: 41.9, lng: -6.4 },
      { name: 'Isle Royale National Park, USA', lat: 48.0, lng: -88.9 }
    ],
    habitat: 'Forests, tundra, mountains, grasslands, deserts',
    diet: 'Carnivore – deer, elk, moose, bison, smaller mammals',
    size: '60–85 cm at the shoulder, 30–80 kg',
    lifespan: '6–8 years in the wild',
    speed: '60 km/h',
    summary: 'A highly social pack hunter and the wild ancestor of the domestic dog, once the most widespread land mammal after humans.',
    facts: [
      'All domestic dogs descend from ancient wolves domesticated more than 15,000 years ago.',
      'Reintroducing wolves to Yellowstone in 1995 triggered a “trophic cascade”, changing elk behaviour and letting streamside willows recover.',
      'A howl can carry for around 10 km across open country.',
      'Wolves were wiped out from most of the contiguous USA and western Europe by the early 20th century; Japan’s wolves were gone by 1905.',
      'They have naturally recolonised Germany since 2000, which now has well over 150 packs.',
      'Wolves can travel more than 50 km in a single day while hunting.'
    ],
    wiki: 'Wolf'
  },
  {
    id: 'red-fox',
    name: 'Red Fox',
    scientific: 'Vulpes vulpes',
    group: 'Mammals',
    emoji: '🦊',
    status: 'LC',
    population: 'Millions',
    color: '#ff1744',
    range: [
      [50, -100, 14, 33],
      [64, -150, 5, 12],
      [52, 10, 11, 17],
      [57, 90, 12, 55],
      [35, 60, 7, 15],
      [32, 2, 3, 9],
      [27, 31, 4, 2],
      [37, 138, 5, 4],
      [-32, 141, 7, 13]
    ],
    pastRange: null,
    extinct: null,
    hotspots: [
      { name: 'Shiretoko National Park, Japan', lat: 44.07, lng: 145.12 },
      { name: 'Hampstead Heath, London, UK', lat: 51.56, lng: -0.165 },
      { name: 'Prince Edward Island National Park, Canada', lat: 46.42, lng: -63.08 }
    ],
    habitat: 'Forests, grasslands, tundra, farmland, cities',
    diet: 'Omnivore – rodents, rabbits, birds, insects, fruit, scraps',
    size: '45–90 cm body plus bushy tail, 2.2–14 kg',
    lifespan: '2–5 years in the wild',
    speed: '50 km/h',
    summary: 'The most widely distributed wild carnivore on Earth, equally at home in Arctic tundra and city back gardens.',
    facts: [
      'Its natural range spans North America, Europe, Asia and North Africa – larger than any other wild member of the order Carnivora.',
      'Introduced to Australia for sport hunting in the 1800s, it has since contributed to the extinction of several small native mammals.',
      'Foxes “mouse” by leaping high and diving into snow; a study found their pounces are most successful when aimed roughly north-east, hinting they may sense Earth’s magnetic field.',
      'A Soviet experiment begun in 1959 bred silver foxes for tameness, producing dog-like foxes with floppy ears and wagging tails.',
      'Foxes bury surplus food in scattered caches and remember where to find them.',
      'Thousands of foxes live in cities such as London and Bristol.'
    ],
    wiki: 'Red_fox'
  },
  {
    id: 'wild-horse',
    name: 'Przewalski’s Horse',
    scientific: 'Equus ferus przewalskii',
    group: 'Mammals',
    emoji: '🐎',
    status: 'EN',
    population: '~2,500 (incl. captivity)',
    color: '#9e9d24',
    range: [
      [47.8, 105.9, 0.3, 0.4],
      [47.95, 95.1, 0.3, 0.4],
      [45.5, 93.6, 0.8, 1.2],
      [45.0, 89.2, 0.8, 1.2],
      [51.3, 29.9, 0.4, 0.6],
      [47.6, 21.1, 0.2, 0.3]
    ],
    pastRange: {
      until: 1880,
      zones: [
        [46, 92, 5, 12],
        [48, 70, 4, 12],
        [43, 100, 3, 8]
      ]
    },
    extinct: null,
    hotspots: [
      { name: 'Hustai National Park, Mongolia', lat: 47.75, lng: 105.87 },
      { name: 'Great Gobi B Strictly Protected Area, Mongolia', lat: 45.55, lng: 93.65 },
      { name: 'Hortobágy National Park, Hungary', lat: 47.58, lng: 21.15 }
    ],
    habitat: 'Dry steppe and semi-desert grasslands',
    diet: 'Herbivore – grasses and shrubs',
    size: '1.2–1.4 m at the shoulder, ~200–340 kg',
    lifespan: '~20 years',
    speed: '~60 km/h',
    summary: 'A stocky, dun-coloured horse of the Mongolian steppe that went extinct in the wild and was brought back from zoos.',
    facts: [
      'The last wild Przewalski’s horse was seen in Mongolia’s Dzungarian Gobi in 1969.',
      'Every living Przewalski’s horse descends from just 12–15 founder animals kept in captivity.',
      'It has 66 chromosomes, two more than the domestic horse’s 64.',
      'Reintroductions to Mongolia began in 1992, and the species improved from Extinct in the Wild to Endangered in 2011.',
      'A 2018 genetic study suggested these horses are feral descendants of horses herded by the Botai culture of Kazakhstan about 5,500 years ago.',
      'A free-roaming herd released in the 1990s now thrives inside the Chernobyl Exclusion Zone.'
    ],
    wiki: 'Przewalski\'s_horse'
  },
  {
    id: 'american-bison',
    name: 'American Bison',
    scientific: 'Bison bison',
    group: 'Mammals',
    emoji: '🦬',
    status: 'NT',
    population: '~31,000 in conservation herds',
    color: '#6a1b9a',
    range: [
      [44.7, -110.5, 0.5, 0.7],
      [59.5, -113, 1.8, 2],
      [61.5, -116.5, 1, 1.5],
      [37.9, -110.8, 0.4, 0.4],
      [43.6, -103, 0.8, 1.5],
      [47, -103.5, 0.5, 0.6],
      [62.5, -158.5, 1, 2],
      [30.9, -108.4, 0.3, 0.4],
      [36.8, -96.4, 0.25, 0.25],
      [53.6, -112.8, 0.3, 0.4]
    ],
    pastRange: {
      until: 1800,
      zones: [
        [42, -100, 12, 15],
        [37, -84, 6, 8],
        [57, -118, 6, 12],
        [64, -150, 4, 10],
        [27, -103, 4, 4]
      ]
    },
    extinct: null,
    hotspots: [
      { name: 'Lamar Valley, Yellowstone, USA', lat: 44.9, lng: -110.2 },
      { name: 'Custer State Park, South Dakota, USA', lat: 43.75, lng: -103.42 },
      { name: 'Theodore Roosevelt National Park, USA', lat: 46.97, lng: -103.54 },
      { name: 'Wood Buffalo National Park, Canada', lat: 59.4, lng: -113.1 }
    ],
    habitat: 'Prairies, river valleys, boreal meadows',
    diet: 'Herbivore – grasses and sedges',
    size: 'Up to ~1.9 m at the shoulder, bulls up to ~1,000 kg',
    lifespan: '15–20 years',
    speed: '55 km/h',
    summary: 'North America’s heaviest land animal, which once roamed the plains in the tens of millions and was hunted to the brink of extinction.',
    facts: [
      'An estimated 30–60 million bison once lived in North America; by 1889 fewer than 1,000 remained.',
      'Yellowstone is the only place in the USA where bison have lived continuously since prehistoric times.',
      'It became the official national mammal of the United States in 2016.',
      'Despite weighing up to a tonne, bison can jump fences nearly 1.8 m high.',
      'A raised tail is a warning sign that a bison may charge.',
      'Roughly 400,000 more bison live in commercial herds raised for meat.'
    ],
    wiki: 'American_bison'
  },
  {
    id: 'red-kangaroo',
    name: 'Red Kangaroo',
    scientific: 'Osphranter rufus',
    group: 'Mammals',
    emoji: '🦘',
    status: 'LC',
    population: 'Millions (varies with rainfall)',
    color: '#c62828',
    range: [
      [-27, 136, 6, 12],
      [-24, 120, 5, 5],
      [-21, 140, 3, 6],
      [-31, 144, 3, 4]
    ],
    pastRange: null,
    extinct: null,
    hotspots: [
      { name: 'Sturt National Park, NSW', lat: -29.1, lng: 141.75 },
      { name: 'Ikara-Flinders Ranges National Park, SA', lat: -31.5, lng: 138.6 },
      { name: 'Mungo National Park, NSW', lat: -33.73, lng: 143.05 }
    ],
    habitat: 'Arid and semi-arid grasslands, open plains',
    diet: 'Herbivore – grasses and forbs',
    size: 'Males up to ~1.6 m head-body plus 1 m tail, up to ~90 kg',
    lifespan: '12–22 years',
    speed: '~60 km/h',
    summary: 'The world’s largest marsupial, bounding across the Australian outback on powerful spring-loaded hind legs.',
    facts: [
      'A single bound can cover around 8 m, and hopping uses elastic tendons that make high speeds surprisingly energy-efficient.',
      'Females can pause an embryo’s development (embryonic diapause) until conditions improve.',
      'A mother can have an embryo, a pouch joey and a young-at-foot at once, producing two different kinds of milk at the same time.',
      'A newborn joey is about the size of a jellybean and crawls unaided into the pouch.',
      'Red kangaroos can survive long periods without drinking, getting moisture from plants.',
      'Males “box” each other, balancing on their tails to deliver kicks.'
    ],
    wiki: 'Red_kangaroo'
  },
  {
    id: 'koala',
    name: 'Koala',
    scientific: 'Phascolarctos cinereus',
    group: 'Mammals',
    emoji: '🐨',
    status: 'VU',
    population: '~224,000–524,000',
    color: '#9fa8da',
    range: [
      [-20, 146.5, 3, 2],
      [-26, 150.5, 3.5, 2.5],
      [-31, 151, 3, 1.8],
      [-37, 145.5, 1.8, 3.5],
      [-35, 138.8, 1, 0.6],
      [-35.8, 137.2, 0.3, 0.8]
    ],
    pastRange: null,
    extinct: null,
    hotspots: [
      { name: 'Magnetic Island, Queensland', lat: -19.15, lng: 146.85 },
      { name: 'Kennett River, Great Otway NP, Victoria', lat: -38.67, lng: 143.86 },
      { name: 'Phillip Island Koala Conservation Reserve, Victoria', lat: -38.49, lng: 145.25 }
    ],
    habitat: 'Eucalyptus forests and woodlands',
    diet: 'Herbivore – almost exclusively eucalyptus leaves',
    size: '60–85 cm, 4–15 kg',
    lifespan: '10–15 years',
    speed: 'Slow climber; short fast bursts on the ground',
    summary: 'A tree-hugging marsupial of eastern Australia that survives on toxic eucalyptus leaves and sleeps most of the day.',
    facts: [
      'Koalas sleep or rest for up to 18–22 hours a day to conserve the little energy their diet provides.',
      'Eucalyptus leaves are toxic to most animals; koalas rely on specialised gut microbes to detoxify them.',
      'Joeys eat “pap”, a special soft dropping from their mother, to acquire those gut microbes.',
      'Koala fingerprints are so similar to human ones that they could confuse a crime scene.',
      'Males bellow with a voice far deeper than expected, thanks to extra vocal folds outside the larynx.',
      'Koalas in Queensland, New South Wales and the ACT were listed as Endangered by the Australian government in 2022.'
    ],
    wiki: 'Koala'
  },
  {
    id: 'snow-leopard',
    name: 'Snow Leopard',
    scientific: 'Panthera uncia',
    group: 'Mammals',
    emoji: '🐆',
    status: 'VU',
    population: '~4,000–6,500',
    color: '#00bfa5',
    range: [
      [30, 82, 3, 10],
      [35, 95, 3, 8],
      [37, 72, 2.5, 4],
      [42, 78, 2, 6],
      [48, 92, 3, 8],
      [44, 102, 2, 4]
    ],
    pastRange: null,
    extinct: null,
    hotspots: [
      { name: 'Hemis National Park, Ladakh, India', lat: 34.0, lng: 77.45 },
      { name: 'Kibber, Spiti Valley, India', lat: 32.33, lng: 78.01 },
      { name: 'Sanjiangyuan, Qinghai, China', lat: 33.9, lng: 96.0 }
    ],
    habitat: 'Alpine and subalpine mountains, 3,000–5,500 m',
    diet: 'Carnivore – blue sheep, ibex, marmots, pikas',
    size: '75–130 cm body plus 80–105 cm tail, 22–55 kg',
    lifespan: '10–12 years in the wild',
    speed: '~55–65 km/h',
    summary: 'The elusive “ghost of the mountains”, a smoky-grey cat that hunts across the steep cliffs of Central and South Asia.',
    facts: [
      'Its thick tail, nearly as long as its body, is used for balance and wrapped around the face like a scarf when resting.',
      'Snow leopards cannot roar; they communicate with a puffing “chuff”, mews and growls.',
      'Wide, fur-covered paws act like snowshoes.',
      'A large nasal cavity warms and humidifies the thin, frigid mountain air.',
      'They can leap several times their own body length to catch prey on steep slopes.',
      'Their range spans 12 countries, with China holding the largest share.'
    ],
    wiki: 'Snow_leopard'
  },
  {
    id: 'hippopotamus',
    name: 'Hippopotamus',
    scientific: 'Hippopotamus amphibius',
    group: 'Mammals',
    emoji: '🦛',
    status: 'VU',
    population: '~115,000–130,000',
    color: '#f06292',
    range: [
      [-3, 33, 6, 5],
      [-13, 32, 4, 4],
      [-18, 24, 3, 6],
      [-24, 32, 3, 1.5],
      [11, -4, 3, 10],
      [9, 24, 3, 8],
      [-0.5, 29.5, 1, 1]
    ],
    pastRange: {
      until: 1800,
      zones: [
        [27, 31, 5, 1.5],
        [-30, 25, 4, 7]
      ]
    },
    extinct: null,
    hotspots: [
      { name: 'South Luangwa National Park, Zambia', lat: -13.08, lng: 31.8 },
      { name: 'Kazinga Channel, Queen Elizabeth NP, Uganda', lat: -0.18, lng: 29.9 },
      { name: 'Okavango Delta, Botswana', lat: -19.3, lng: 22.9 },
      { name: 'Lake Naivasha, Kenya', lat: -0.77, lng: 36.36 }
    ],
    habitat: 'Rivers, lakes and swamps beside grasslands',
    diet: 'Herbivore – mainly grasses, grazed at night',
    size: '3.3–5.2 m long, 1,300–3,200 kg',
    lifespan: '40–50 years',
    speed: '30 km/h',
    summary: 'A massive semi-aquatic grazer that spends its days in rivers and its nights grazing on land – and one of Africa’s most dangerous animals.',
    facts: [
      'Hippos secrete a reddish fluid, nicknamed “blood sweat”, that acts as a sunscreen and may be antibacterial.',
      'Their closest living relatives are whales and dolphins.',
      'Hippos can’t really swim – they walk or bounce along river bottoms and can hold their breath for about 5 minutes.',
      'At night they leave the water and may walk several kilometres to graze.',
      'Hippos were found along the Nile in Egypt in ancient times but disappeared from the lower Nile by the early 19th century.',
      'Descendants of four hippos imported by Pablo Escobar now form an invasive population of well over 100 in Colombia.'
    ],
    wiki: 'Hippopotamus'
  },
  {
    id: 'white-rhino',
    name: 'Southern White Rhinoceros',
    scientific: 'Ceratotherium simum simum',
    group: 'Mammals',
    emoji: '🦏',
    status: 'NT',
    population: '~15,750',
    color: '#546e7a',
    range: [
      [-24, 31.5, 2, 1],
      [-28.2, 31.9, 0.6, 0.5],
      [-26.5, 31.5, 0.6, 0.5],
      [-25.5, 27, 2, 3],
      [-20, 30, 2, 2],
      [-19.5, 22.5, 1.5, 2],
      [-20, 17, 2, 2],
      [0, 37, 1.5, 1]
    ],
    pastRange: {
      until: 1850,
      zones: [
        [-24, 26, 7, 9],
        [-17, 28, 4, 6],
        [-15, 16, 3, 4]
      ]
    },
    extinct: null,
    hotspots: [
      { name: 'Hluhluwe-iMfolozi Park, South Africa', lat: -28.22, lng: 31.95 },
      { name: 'Kruger National Park, South Africa', lat: -24.0, lng: 31.5 },
      { name: 'Matobo National Park, Zimbabwe', lat: -20.55, lng: 28.5 },
      { name: 'Lake Nakuru National Park, Kenya', lat: -0.36, lng: 36.09 }
    ],
    habitat: 'Grassland and savanna',
    diet: 'Herbivore – short grasses',
    size: '1.7–1.85 m at the shoulder, up to ~3,600 kg',
    lifespan: '40–50 years',
    speed: '50 km/h',
    summary: 'The largest rhino and the biggest land mammal after the elephants, a square-lipped grazer rescued from near-extinction.',
    facts: [
      'By around 1895 perhaps only 20–50 southern white rhinos survived, all in what is now Hluhluwe-iMfolozi Park.',
      'Conservation, including the 1960s “Operation Rhino” relocations, rebuilt the population to over 20,000 by the 2010s.',
      'Poaching, drought and recounts cut the African white rhino total to about 15,750 by the end of 2024.',
      'Its wide, square upper lip is built for mowing grass, unlike the hooked lip of the browsing black rhino.',
      'Rhino horn is made of keratin – the same protein as fingernails.',
      'White rhinos use communal dung heaps (middens) as message boards to signal territory.'
    ],
    wiki: 'Southern_white_rhinoceros'
  },
  {
    id: 'dromedary',
    name: 'Dromedary Camel',
    scientific: 'Camelus dromedarius',
    group: 'Mammals',
    emoji: '🐪',
    status: 'EW',
    population: '~35 million (domestic)',
    color: '#ffcc80',
    range: [
      [20, 3, 7, 15],
      [17, 25, 6, 8],
      [7, 44, 5, 5],
      [23, 47, 7, 9],
      [30, 62, 5, 9],
      [27, 72, 2.5, 3],
      [-25, 130, 6, 10]
    ],
    pastRange: null,
    extinct: null,
    hotspots: [
      { name: 'Erg Chebbi, Merzouga, Morocco', lat: 31.15, lng: -4.0 },
      { name: 'Wahiba Sands, Oman', lat: 22.0, lng: 58.75 },
      { name: 'Pushkar Camel Fair, India', lat: 26.49, lng: 74.55 },
      { name: 'Uluru-Kata Tjuta region, Australia', lat: -25.24, lng: 130.99 }
    ],
    habitat: 'Hot deserts and dry scrublands',
    diet: 'Herbivore – thorny shrubs, dry grasses, salty plants',
    size: '1.8–2 m at the shoulder, 400–600 kg',
    lifespan: '~40 years',
    speed: '65 km/h (short bursts)',
    summary: 'The one-humped “ship of the desert”, domesticated in Arabia and so well adapted that it can go for long stretches without drinking.',
    facts: [
      'Truly wild dromedaries died out long ago; today they are domestic, plus a large feral herd in central Australia.',
      'Australia’s feral camels, descended from animals imported for outback transport, form the world’s largest free-living dromedary population.',
      'The hump stores fat, not water.',
      'A thirsty camel can drink over 100 litres of water in about 10 minutes.',
      'Its body temperature can swing by several degrees during the day, saving water that would otherwise be lost as sweat.',
      'Like other camelids, it has oval red blood cells, which keep flowing when the animal is dehydrated.'
    ],
    wiki: 'Dromedary'
  },
  {
    id: 'moose',
    name: 'Moose',
    scientific: 'Alces alces',
    group: 'Mammals',
    emoji: '🫎',
    status: 'LC',
    population: '~1.5 million',
    color: '#33691e',
    range: [
      [62, -145, 6, 15],
      [55, -105, 7, 28],
      [47, -70, 3, 6],
      [48.5, -56, 1.5, 2.5],
      [47, -90, 2, 4],
      [45, -111, 3, 3],
      [63, 20, 5, 9],
      [56, 35, 6, 15],
      [60, 100, 8, 45],
      [52, 135, 5, 10]
    ],
    pastRange: null,
    extinct: null,
    hotspots: [
      { name: 'Denali National Park, Alaska', lat: 63.73, lng: -148.9 },
      { name: 'Algonquin Provincial Park, Canada', lat: 45.55, lng: -78.5 },
      { name: 'Gros Morne National Park, Canada', lat: 49.6, lng: -57.75 },
      { name: 'Biebrza National Park, Poland', lat: 53.6, lng: 22.6 }
    ],
    habitat: 'Boreal and mixed forests, wetlands, lakeshores',
    diet: 'Herbivore – twigs, leaves, bark, aquatic plants',
    size: 'Up to ~2.1 m at the shoulder, males 380–700 kg',
    lifespan: '15–20 years',
    speed: '56 km/h',
    summary: 'The largest member of the deer family, a long-legged browser of northern forests and lakes.',
    facts: [
      'A bull’s antlers can span about 1.8 m and are shed and regrown every year.',
      'Moose are strong swimmers and dive underwater to feed on aquatic plants.',
      'Just four moose introduced to Newfoundland in 1904 grew into a population of over 100,000.',
      'In Europe the same animal is called an “elk” (älg, Elch, łoś).',
      'Their long legs let them wade through deep snow and step over fallen logs.',
      'Winter ticks can infest a single moose in the tens of thousands, a growing threat as winters warm.'
    ],
    wiki: 'Moose'
  },
  {
    id: 'jaguar',
    name: 'Jaguar',
    scientific: 'Panthera onca',
    group: 'Mammals',
    emoji: '🐆',
    status: 'NT',
    population: '~173,000',
    color: '#d500f9',
    range: [
      [-5, -62, 8, 12],
      [-17.5, -57, 3, 2.5],
      [16.5, -89.5, 3, 3],
      [12, -85, 3, 2],
      [21, -104, 3, 2],
      [-22, -61, 3, 3],
      [-25.5, -53.5, 1.5, 1.5],
      [4.5, -57, 3, 5],
      [7, -69, 2.5, 4],
      [29.5, -109.5, 1.2, 1]
    ],
    pastRange: {
      until: 1900,
      zones: [
        [32, -108, 3.5, 6],
        [-33, -62, 5, 5],
        [-22, -45, 5, 5]
      ]
    },
    extinct: null,
    hotspots: [
      { name: 'Porto Jofre, Pantanal, Brazil', lat: -17.37, lng: -56.77 },
      { name: 'Cockscomb Basin Wildlife Sanctuary, Belize', lat: 16.77, lng: -88.46 },
      { name: 'Iguaçu National Park, Brazil', lat: -25.6, lng: -54.3 },
      { name: 'Corcovado National Park, Costa Rica', lat: 8.54, lng: -83.59 }
    ],
    habitat: 'Rainforests, wetlands, dry forests, scrub',
    diet: 'Carnivore – capybara, peccaries, caimans, deer, turtles',
    size: '1.1–1.85 m body, 56–96 kg (Pantanal males heavier)',
    lifespan: '12–15 years in the wild',
    speed: '~80 km/h (short bursts)',
    summary: 'The largest cat of the Americas, a powerful, water-loving hunter with the strongest bite for its size of any big cat.',
    facts: [
      'Jaguars often kill by biting directly through the skull of their prey, and can crack turtle shells.',
      'They are excellent swimmers and regularly hunt caimans in Pantanal rivers.',
      'Unlike leopards, jaguars have spots inside many of their rosettes.',
      '“Black panthers” in the Americas are melanistic jaguars whose rosettes are still faintly visible.',
      'Jaguars once ranged into the southwestern USA, and lone males are still occasionally recorded in Arizona.',
      'Its name comes from the Tupi–Guarani word “yaguara”.'
    ],
    wiki: 'Jaguar'
  },
  {
    id: 'emperor-penguin',
    name: 'Emperor Penguin',
    scientific: 'Aptenodytes forsteri',
    group: 'Birds',
    emoji: '🐧',
    status: 'NT',
    population: '~600,000',
    color: '#3d5afe',
    range: [
      [-70, 0, 3, 30],
      [-67.5, 70, 3, 40],
      [-67, 140, 3, 30],
      [-75, 172, 3, 6],
      [-74, -160, 3, 15],
      [-73, -110, 3, 35],
      [-74, -45, 4, 30]
    ],
    pastRange: null,
    extinct: null,
    hotspots: [
      { name: 'Snow Hill Island, Antarctica', lat: -64.52, lng: -57.44 },
      { name: 'Atka Bay, Antarctica', lat: -70.6, lng: -8.1 },
      { name: 'Cape Washington, Ross Sea', lat: -74.65, lng: 165.4 }
    ],
    habitat: 'Antarctic fast sea ice and surrounding ocean',
    diet: 'Carnivore – fish, krill, squid',
    size: '1–1.3 m tall, 22–45 kg',
    lifespan: '~20 years',
    speed: '6–9 km/h swimming',
    summary: 'The tallest and heaviest penguin, the only animal that breeds on Antarctic sea ice in the depths of the polar winter.',
    facts: [
      'Males incubate the single egg on their feet for about two months through the Antarctic winter without eating.',
      'Huddles of thousands of birds keep warm in temperatures below −40 °C and winds over 100 km/h.',
      'Emperors are the deepest-diving birds, recorded at over 500 m, and can stay under for more than 20 minutes.',
      'Many colonies were discovered from space by spotting brown guano stains on the ice in satellite images.',
      'In 2022, four of five colonies in the Bellingshausen Sea lost almost all their chicks when sea ice broke up early.',
      'The global population is estimated at roughly a quarter of a million breeding pairs.'
    ],
    wiki: 'Emperor_penguin'
  },
  {
    id: 'bald-eagle',
    name: 'Bald Eagle',
    scientific: 'Haliaeetus leucocephalus',
    group: 'Birds',
    emoji: '🦅',
    status: 'LC',
    population: '~316,700 (lower 48 US, 2020)',
    color: '#1e88e5',
    range: [
      [58, -140, 5, 15],
      [54, -100, 7, 28],
      [46, -122, 4, 4],
      [44, -90, 4, 8],
      [40, -75, 5, 5],
      [29, -83, 3, 4],
      [42, -110, 5, 8],
      [31, -95, 2, 4]
    ],
    pastRange: null,
    extinct: null,
    hotspots: [
      { name: 'Chilkat Bald Eagle Preserve, Alaska', lat: 59.38, lng: -135.95 },
      { name: 'Brackendale, British Columbia', lat: 49.78, lng: -123.15 },
      { name: 'Conowingo Dam, Maryland', lat: 39.66, lng: -76.17 },
      { name: 'Klamath Basin, Oregon/California', lat: 41.95, lng: -121.6 }
    ],
    habitat: 'Coasts, lakes and rivers with tall trees',
    diet: 'Carnivore – mainly fish, also waterbirds and carrion',
    size: 'Wingspan 1.8–2.3 m, 3–6.3 kg',
    lifespan: '~20 years (record 38)',
    speed: '~120–160 km/h in a dive',
    summary: 'North America’s national bird, a fish-snatching sea eagle that made one of conservation’s great comebacks.',
    facts: [
      'Only about 417 nesting pairs remained in the lower 48 US states in 1963, largely due to DDT pesticide thinning eggshells.',
      'After DDT was banned in 1972, numbers rebounded and the species was removed from the US endangered list in 2007.',
      'A nest in St. Petersburg, Florida measured 2.9 m wide and 6 m deep, weighing an estimated 2 tonnes – among the largest bird nests ever recorded.',
      '“Bald” comes from an old English word meaning white, referring to the white head.',
      'It has been on the Great Seal of the United States since 1782 and was formally made the national bird by law in 2024.',
      'Bald eagles often steal fish from ospreys rather than catching their own.'
    ],
    wiki: 'Bald_eagle'
  },
  {
    id: 'flamingo',
    name: 'Greater Flamingo',
    scientific: 'Phoenicopterus roseus',
    group: 'Birds',
    emoji: '🦩',
    status: 'LC',
    population: '~550,000–680,000',
    color: '#ff4081',
    range: [
      [39, 10, 4, 15],
      [17, -16, 4, 2.5],
      [-1, 36, 5, 2.5],
      [-21, 20, 4, 6],
      [29, 51, 5, 8],
      [50.4, 69, 1.5, 2.5],
      [23.5, 70, 1.5, 2.5],
      [9, 80, 1.5, 1]
    ],
    pastRange: null,
    extinct: null,
    hotspots: [
      { name: 'Pont de Gau, Camargue, France', lat: 43.48, lng: 4.41 },
      { name: 'Fuente de Piedra Lagoon, Spain', lat: 37.1, lng: -4.75 },
      { name: 'Walvis Bay Lagoon, Namibia', lat: -22.96, lng: 14.5 },
      { name: 'Little Rann of Kutch, India', lat: 23.6, lng: 71.2 }
    ],
    habitat: 'Salt lakes, lagoons, mudflats, salt pans',
    diet: 'Filter-feeder – brine shrimp, algae, small invertebrates',
    size: '110–150 cm tall, 2–4 kg',
    lifespan: '30–40 years (captive record over 80)',
    speed: '~50–60 km/h in flight',
    summary: 'The largest and most widespread flamingo, a pink filter-feeder that gathers in huge flocks on salty lagoons from Spain to India.',
    facts: [
      'Its pink colour comes from carotenoid pigments in the algae and brine shrimp it eats; chicks hatch grey.',
      'Flamingos feed with their heads upside down, pumping water through comb-like plates in the bill.',
      'The backward-bending “knee” is actually its ankle; the real knee is hidden up under the feathers.',
      'Standing on one leg requires less muscle effort than standing on two, according to a 2017 study.',
      'Both parents feed their chick with a red “crop milk” produced in their throats.',
      'A greater flamingo at Adelaide Zoo lived to at least 83 years old.'
    ],
    wiki: 'Greater_flamingo'
  },
  {
    id: 'scarlet-macaw',
    name: 'Scarlet Macaw',
    scientific: 'Ara macao',
    group: 'Birds',
    emoji: '🦜',
    status: 'LC',
    population: '~20,000–50,000',
    color: '#d50000',
    range: [
      [-6, -63, 8, 11],
      [6, -62, 3, 6],
      [17, -90.5, 1.2, 1.5],
      [14.5, -84.5, 1.2, 1.5],
      [9, -84, 1, 1],
      [7.5, -81, 0.6, 0.8]
    ],
    pastRange: {
      until: 1900,
      zones: [
        [17, -94, 3, 4],
        [13, -87, 3, 4]
      ]
    },
    extinct: null,
    hotspots: [
      { name: 'Tambopata National Reserve, Peru', lat: -12.95, lng: -69.55 },
      { name: 'Carara National Park, Costa Rica', lat: 9.78, lng: -84.6 },
      { name: 'Corcovado National Park, Costa Rica', lat: 8.54, lng: -83.59 },
      { name: 'Copán Ruins, Honduras', lat: 14.84, lng: -89.14 }
    ],
    habitat: 'Humid lowland rainforests, riverbanks',
    diet: 'Herbivore – fruit, nuts, seeds, flowers, clay',
    size: '81–96 cm long incl. tail, ~1 kg',
    lifespan: '40–50 years (longer in captivity)',
    speed: '~56 km/h in flight',
    summary: 'A blazing red, yellow and blue parrot of tropical American rainforests, often seen in raucous pairs flying over the canopy.',
    facts: [
      'It is the national bird of Honduras.',
      'Flocks gather at riverbank clay licks (collpas), probably to get sodium missing from their diet.',
      'Pairs bond for life and fly so close together their wings almost touch.',
      'Its powerful bill can crack hard nuts and seeds that other animals can’t open.',
      'Scarlet macaws were sacred to the ancient Maya and appear in carvings at Copán.',
      'Habitat loss and the pet trade wiped them out from much of Central America; reintroduction projects are bringing them back.'
    ],
    wiki: 'Scarlet_macaw'
  },
  {
    id: 'white-stork',
    name: 'White Stork',
    scientific: 'Ciconia ciconia',
    group: 'Birds',
    emoji: '🐦',
    status: 'LC',
    population: '~700,000',
    color: '#18ffff',
    range: [
      [40, -5, 3, 4],
      [52, 22, 5, 12],
      [41.5, 30, 3, 10],
      [34, -2, 2, 6],
      [38.5, 46, 3, 5],
      [40, 66, 2, 4],
      [32, 35, 3, 2.5],
      [13, -5, 4, 14],
      [-2, 34, 10, 5],
      [-25, 27, 5, 6]
    ],
    pastRange: null,
    extinct: null,
    hotspots: [
      { name: 'Żywkowo “stork village”, Poland', lat: 54.37, lng: 20.66 },
      { name: 'Hula Valley, Israel', lat: 33.1, lng: 35.61 },
      { name: 'NaturOparC, Hunawihr, Alsace, France', lat: 48.18, lng: 7.31 },
      { name: 'Los Barruecos, Extremadura, Spain', lat: 39.45, lng: -6.5 }
    ],
    habitat: 'Farmland, wet meadows, grasslands, savannas',
    diet: 'Carnivore – insects, frogs, fish, rodents, worms',
    size: '100–115 cm tall, wingspan 155–215 cm, 2.3–4.5 kg',
    lifespan: '20–30 years',
    speed: '~45 km/h in flight',
    summary: 'A long-distance soaring migrant that nests on European rooftops in summer and winters across Africa, linked to folklore about delivering babies.',
    facts: [
      'Migrating storks avoid long sea crossings, funnelling over the Strait of Gibraltar and the Bosporus where rising thermals help them soar.',
      'In 1822 a stork shot in Germany carried an African spear in its neck – this “Pfeilstorch” helped prove that birds migrate.',
      'Adults are nearly voiceless and communicate by loudly clattering their bills.',
      'Poland hosts roughly a fifth of the world’s breeding white storks.',
      'Many Iberian storks now skip migration and spend winter feeding at landfill sites.',
      'Storks reuse and enlarge the same nest for years; some rooftop nests are decades old.'
    ],
    wiki: 'White_stork'
  },
  {
    id: 'ostrich',
    name: 'Common Ostrich',
    scientific: 'Struthio camelus',
    group: 'Birds',
    emoji: '🪶',
    status: 'LC',
    population: 'Unknown (declining)',
    color: '#bf360c',
    range: [
      [-2, 37, 5, 4],
      [-21, 22, 5, 7],
      [-31, 21, 2.5, 4],
      [14, 19, 2, 5],
      [15.5, 8, 1.5, 3]
    ],
    pastRange: {
      until: 1900,
      zones: [
        [30, 0, 4, 12],
        [15, 0, 4, 20],
        [26, 42, 6, 8]
      ]
    },
    extinct: null,
    hotspots: [
      { name: 'Etosha National Park, Namibia', lat: -18.85, lng: 16.33 },
      { name: 'Cape of Good Hope, South Africa', lat: -34.36, lng: 18.47 },
      { name: 'Nairobi National Park, Kenya', lat: -1.37, lng: 36.86 },
      { name: 'Maasai Mara National Reserve, Kenya', lat: -1.49, lng: 35.14 }
    ],
    habitat: 'Savannas, semi-deserts, open plains',
    diet: 'Omnivore – plants, seeds, insects, small reptiles',
    size: '2.1–2.75 m tall, 63–145 kg',
    lifespan: '30–40 years',
    speed: '70 km/h',
    summary: 'The largest living bird, flightless but the fastest runner on two legs, striding across African plains.',
    facts: [
      'It lays the largest eggs of any living bird, each weighing about 1.4 kg.',
      'Its eyes, about 5 cm across, are the largest of any land animal and bigger than its brain.',
      'Ostriches are the only birds with just two toes on each foot.',
      'They do not bury their heads in sand – they lower them to turn eggs in their ground nests.',
      'A forward kick from an ostrich can kill a lion.',
      'The Arabian ostrich, a subspecies, was hunted to extinction by the 1960s; the Somali ostrich is now treated as a separate species.'
    ],
    wiki: 'Common_ostrich'
  },
  {
    id: 'blue-whale',
    name: 'Blue Whale',
    scientific: 'Balaenoptera musculus',
    group: 'Marine Life',
    emoji: '🐋',
    status: 'EN',
    population: '~5,000–15,000',
    color: '#0d47a1',
    range: [
      [-62, 0, 6, 55],
      [-62, 115, 6, 55],
      [-62, -115, 6, 55],
      [27, -120, 10, 8],
      [9, -90, 3, 5],
      [62, -25, 7, 18],
      [6, 80, 4, 8],
      [-38, 130, 4, 14],
      [-42, -75, 4, 3],
      [48.5, -65, 2, 4]
    ],
    pastRange: null,
    extinct: null,
    hotspots: [
      { name: 'Monterey Bay, California', lat: 36.8, lng: -122.0 },
      { name: 'Mirissa, Sri Lanka', lat: 5.95, lng: 80.46 },
      { name: 'Loreto, Gulf of California, Mexico', lat: 26.0, lng: -111.3 },
      { name: 'Gulf of Corcovado, Chile', lat: -43.5, lng: -73.5 }
    ],
    habitat: 'Open oceans worldwide, feeding in cold upwelling waters',
    diet: 'Filter-feeder – krill',
    size: 'Up to ~30 m long, up to ~190 tonnes',
    lifespan: '80–90 years',
    speed: '~30–50 km/h (bursts)',
    summary: 'The largest animal known to have ever lived, a krill-gulping giant whose calls travel hundreds of kilometres through the ocean.',
    facts: [
      'A blue whale’s heart weighs about 180 kg – roughly the weight of a small motorbike.',
      'During the feeding season a single whale can eat up to about 16 tonnes of krill a day.',
      'Its calls reach about 188 decibels, louder than a jet engine, and can be heard across ocean basins.',
      'Whaling killed around 360,000 blue whales in the Southern Hemisphere during the 20th century.',
      'Antarctic blue whales were reduced to perhaps 360 animals by the early 1970s – under 1% of their original numbers.',
      'A calf gains about 90 kg a day while nursing.'
    ],
    wiki: 'Blue_whale'
  },
  {
    id: 'humpback-whale',
    name: 'Humpback Whale',
    scientific: 'Megaptera novaeangliae',
    group: 'Marine Life',
    emoji: '🐳',
    status: 'LC',
    population: '~135,000',
    color: '#26c6da',
    range: [
      [44, -60, 6, 12],
      [19, -68, 3, 6],
      [67, -5, 5, 18],
      [57, -145, 5, 15],
      [21, -157, 2, 3],
      [18, -103, 5, 8],
      [-64, -60, 5, 28],
      [-25, 154, 10, 3],
      [-19, -174, 3, 3],
      [-17, -37, 4, 3]
    ],
    pastRange: null,
    extinct: null,
    hotspots: [
      { name: 'Au‘au Channel, Maui, Hawaii', lat: 20.85, lng: -156.65 },
      { name: 'Samaná Bay, Dominican Republic', lat: 19.2, lng: -69.3 },
      { name: 'Vava‘u, Tonga', lat: -18.65, lng: -173.98 },
      { name: 'Hervey Bay, Queensland, Australia', lat: -25.2, lng: 152.9 }
    ],
    habitat: 'All oceans; polar feeding grounds and tropical breeding grounds',
    diet: 'Filter-feeder – krill and small schooling fish',
    size: '12–16 m long, 25–30 tonnes',
    lifespan: '45–50+ years',
    speed: '~27 km/h',
    summary: 'An acrobatic singer of the seas, famous for breaching, bubble-net feeding and some of the longest migrations of any mammal.',
    facts: [
      'Its pectoral fins can reach about 5 m long – nearly a third of its body length, the longest of any whale.',
      'Males sing long, complex songs, and new song themes spread eastward across the South Pacific from population to population.',
      'Groups hunt cooperatively by blowing spiralling “bubble nets” to trap fish.',
      'One female was tracked travelling about 9,800 km from Brazil to Madagascar.',
      'The black-and-white pattern under each tail fluke is unique, letting researchers identify individuals.',
      'After whaling was banned, numbers recovered so well that the IUCN moved it to Least Concern in 2008.'
    ],
    wiki: 'Humpback_whale'
  },
  {
    id: 'great-white-shark',
    name: 'Great White Shark',
    scientific: 'Carcharodon carcharias',
    group: 'Marine Life',
    emoji: '🦈',
    status: 'VU',
    population: 'Unknown',
    color: '#7c4dff',
    range: [
      [-35, 22, 3, 8],
      [-37, 135, 5, 15],
      [-42, 172, 5, 5],
      [32, -121, 7, 6],
      [37, -71, 7, 5],
      [38, 15, 4, 10],
      [36, 140, 4, 5],
      [23, -138, 4, 7],
      [-28, -112, 4, 6]
    ],
    pastRange: null,
    extinct: null,
    hotspots: [
      { name: 'Gansbaai, South Africa', lat: -34.58, lng: 19.35 },
      { name: 'Guadalupe Island, Mexico', lat: 29.05, lng: -118.3 },
      { name: 'Neptune Islands, South Australia', lat: -35.33, lng: 136.12 },
      { name: 'Chatham, Cape Cod, USA', lat: 41.67, lng: -69.95 }
    ],
    habitat: 'Coastal and open temperate oceans',
    diet: 'Carnivore – seals, sea lions, fish, dolphins, carrion',
    size: '4–6 m long, up to ~2,000 kg',
    lifespan: '70+ years',
    speed: '~40 km/h (bursts)',
    summary: 'The largest predatory fish, a warm-bodied ocean hunter that can launch itself clear of the water to ambush seals.',
    facts: [
      'Radiocarbon dating of vertebrae shows great whites can live over 70 years.',
      'Special pores called ampullae of Lorenzini let them sense the tiny electric fields of living prey.',
      'They keep parts of their body warmer than the surrounding water, giving them faster muscles in cold seas.',
      'A shark named Nicole swam from South Africa to Australia and back – about 20,000 km – in under nine months.',
      'Since 2017, orcas hunting sharks for their livers have driven great whites away from parts of South Africa’s coast.',
      'Many great whites gather each year at a remote mid-Pacific spot between Baja California and Hawaii nicknamed the “White Shark Café”.'
    ],
    wiki: 'Great_white_shark'
  },
  {
    id: 'orca',
    name: 'Orca (Killer Whale)',
    scientific: 'Orcinus orca',
    group: 'Marine Life',
    emoji: '🐋',
    status: 'DD',
    population: '~50,000+',
    color: '#00e676',
    range: [
      [52, -140, 9, 18],
      [65, -10, 7, 18],
      [-65, -60, 5, 40],
      [-65, 50, 5, 40],
      [-66, 150, 5, 25],
      [-42, -63, 3, 3],
      [-46, 51, 3, 5],
      [50, 155, 6, 10],
      [-35, 118, 3, 10],
      [36, -7, 2, 3]
    ],
    pastRange: null,
    extinct: null,
    hotspots: [
      { name: 'Lime Kiln Point, San Juan Island, USA', lat: 48.52, lng: -123.15 },
      { name: 'Skjervøy, Northern Norway', lat: 70.03, lng: 20.97 },
      { name: 'Punta Norte, Península Valdés, Argentina', lat: -42.07, lng: -63.75 },
      { name: 'Bremer Canyon, Western Australia', lat: -34.75, lng: 119.6 }
    ],
    habitat: 'All oceans, most common in cold coastal waters',
    diet: 'Carnivore – fish, seals, sharks, penguins, other whales',
    size: '6–8 m long (males up to ~9.8 m), up to ~6,000 kg',
    lifespan: '50–80+ years (females live longer)',
    speed: '~55 km/h',
    summary: 'The largest dolphin and the ocean’s top predator, living in tight family pods with their own hunting cultures and dialects.',
    facts: [
      'Orcas are one of very few animals besides humans in which females go through menopause; older females lead their pods.',
      'Different ecotypes specialise in different prey – some eat only fish, others hunt seals or whales – and rarely interbreed.',
      'Each pod has its own repertoire of calls, a vocal dialect passed down through generations.',
      'In 2018 a Southern Resident orca named Tahlequah carried her dead calf for 17 days.',
      'Since 2020, a small group of Iberian orcas has been interacting with sailing boats and damaging their rudders.',
      'Its global population status is listed as Data Deficient by the IUCN because the species may comprise several distinct types.'
    ],
    wiki: 'Orca'
  },
  {
    id: 'bottlenose-dolphin',
    name: 'Common Bottlenose Dolphin',
    scientific: 'Tursiops truncatus',
    group: 'Marine Life',
    emoji: '🐬',
    status: 'LC',
    population: '~600,000',
    color: '#40c4ff',
    range: [
      [33, -70, 9, 12],
      [23, -85, 5, 10],
      [38, 5, 5, 16],
      [10, -18, 8, 4],
      [15, -112, 10, 12],
      [28, 135, 8, 12],
      [-33, 152, 5, 6],
      [-29, -45, 5, 6],
      [-34, 25, 3, 8],
      [52, -8, 5, 6]
    ],
    pastRange: null,
    extinct: null,
    hotspots: [
      { name: 'Chanonry Point, Moray Firth, Scotland', lat: 57.57, lng: -4.09 },
      { name: 'Sarasota Bay, Florida', lat: 27.33, lng: -82.58 },
      { name: 'Laguna, Santa Catarina, Brazil', lat: -28.48, lng: -48.78 },
      { name: 'New Quay, Cardigan Bay, Wales', lat: 52.22, lng: -4.36 }
    ],
    habitat: 'Warm and temperate coastal and offshore waters',
    diet: 'Carnivore – fish, squid, crustaceans',
    size: '2–4 m long, 150–650 kg',
    lifespan: '40–60 years',
    speed: '~35 km/h',
    summary: 'The familiar, highly intelligent dolphin of coasts worldwide, which calls its companions by individual whistle “names”.',
    facts: [
      'Each dolphin develops a unique signature whistle that others use to call it, much like a name.',
      'Dolphins sleep with one half of the brain at a time, keeping one eye open.',
      'In Laguna, Brazil, dolphins and fishers have cooperated for generations, with dolphins signalling when to cast nets.',
      'The Sarasota Dolphin Research Program, running since 1970, is the world’s longest study of a wild dolphin population.',
      'Bottlenose dolphins have recognised themselves in mirrors, a rare sign of self-awareness.',
      'They find prey using echolocation, emitting clicks and reading the returning echoes.'
    ],
    wiki: 'Common_bottlenose_dolphin'
  },
  {
    id: 'green-sea-turtle',
    name: 'Green Sea Turtle',
    scientific: 'Chelonia mydas',
    group: 'Marine Life',
    emoji: '🐢',
    status: 'EN',
    population: '~85,000–90,000 nesting females',
    color: '#2e7d32',
    range: [
      [20, -75, 7, 14],
      [-10, -32, 8, 7],
      [5, -15, 9, 9],
      [34, 32, 2, 4],
      [20, 48, 7, 11],
      [-15, 50, 9, 9],
      [5, 115, 8, 14],
      [-15, 145, 8, 12],
      [23, -162, 3, 6],
      [5, -92, 10, 12]
    ],
    pastRange: null,
    extinct: null,
    hotspots: [
      { name: 'Tortuguero National Park, Costa Rica', lat: 10.54, lng: -83.5 },
      { name: 'Raine Island, Great Barrier Reef', lat: -11.59, lng: 144.03 },
      { name: 'Ras al Jinz Turtle Reserve, Oman', lat: 22.42, lng: 59.83 },
      { name: 'Selingan, Turtle Islands Park, Sabah', lat: 6.17, lng: 118.06 }
    ],
    habitat: 'Tropical and subtropical seagrass beds, reefs, nesting beaches',
    diet: 'Herbivore as adult – seagrass and algae',
    size: '~1–1.2 m shell, 68–190 kg',
    lifespan: '70+ years',
    speed: '~2–3 km/h cruising, faster in bursts',
    summary: 'A large, ocean-wandering turtle that grazes seagrass meadows and returns across thousands of kilometres to nest on the beach where it hatched.',
    facts: [
      'It is named for the greenish colour of its fat and cartilage, not its shell.',
      'Adults are the only mostly herbivorous sea turtles, grazing seagrass like underwater lawnmowers.',
      'Nest temperature decides hatchling sex – warmer sand produces more females.',
      'A 2018 study found about 99% of young green turtles from northern Great Barrier Reef beaches are female, linked to warming sand.',
      'Turtles nesting on Ascension Island swim over 2,000 km from feeding grounds off Brazil.',
      'Females may take 20–40 years to reach breeding age.'
    ],
    wiki: 'Green_sea_turtle'
  },
  {
    id: 'saltwater-crocodile',
    name: 'Saltwater Crocodile',
    scientific: 'Crocodylus porosus',
    group: 'Reptiles',
    emoji: '🐊',
    status: 'LC',
    population: '~200,000–300,000',
    color: '#827717',
    range: [
      [-14, 133, 3, 10],
      [-18, 145.5, 4, 2],
      [-5, 141, 4, 8],
      [0, 112, 4, 8],
      [11, 123, 5, 3],
      [3, 102, 3, 4],
      [14, 94, 4, 3],
      [21, 88, 2, 4],
      [7.8, 80.7, 1.5, 1],
      [-9, 160, 3, 6]
    ],
    pastRange: null,
    extinct: null,
    hotspots: [
      { name: 'Adelaide River, Northern Territory', lat: -12.67, lng: 131.33 },
      { name: 'Yellow Water, Kakadu National Park', lat: -12.9, lng: 132.52 },
      { name: 'Bhitarkanika National Park, India', lat: 20.72, lng: 86.9 },
      { name: 'Daintree River, Queensland', lat: -16.3, lng: 145.43 }
    ],
    habitat: 'Estuaries, mangroves, rivers, coastal seas',
    diet: 'Carnivore – fish, birds, turtles, mammals, buffalo',
    size: 'Males 4.5–6 m, up to 1,000+ kg',
    lifespan: '70+ years',
    speed: '~25–29 km/h swimming bursts',
    summary: 'The largest living reptile, an ambush predator of Indo-Pacific estuaries that can cross open sea between islands.',
    facts: [
      'It has the strongest bite ever measured in a living animal, about 16,400 newtons.',
      'Tagged crocodiles have ridden ocean currents for hundreds of kilometres, one travelling 590 km in 25 days.',
      'Lolong, captured in the Philippines in 2011, measured 6.17 m – the largest crocodile ever measured in captivity.',
      'Glands on its tongue excrete excess salt, letting it live in seawater.',
      'After hunting was banned in 1971, the Northern Territory’s population grew from a few thousand to around 100,000.',
      'The temperature of the nest decides whether hatchlings are male or female.'
    ],
    wiki: 'Saltwater_crocodile'
  },
  {
    id: 'komodo-dragon',
    name: 'Komodo Dragon',
    scientific: 'Varanus komodoensis',
    group: 'Reptiles',
    emoji: '🦎',
    status: 'EN',
    population: '~3,500 (~1,400 adults)',
    color: '#ff9100',
    range: [
      [-8.55, 119.47, 0.15, 0.12],
      [-8.66, 119.71, 0.12, 0.08],
      [-8.55, 120.2, 0.25, 0.6]
    ],
    pastRange: null,
    extinct: null,
    hotspots: [
      { name: 'Loh Liang, Komodo Island', lat: -8.567, lng: 119.5 },
      { name: 'Loh Buaya, Rinca Island', lat: -8.656, lng: 119.72 },
      { name: 'Wae Wuul Nature Reserve, Flores', lat: -8.6, lng: 119.83 }
    ],
    habitat: 'Dry savanna, tropical forest, beaches of a few Indonesian islands',
    diet: 'Carnivore – deer, pigs, water buffalo, carrion',
    size: 'Up to ~3 m long, 70–90 kg (males)',
    lifespan: '~30 years',
    speed: '20 km/h',
    summary: 'The world’s largest lizard, a venomous giant found only on Komodo, Rinca, Flores and a few nearby islets of Indonesia.',
    facts: [
      'Komodo dragons have venom glands that secrete toxins which stop blood clotting and lower blood pressure.',
      'They can eat up to 80% of their body weight in a single meal.',
      'Females can reproduce without males through parthenogenesis, first documented in British zoos in 2006.',
      'Young dragons live in trees to avoid being eaten by adults.',
      'They were unknown to Western science until 1910 and formally described in 1912.',
      'In 2021 the IUCN upgraded them to Endangered because rising sea levels are expected to shrink their habitat.'
    ],
    wiki: 'Komodo_dragon'
  },
  {
    id: 'king-cobra',
    name: 'King Cobra',
    scientific: 'Ophiophagus hannah',
    group: 'Reptiles',
    emoji: '🐍',
    status: 'VU',
    population: 'Unknown',
    color: '#4a148c',
    range: [
      [12, 75.5, 4, 1.5],
      [26.5, 90, 2, 6],
      [19.5, 83.5, 2, 2],
      [17, 100, 7, 6],
      [24, 110, 3, 7],
      [2, 102, 4, 4],
      [1, 114, 3, 4],
      [-7.3, 109, 1, 4],
      [12, 122, 6, 3],
      [12, 92.8, 1, 0.4]
    ],
    pastRange: null,
    extinct: null,
    hotspots: [
      { name: 'Agumbe, Karnataka, India', lat: 13.51, lng: 75.09 },
      { name: 'Khao Yai National Park, Thailand', lat: 14.44, lng: 101.37 },
      { name: 'Danum Valley, Sabah, Malaysia', lat: 4.96, lng: 117.79 }
    ],
    habitat: 'Tropical rainforests, mangroves, bamboo thickets',
    diet: 'Carnivore – mostly other snakes, also monitor lizards',
    size: '3–4 m (record 5.85 m), up to ~9 kg',
    lifespan: '~20 years',
    speed: 'Not well measured',
    summary: 'The world’s longest venomous snake, a snake-eating hunter of South and Southeast Asian forests.',
    facts: [
      'Its genus name Ophiophagus means “snake-eater” – other snakes are its main prey.',
      'It is the only snake known to build a nest; the female heaps leaf litter over her eggs and guards them.',
      'When threatened it can rear up about a third of its body length and gives a deep, growl-like hiss.',
      'A 2024 study split the king cobra into four species, including Ophiophagus kaalinga of India’s Western Ghats.',
      'Drop for drop its venom is less potent than many cobras’, but a single bite delivers a very large dose.',
      'Agumbe Rainforest Research Station in India was founded to study king cobras in the wild.'
    ],
    wiki: 'King_cobra'
  },
  {
    id: 'monarch-butterfly',
    name: 'Monarch Butterfly',
    scientific: 'Danaus plexippus',
    group: 'Insects',
    emoji: '🦋',
    status: 'VU',
    population: 'Tens to hundreds of millions',
    color: '#ffab00',
    range: [
      [42, -85, 8, 15],
      [32, -98, 5, 4],
      [19.6, -100.25, 0.4, 0.4],
      [42, -117, 6, 6],
      [35.5, -120.3, 3, 1.3],
      [24, -79, 3, 5],
      [5, -70, 12, 14],
      [-30, 148, 8, 5],
      [20.5, -157, 1, 2],
      [28.3, -16, 1, 2]
    ],
    pastRange: null,
    extinct: null,
    hotspots: [
      { name: 'El Rosario Monarch Sanctuary, Mexico', lat: 19.59, lng: -100.27 },
      { name: 'Sierra Chincua Sanctuary, Mexico', lat: 19.67, lng: -100.29 },
      { name: 'Monarch Grove Sanctuary, Pacific Grove, California', lat: 36.62, lng: -121.92 },
      { name: 'Point Pelee National Park, Canada', lat: 41.96, lng: -82.52 }
    ],
    habitat: 'Meadows, fields and gardens with milkweed; fir forests in winter',
    diet: 'Larvae: milkweed leaves; adults: flower nectar',
    size: 'Wingspan 8.9–10.2 cm, ~0.5 g',
    lifespan: '2–5 weeks; migratory generation up to ~8 months',
    speed: '~9 km/h cruising, faster with tailwinds',
    summary: 'An orange-and-black butterfly whose eastern North American population makes an astonishing multi-generation migration of up to ~4,800 km to fir forests in central Mexico.',
    facts: [
      'Eastern monarchs fly up to about 4,800 km to overwinter in oyamel fir forests at around 3,000 m in the mountains of central Mexico.',
      'The journey north takes several generations, but a single long-lived “super generation” makes the entire trip south.',
      'They navigate with a sun compass corrected by a circadian clock partly located in their antennae.',
      'Caterpillars feed only on milkweed and store its toxins, making both larvae and adults poisonous to many predators.',
      'The Mexican overwintering sites were only revealed to science in 1975, after a decades-long search organised by Fred Urquhart.',
      'The migratory subspecies was listed as Endangered by the IUCN in 2022, then reassessed as Vulnerable in 2023; western monarchs fell below 2,000 at California sites in 2020.'
    ],
    wiki: 'Monarch_butterfly'
  },
  {
    id: 'dodo',
    name: 'Dodo',
    scientific: 'Raphus cucullatus',
    group: 'Extinct',
    emoji: '🦤',
    status: 'EX',
    population: 'Extinct',
    color: '#ba68c8',
    range: [
      [-20.28, 57.57, 0.22, 0.2],
      [-20.42, 57.62, 0.1, 0.12]
    ],
    pastRange: null,
    extinct: 1662,
    hotspots: [
      { name: 'Mare aux Songes fossil site, Mauritius', lat: -20.44, lng: 57.69 },
      { name: 'Natural History Museum, Port Louis, Mauritius', lat: -20.162, lng: 57.502 },
      { name: 'Oxford University Museum of Natural History, UK', lat: 51.758, lng: -1.256 }
    ],
    habitat: 'Coastal and dry lowland forests of Mauritius',
    diet: 'Herbivore – fruit, seeds, nuts (probably)',
    size: '~1 m tall, ~10–18 kg',
    lifespan: 'Unknown',
    speed: 'Unknown (flightless)',
    summary: 'A flightless giant pigeon of Mauritius that vanished within a century of human arrival, becoming the global symbol of extinction.',
    facts: [
      'Its closest living relative is the Nicobar pigeon of Southeast Asia.',
      'Dutch sailors first recorded the dodo in 1598; the last widely accepted sighting was in 1662, though statistical studies suggest it may have lingered until about 1690.',
      'Introduced pigs, rats, monkeys and dogs that ate eggs and competed for food likely did more damage than hunting by sailors.',
      'The only surviving soft tissue – a head and foot – is held at the Oxford University Museum of Natural History.',
      'Many old paintings show overfed captive birds; skeletons suggest wild dodos were slimmer.',
      'Its fame owes much to its appearance in Lewis Carroll’s Alice’s Adventures in Wonderland (1865).'
    ],
    wiki: 'Dodo'
  },
  {
    id: 'woolly-mammoth',
    name: 'Woolly Mammoth',
    scientific: 'Mammuthus primigenius',
    group: 'Extinct',
    emoji: '🦣',
    status: 'EX',
    population: 'Extinct',
    color: '#b388ff',
    range: [
      [52, 10, 6, 15],
      [58, 50, 8, 20],
      [62, 100, 10, 30],
      [66, 150, 8, 20],
      [66, -155, 6, 15],
      [62, -125, 6, 15],
      [45, -88, 5, 12],
      [71.2, -178.5, 0.4, 1.4],
      [57.18, -170.27, 0.15, 0.3]
    ],
    pastRange: null,
    extinct: -2000,
    hotspots: [
      { name: 'Wrangel Island, Russia', lat: 71.25, lng: -179.5 },
      { name: 'Mammoth Museum, Yakutsk, Russia', lat: 62.03, lng: 129.73 },
      { name: 'The Mammoth Site, Hot Springs, South Dakota', lat: 43.42, lng: -103.48 }
    ],
    habitat: 'Ice Age “mammoth steppe” grasslands and tundra',
    diet: 'Herbivore – grasses, sedges, herbs, shrubs',
    size: '2.7–3.4 m at the shoulder, up to ~6 tonnes',
    lifespan: '~60 years',
    speed: 'Unknown',
    summary: 'A shaggy, cold-adapted elephant that roamed the frozen steppes of Eurasia and North America during the last Ice Age.',
    facts: [
      'Most mammoths died out around 10,000 years ago, but an isolated population survived on Wrangel Island until about 4,000 years ago – after the Great Pyramid of Giza was built.',
      'Another island population on St. Paul Island, Alaska, lasted until about 5,600 years ago.',
      'Its outer hair grew up to about 90 cm long over a dense woolly undercoat, and small ears and a short tail reduced heat loss.',
      'Tusks could reach over 4 m along their curve.',
      'Frozen carcasses such as baby Lyuba, found in Siberia in 2007, preserve skin, organs and even milk in the stomach.',
      'Its closest living relative is the Asian elephant.'
    ],
    wiki: 'Woolly_mammoth'
  },
  {
    id: 'thylacine',
    name: 'Thylacine (Tasmanian Tiger)',
    scientific: 'Thylacinus cynocephalus',
    group: 'Extinct',
    emoji: '🐾',
    status: 'EX',
    population: 'Extinct',
    color: '#f4511e',
    range: [
      [-42, 146.6, 1.3, 1.4],
      [-41.4, 145.3, 0.6, 0.8]
    ],
    pastRange: {
      until: -1000,
      zones: [
        [-25, 135, 9, 14],
        [-35, 145, 4, 6],
        [-5, 141, 3, 6]
      ]
    },
    extinct: 1936,
    hotspots: [
      { name: 'Tasmanian Museum and Art Gallery, Hobart', lat: -42.881, lng: 147.333 },
      { name: 'Florentine Valley, Tasmania', lat: -42.6, lng: 146.45 },
      { name: 'Australian Museum, Sydney', lat: -33.874, lng: 151.213 }
    ],
    habitat: 'Dry eucalyptus forests, wetlands, grasslands',
    diet: 'Carnivore – wallabies, possums, birds',
    size: '~1–1.3 m body plus 50–65 cm tail, 15–30 kg',
    lifespan: '5–7 years in the wild (up to 9 in captivity)',
    speed: 'Unknown',
    summary: 'A striped, dog-like marsupial predator that survived only in Tasmania in historic times and was hunted to extinction in the 20th century.',
    facts: [
      'It was the largest carnivorous marsupial of modern times and a classic case of convergent evolution with dogs and wolves.',
      'Both sexes had a pouch; the male’s protected its external genitals.',
      'Tasmania paid government bounties on 2,184 thylacines between 1888 and 1909.',
      'The last known thylacine died in Hobart’s Beaumaris Zoo on 7 September 1936, just 59 days after the species was given legal protection.',
      'It disappeared from mainland Australia and New Guinea thousands of years earlier, possibly due to humans, climate change and the arrival of dingoes.',
      'In 2023 scientists recovered RNA from a museum thylacine specimen over 130 years old.'
    ],
    wiki: 'Thylacine'
  }
];

// Guided learning tours for Earth Explorer.
// Each step flies the camera to lat/lng/alt (alt in globe radii), optionally sets the time machine
// (`year`), opens a card (`open`: 'w:' wonder | 'l:' landmark | 'a:' animal | 'e:' event index in
// EVENTS | 'c:' ISO3 country), shows animal ranges / resource sites, and may ask a quick question.
// Event ids ('ev' + index) follow the order of the EVENTS array in timeline.js — if that array is
// reordered, update the 'e:' references here.

export const TOURS = [
  // ───────────────────────────── 1. Seven Wonders of the Ancient World ─────────────────────────────
  {
    id: "ancient-wonders",
    title: "The Seven Wonders of the Ancient World",
    subtitle: "Travel back to 240 BC, the only moment when all seven wonders stood at once, and see what became of each.",
    theme: "History",
    level: "Beginner",
    minutes: 9,
    cover: "Seven_Wonders_of_the_Ancient_World",
    steps: [
      {
        title: "A Greek travel bucket list",
        text: "The Seven Wonders were a 'must-see' list compiled by Greek writers such as Antipater of Sidon in the 2nd century BC. Every wonder sits around the eastern Mediterranean, the world the Greeks knew after Alexander the Great's conquests. The time machine is set to 240 BC: the Lighthouse of Alexandria had just been finished and the Colossus of Rhodes had not yet fallen, so for roughly twenty years all seven stood at the same time.",
        lat: 33.5, lng: 30.0, alt: 1.3,
        year: -240,
        wiki: "Seven_Wonders_of_the_Ancient_World",
        question: { q: "Why are all seven wonders found around the eastern Mediterranean?", options: ["It was the only region with stone", "The list was written by Greeks describing the world they knew", "The Romans chose them", "They were all built by one king"], answer: 1, explain: "Greek writers listed the marvels of their own known world, the lands around the Mediterranean and Near East." }
      },
      {
        title: "The Great Pyramid of Giza",
        text: "The oldest wonder was already about 2,300 years old when the list was written. It was built as the tomb of Pharaoh Khufu around 2560 BC from an estimated 2.3 million stone blocks, and it originally rose about 146.6 m. For nearly 4,000 years it was the tallest human-made structure on Earth, until Lincoln Cathedral's spire overtook it around 1311. It is the only one of the seven still standing.",
        lat: 29.9792, lng: 31.1342, alt: 0.12,
        year: -2560,
        open: "l:great-pyramid-of-giza",
        wiki: "Great_Pyramid_of_Giza"
      },
      {
        title: "The Hanging Gardens of Babylon",
        text: "Greek and Roman writers described terraced gardens rising like a green mountain in Babylon, traditionally built by King Nebuchadnezzar II (reigned 605–562 BC) for his homesick wife Amytis. Yet no Babylonian text mentions them, and archaeologists have never found a definite trace. Historian Stephanie Dalley has argued they may really have stood in Nineveh, built by the Assyrian king Sennacherib. They are the most mysterious wonder of all.",
        lat: 32.5422, lng: 44.4275, alt: 0.15,
        year: -580,
        open: "l:hanging-gardens-of-babylon",
        wiki: "Hanging_Gardens_of_Babylon",
        question: { q: "What makes the Hanging Gardens unique among the seven wonders?", options: ["They were the tallest", "They are still standing", "Nobody is sure they ever existed", "They were made of gold"], answer: 2, explain: "No Babylonian record or archaeological evidence confirms the gardens, so their existence is still debated." }
      },
      {
        title: "The Temple of Artemis at Ephesus",
        text: "This huge marble temple to the goddess Artemis stood near today's Selçuk in Türkiye. The first great temple, partly paid for by the wealthy King Croesus of Lydia, was completed around 550 BC. In 356 BC a man named Herostratus burned it down simply to make his name famous. The Ephesians rebuilt it even larger, and it was finally destroyed in AD 401; today a single re-erected column marks the spot.",
        lat: 37.9497, lng: 27.3639, alt: 0.12,
        year: -300,
        open: "l:temple-of-artemis",
        wiki: "Temple_of_Artemis"
      },
      {
        title: "The Statue of Zeus at Olympia",
        text: "Around 435 BC the sculptor Phidias made a seated Zeus about 12 m tall for the god's temple at Olympia, home of the Olympic Games. The statue was 'chryselephantine': plates of ivory for skin and panels of gold for robes, laid over a wooden frame. Ancient writers joked that if Zeus stood up he would lift the roof off. It was lost in the 5th century AD, possibly in a fire in Constantinople.",
        lat: 37.6379, lng: 21.63, alt: 0.12,
        year: -400,
        open: "l:statue-of-zeus-at-olympia",
        wiki: "Statue_of_Zeus_at_Olympia"
      },
      {
        title: "The Mausoleum at Halicarnassus",
        text: "In today's Bodrum, Türkiye, a grand tomb about 45 m high was built around 353–350 BC for Mausolus, a ruler of Caria under the Persian Empire, and his wife Artemisia II. Four of the most famous Greek sculptors each decorated one side. It was so admired that his name became our word 'mausoleum'. Earthquakes toppled it between the 12th and 15th centuries, and crusader knights reused its stones for Bodrum Castle by 1522.",
        lat: 37.0379, lng: 27.4241, alt: 0.12,
        year: -300,
        open: "l:mausoleum-at-halicarnassus",
        wiki: "Mausoleum_at_Halicarnassus"
      },
      {
        title: "The Colossus of Rhodes",
        text: "The people of Rhodes celebrated surviving a year-long siege in 305 BC by building a bronze statue of the sun god Helios, about 33 m tall. It took around 12 years and was finished in 280 BC. Just 54 years later, in 226 BC, an earthquake snapped it at the knees. Famous pictures of it straddling the harbour entrance are a later myth; it almost certainly stood on land.",
        lat: 36.451, lng: 28.2258, alt: 0.12,
        year: -250,
        open: "l:colossus-of-rhodes",
        wiki: "Colossus_of_Rhodes",
        question: { q: "Which wonder stood for the shortest time?", options: ["The Lighthouse of Alexandria", "The Colossus of Rhodes", "The Great Pyramid", "The Mausoleum at Halicarnassus"], answer: 1, explain: "The Colossus stood only from 280 to 226 BC before an earthquake toppled it." }
      },
      {
        title: "The Lighthouse of Alexandria",
        text: "On the island of Pharos, guarding the harbour of Alexandria, the Ptolemaic kings built a tower roughly 100 m tall between about 280 and 247 BC. A fire at the top guided ships into one of the busiest ports of the ancient world. 'Pharos' became the word for lighthouse in several languages, such as French 'phare' and Italian 'faro'. Earthquakes left it a ruin by 1323, and in 1477 Sultan Qaitbay built a fortress on the site, partly from its stones.",
        lat: 31.214, lng: 29.8856, alt: 0.12,
        year: -200,
        open: "l:lighthouse-of-alexandria",
        wiki: "Lighthouse_of_Alexandria"
      },
      {
        title: "What survives today",
        text: "Jump to the present day and six of the seven wonders have vanished, destroyed by earthquakes, fire and people reusing their stone. Only the Great Pyramid, the oldest of them all, still stands at Giza. Fragments of the others survive in museums such as the British Museum in London. Try moving the time slider yourself and watch the wonders appear and disappear.",
        lat: 33.5, lng: 30.0, alt: 1.3,
        year: 2026,
        wiki: "Seven_Wonders_of_the_Ancient_World",
        question: { q: "Which ancient wonder is the only one still standing?", options: ["The Colossus of Rhodes", "The Temple of Artemis", "The Great Pyramid of Giza", "The Lighthouse of Alexandria"], answer: 2, explain: "The Great Pyramid, built around 2560 BC, is the oldest wonder and the only survivor." }
      }
    ]
  },

  // ───────────────────────────── 2. New Seven Wonders ─────────────────────────────
  {
    id: "new-seven-wonders",
    title: "The New Seven Wonders of the World",
    subtitle: "Seven monuments chosen in a worldwide vote in 2007, from the Great Wall to Christ the Redeemer.",
    theme: "History",
    level: "Beginner",
    minutes: 8,
    cover: "New_7_Wonders_of_the_World",
    steps: [
      {
        title: "A modern list chosen by vote",
        text: "In 2007 the Swiss-based New7Wonders Foundation ran a global public vote to pick seven new wonders. The organisers said over 100 million votes were cast online and by phone, and the winners were announced in Lisbon on 7 July 2007 (7/7/07). Unlike the ancient list, the new wonders are spread across four continents. The Great Pyramid of Giza, the last ancient wonder, was given honorary status.",
        lat: 20.0, lng: 10.0, alt: 2.5,
        year: 2026,
        wiki: "New_7_Wonders_of_the_World"
      },
      {
        title: "The Great Wall of China",
        text: "Chinese states built walls from the 7th century BC, and after unifying China in 221 BC the first emperor, Qin Shi Huang, linked many of them. Most of the brick-and-stone wall visitors walk on today, like this section near Beijing, was built by the Ming dynasty (1368–1644). A 2012 Chinese survey counted 21,196 km of walls and branches. Despite the popular myth, it is not visible to the naked eye from the Moon.",
        lat: 40.4319, lng: 116.5704, alt: 0.15,
        open: "l:great-wall-of-china",
        wiki: "Great_Wall_of_China",
        question: { q: "Which dynasty built most of the Great Wall that tourists see today?", options: ["Qin", "Han", "Ming", "Tang"], answer: 2, explain: "The best-preserved brick and stone sections were built under the Ming dynasty (1368–1644)." }
      },
      {
        title: "Petra, Jordan",
        text: "Petra was the capital of the Nabataeans, Arab traders who grew rich controlling caravan routes for incense and spices. Visitors enter through the Siq, a narrow gorge about 1.2 km long, to face Al-Khazneh, the 'Treasury', carved straight into the rose-red cliff in the 1st century AD. The Nabataeans also built clever channels and cisterns to store scarce desert rain. Swiss explorer Johann Ludwig Burckhardt brought Petra to Western attention in 1812.",
        lat: 30.3285, lng: 35.4444, alt: 0.12,
        open: "l:petra",
        wiki: "Petra"
      },
      {
        title: "The Colosseum, Rome",
        text: "Emperor Vespasian began this giant amphitheatre around AD 72 and his son Titus opened it in AD 80. It could hold an estimated 50,000 to 80,000 spectators, who watched gladiator fights, animal hunts and public executions. Its original name was the Flavian Amphitheatre, after the family of emperors who built it. An earthquake in 1349 brought down its southern side, and much of its stone was later reused in other buildings.",
        lat: 41.8902, lng: 12.4922, alt: 0.1,
        open: "l:colosseum",
        wiki: "Colosseum"
      },
      {
        title: "Chichen Itza, Mexico",
        text: "Chichen Itza was one of the largest Maya cities, flourishing from about AD 600 to 1200 on Mexico's Yucatán Peninsula. Its pyramid, El Castillo (the Temple of Kukulcán), has 91 steps on each of its four sides; adding the top platform makes 365, one for each day of the year. At the spring and autumn equinoxes, the late-afternoon sun casts triangles of shadow that look like a serpent sliding down the staircase.",
        lat: 20.6843, lng: -88.5678, alt: 0.12,
        open: "l:chichen-itza",
        wiki: "Chichen_Itza",
        question: { q: "Why does El Castillo have 365 steps in total?", options: ["One for each Maya king", "One for each day of the year", "It was a coincidence", "One for each warrior"], answer: 1, explain: "Four stairways of 91 steps plus the top platform give 365, matching the days of the solar year." }
      },
      {
        title: "Machu Picchu, Peru",
        text: "High in the Andes at about 2,430 m, Machu Picchu was probably built around 1450 as a royal estate for the Inca emperor Pachacuti. Its walls are made of finely cut stones fitted together without mortar, a technique that helps them survive earthquakes. It was abandoned around the time of the Spanish conquest and never found by the conquistadors. American explorer Hiram Bingham brought it to international attention in 1911.",
        lat: -13.1631, lng: -72.545, alt: 0.12,
        open: "l:machu-picchu",
        wiki: "Machu_Picchu"
      },
      {
        title: "Christ the Redeemer, Rio de Janeiro",
        text: "The youngest of the new wonders was inaugurated on 12 October 1931 on top of Corcovado mountain, about 700 m above Rio de Janeiro. The statue is 30 m tall on an 8 m pedestal, and its arms stretch 28 m. It is built of reinforced concrete covered with a mosaic of thousands of soapstone tiles. French sculptor Paul Landowski designed it, and the Romanian sculptor Gheorghe Leonida made the face.",
        lat: -22.9519, lng: -43.2105, alt: 0.12,
        open: "l:christ-the-redeemer",
        wiki: "Christ_the_Redeemer_(statue)"
      },
      {
        title: "The Taj Mahal, India",
        text: "When his wife Mumtaz Mahal died in childbirth in 1631, the Mughal emperor Shah Jahan ordered a tomb for her in Agra beside the Yamuna River. The white marble mausoleum was finished around 1648 and the whole complex, with gardens, mosque and guest house, around 1653. About 20,000 artisans worked on it, inlaying the marble with semi-precious stones. It is often called the finest example of Mughal architecture.",
        lat: 27.1751, lng: 78.0421, alt: 0.1,
        open: "l:taj-mahal",
        wiki: "Taj_Mahal",
        question: { q: "Who is the Taj Mahal a tomb for?", options: ["Emperor Akbar", "Mumtaz Mahal", "Shah Jahan's mother", "A Persian poet"], answer: 1, explain: "Shah Jahan built it for his wife Mumtaz Mahal, who died in 1631; he was later buried beside her." }
      }
    ]
  },

  // ───────────────────────────── 3. Cradles of civilisation ─────────────────────────────
  {
    id: "cradles-of-civilisation",
    title: "Cradles of Civilisation",
    subtitle: "Visit the six places where people independently invented farming, cities and states.",
    theme: "History",
    level: "Intermediate",
    minutes: 10,
    cover: "Cradle_of_civilization",
    steps: [
      {
        title: "The Fertile Crescent: the first farmers",
        text: "Around 9500–8500 BC, people in an arc of land from the Levant to the Zagros Mountains began planting wheat, barley and lentils and herding sheep and goats. Farming produced surplus food, which let villages grow into towns and some people specialise as builders, priests or traders. This 'Neolithic Revolution' happened independently in several regions, and those regions became the cradles of civilisation.",
        lat: 34.5, lng: 41.0, alt: 1.0,
        year: -8500,
        open: "e:ev11",
        wiki: "Fertile_Crescent"
      },
      {
        title: "Mesopotamia: Uruk and the invention of writing",
        text: "Between the Tigris and Euphrates rivers, the Sumerian city of Uruk grew into perhaps the largest city on Earth, home to tens of thousands of people. Around 3300–3200 BC its scribes began pressing wedge-shaped marks into clay tablets to record grain, beer and livestock: cuneiform, one of the world's first writing systems. Uruk was later remembered as the city of the legendary king Gilgamesh.",
        lat: 31.322, lng: 45.636, alt: 0.4,
        year: -3300,
        open: "e:ev19",
        wiki: "Uruk",
        question: { q: "What were the earliest cuneiform tablets mostly used for?", options: ["Love poems", "Keeping accounts of goods", "Maps", "Prayers"], answer: 1, explain: "The first tablets were mostly bookkeeping records of grain, livestock and other goods." }
      },
      {
        title: "The Nile: a kingdom built on a flood",
        text: "Every summer the Nile flooded and left behind a layer of rich black silt, letting Egyptians grow crops in the middle of a desert. Around 3100 BC Upper and Lower Egypt were united under the first pharaohs, with Memphis as their capital. Hieroglyphic writing appeared at about the same time, and watching the yearly flood helped Egyptians develop a 365-day calendar. Within five centuries they were building the pyramids.",
        lat: 29.845, lng: 31.25, alt: 0.6,
        year: -3100,
        open: "e:ev21",
        wiki: "Early_Dynastic_Period_of_Egypt"
      },
      {
        title: "The Indus Valley: planned cities",
        text: "In today's Pakistan and north-west India, the Indus (or Harappan) civilisation flourished from about 2600 to 1900 BC and covered a larger area than Egypt and Mesopotamia combined. Cities such as Mohenjo-daro had streets laid out in a grid, brick houses, covered drains and a 'Great Bath'. Its people used standard weights and carved seals, but their script has never been deciphered.",
        lat: 27.3294, lng: 68.1389, alt: 0.3,
        year: -2500,
        open: "l:mohenjo-daro",
        wiki: "Indus_Valley_Civilisation",
        question: { q: "What is still unknown about the Indus civilisation?", options: ["Where its cities were", "How to read its script", "What crops it grew", "Whether it had drains"], answer: 1, explain: "Hundreds of Indus inscriptions survive, but the script remains undeciphered." }
      },
      {
        title: "The Yellow River: the Shang dynasty",
        text: "Farmers in China's Yellow River valley grew millet from around 7000 BC, while rice was domesticated along the Yangtze to the south. Around 1250 BC the Shang kings ruled from Yin, near today's Anyang. Their priests carved questions to the ancestors on turtle shells and ox bones, then cracked them with heat to read the answers. These 'oracle bones' are the oldest known Chinese writing, and the Shang were also master bronze casters.",
        lat: 36.13, lng: 114.31, alt: 0.4,
        year: -1250,
        open: "e:ev29",
        wiki: "Oracle_bone"
      },
      {
        title: "Mesoamerica: the Olmec",
        text: "In the Americas, people in southern Mexico domesticated maize from a wild grass called teosinte about 9,000 years ago. On the Gulf Coast the Olmec built the first great centre of Mesoamerica at San Lorenzo, at its height around 1200–900 BC. They carved colossal stone heads up to about 3.4 m tall, probably portraits of rulers; 17 are known today. Later cultures such as the Maya inherited Olmec ideas like the ballgame and ceremonial centres.",
        lat: 17.75, lng: -94.76, alt: 0.4,
        year: -1200,
        open: "e:ev30",
        wiki: "Olmecs"
      },
      {
        title: "The Andes: Caral",
        text: "In Peru's dry Supe Valley, people built Caral around 2600 BC, about the same time as the Egyptian pyramids, making it one of the oldest cities in the Americas. It had six large platform mounds, sunken circular plazas and houses for thousands of people. Its builders did not yet use pottery, but grew cotton for fishing nets and traded with fishing villages on the coast. Later Andean civilisations, including the Inca, built on this long tradition.",
        lat: -10.893, lng: -77.52, alt: 0.4,
        year: -2600,
        open: "e:ev23",
        wiki: "Caral"
      },
      {
        title: "Six cradles, one pattern",
        text: "From the globe you can see that these six cradles grew up far apart, along rivers or fertile valleys, and mostly without contact with each other. Each one independently developed farming, cities, social classes and monumental buildings. Writing was invented separately at least in Mesopotamia, China and Mesoamerica, and probably in Egypt too. Drag the time slider between 3500 BC and 1000 BC to watch the first cities spread.",
        lat: 20.0, lng: 30.0, alt: 2.6,
        year: -2500,
        wiki: "Cradle_of_civilization",
        question: { q: "What did most early cradles of civilisation have in common?", options: ["They were all on islands", "They traded with each other constantly", "They developed near rivers or fertile valleys", "They all used the same script"], answer: 2, explain: "Reliable water and fertile soil supported farming surpluses, which allowed cities to grow." }
      }
    ]
  },

  // ───────────────────────────── 4. Rise and fall of Rome ─────────────────────────────
  {
    id: "roman-empire",
    title: "The Rise and Fall of the Roman Empire",
    subtitle: "Watch Rome grow from a village on seven hills into an empire around the Mediterranean, then split and fall.",
    theme: "History",
    level: "Intermediate",
    minutes: 10,
    cover: "Roman_Empire",
    steps: [
      {
        title: "753 BC: the founding of Rome",
        text: "According to legend, the twins Romulus and Remus were raised by a she-wolf, and Romulus founded Rome on the Palatine Hill on 21 April 753 BC. In reality, Rome grew from small hilltop villages beside a crossing of the River Tiber. It was ruled by kings until about 509 BC, when the Romans set up a republic governed by elected magistrates and the Senate.",
        lat: 41.893, lng: 12.483, alt: 0.3,
        year: -753,
        open: "e:ev33",
        wiki: "Founding_of_Rome"
      },
      {
        title: "146 BC: Rome defeats Carthage",
        text: "Rome's great rival was Carthage, a trading power in today's Tunisia. Over three Punic Wars (264–146 BC) the two fought for control of the western Mediterranean; in 218 BC the Carthaginian general Hannibal even marched war elephants over the Alps into Italy. Rome eventually won, and in 146 BC its army destroyed Carthage completely. The same year Rome sacked Corinth, tightening its grip on Greece.",
        lat: 36.853, lng: 10.323, alt: 0.4,
        year: -146,
        open: "e:ev40",
        wiki: "Punic_Wars",
        question: { q: "Which general crossed the Alps with elephants to attack Rome?", options: ["Julius Caesar", "Hannibal", "Alexander the Great", "Spartacus"], answer: 1, explain: "The Carthaginian general Hannibal crossed the Alps in 218 BC during the Second Punic War." }
      },
      {
        title: "AD 1: the age of Augustus",
        text: "After decades of civil war, Julius Caesar's adopted son Octavian became sole ruler and in 27 BC took the title Augustus: the Roman Republic had become an empire. By AD 1 Rome controlled every coast of the Mediterranean, which Romans called 'Mare Nostrum', 'our sea'. With the historical borders switched on you can see its territory around the sea. The empire's population is estimated at around 45–60 million, perhaps a fifth of all people alive.",
        lat: 40.0, lng: 15.0, alt: 1.5,
        year: 1,
        wiki: "Augustus"
      },
      {
        title: "AD 79: Vesuvius buries Pompeii",
        text: "On a day in AD 79, Mount Vesuvius erupted and buried the Roman towns of Pompeii and Herculaneum under several metres of ash and pumice. The writer Pliny the Younger watched from across the Bay of Naples and described the eruption in letters; his uncle Pliny the Elder died trying to rescue people. Because the towns were sealed so quickly, excavations since 1748 have revealed everyday Roman life in astonishing detail.",
        lat: 40.751, lng: 14.4869, alt: 0.15,
        year: 79,
        open: "e:ev42",
        wiki: "Pompeii"
      },
      {
        title: "AD 117: the empire at its largest",
        text: "When Emperor Trajan died in AD 117, the Roman Empire reached its greatest size, about 5 million km², stretching from Britain to Mesopotamia and from the Rhine to Egypt. Trajan had just added Dacia (in today's Romania) and briefly Mesopotamia. More than 80,000 km of paved roads linked the provinces. Compare the borders with the year AD 1 to see how far it grew.",
        lat: 40.0, lng: 22.0, alt: 1.8,
        year: 117,
        wiki: "Trajan",
        question: { q: "Under which emperor did the Roman Empire reach its largest size?", options: ["Augustus", "Nero", "Trajan", "Constantine"], answer: 2, explain: "The empire was largest at Trajan's death in AD 117." }
      },
      {
        title: "Hadrian's Wall: drawing a frontier",
        text: "Trajan's successor Hadrian decided to stop expanding and defend the borders instead. From AD 122 his soldiers built a stone wall about 117 km long across northern Britain, from the River Tyne to the Solway Firth, with forts every few kilometres. It marked the north-west edge of the empire for nearly three centuries. Today it is part of the UNESCO 'Frontiers of the Roman Empire' World Heritage Site.",
        lat: 55.013, lng: -2.331, alt: 0.25,
        year: 122,
        wiki: "Hadrian's_Wall"
      },
      {
        title: "A new capital in the east",
        text: "In AD 330 Emperor Constantine, the first Christian emperor, moved the capital to a new city on the Bosporus: Constantinople, today's Istanbul. The empire had become too big to rule from one place. When Emperor Theodosius I died in 395, it was divided for good between his two sons, a Western Empire ruled from Italy and an Eastern Empire ruled from Constantinople.",
        lat: 41.008, lng: 28.98, alt: 0.4,
        year: 400,
        wiki: "Constantinople"
      },
      {
        title: "AD 476: the fall of the West",
        text: "The Western Empire was weakened by civil wars, economic crisis and pressure from Germanic peoples. The Visigoths sacked Rome in 410 and the Vandals in 455. In 476 the Germanic general Odoacer deposed the last western emperor, the teenage Romulus Augustulus, at Ravenna and did not appoint a replacement. Historians traditionally date the end of ancient Rome, and the start of the Middle Ages, to this moment.",
        lat: 44.418, lng: 12.2, alt: 0.4,
        year: 476,
        open: "e:ev45",
        wiki: "Fall_of_the_Western_Roman_Empire",
        question: { q: "Who deposed the last Western Roman emperor in 476?", options: ["Attila the Hun", "Odoacer", "Alaric", "Charlemagne"], answer: 1, explain: "Odoacer removed Romulus Augustulus in 476 and ruled Italy as king." }
      },
      {
        title: "1453: the end of the Eastern Roman Empire",
        text: "The Eastern Roman Empire, which historians call the Byzantine Empire, survived the West by almost a thousand years. Its emperor Justinian built the great church of Hagia Sophia, completed in 537. On 29 May 1453 the Ottoman sultan Mehmed II captured Constantinople with the help of giant cannons, ending more than 1,100 years of the city as a Roman capital. Roman law, roads, Latin-based languages and architecture still shape the world today.",
        lat: 41.0086, lng: 28.9802, alt: 0.15,
        year: 1453,
        open: "e:ev63",
        wiki: "Fall_of_Constantinople"
      }
    ]
  },

  // ───────────────────────────── 5. The Silk Road ─────────────────────────────
  {
    id: "silk-road",
    title: "Along the Silk Road",
    subtitle: "Follow silk, paper, ideas and plague along the trade routes that linked China to Europe.",
    theme: "History",
    level: "Intermediate",
    minutes: 10,
    cover: "Silk_Road",
    steps: [
      {
        title: "Chang'an: where the road began",
        text: "Chang'an, today's Xi'an, was the capital of the Han dynasty and the eastern starting point of the Silk Road. In 138 BC Emperor Wu sent the envoy Zhang Qian west to find allies; he was captured by the Xiongnu nomads and held for about ten years, but returned with reports of rich kingdoms in Central Asia. Han China then opened routes west to trade silk for horses, glass and silver. The name 'Silk Road' was only coined in 1877 by the German geographer Ferdinand von Richthofen.",
        lat: 34.26, lng: 108.94, alt: 0.4,
        year: -130,
        wiki: "Zhang_Qian"
      },
      {
        title: "Dunhuang: gateway to the desert",
        text: "At the oasis of Dunhuang, caravans left China proper and faced the deserts of the Tarim Basin. Nearby, Buddhist monks and patrons carved the Mogao Caves, around 500 temples decorated with murals over a thousand years. In 1900 a sealed 'Library Cave' was found holding tens of thousands of manuscripts, including the Diamond Sutra of AD 868, the oldest known dated printed book.",
        lat: 40.14, lng: 94.66, alt: 0.4,
        year: -130,
        open: "e:ev41",
        wiki: "Mogao_Caves",
        question: { q: "What famous object was found in Dunhuang's Library Cave?", options: ["The first compass", "The oldest dated printed book", "A Roman coin hoard", "Genghis Khan's tomb"], answer: 1, explain: "The Diamond Sutra, dated AD 868, is the oldest known printed book that carries a date." }
      },
      {
        title: "Kashgar: around the Taklamakan",
        text: "The Taklamakan Desert was too dangerous to cross directly, so caravans split onto a northern route and a southern route along a chain of oases at the foot of the mountains. The two routes met again at Kashgar, before climbing over the high passes of the Pamirs. Goods were rarely carried end to end: they passed from merchant to merchant, rising in price at every market. Bactrian camels, able to survive cold and drought, carried much of the load.",
        lat: 39.47, lng: 75.99, alt: 0.6,
        year: 1279,
        wiki: "Kashgar"
      },
      {
        title: "The Mongol peace",
        text: "In 1206 Genghis Khan united the Mongol tribes. His armies and those of his successors conquered most of Asia, creating the largest contiguous land empire in history. Once the conquests were over, the 'Pax Mongolica' made the Silk Road safer than ever: a relay network of horse stations carried messages, and merchants travelled with official passes called paiza. Around 1279, as shown on the map, the empire stretched from Korea to Eastern Europe.",
        lat: 47.2, lng: 102.8, alt: 1.6,
        year: 1279,
        open: "e:ev54",
        wiki: "Pax_Mongolica",
        question: { q: "Why did trade along the Silk Road grow under the Mongols?", options: ["They banned all wars", "One empire controlled most of the route and protected travellers", "They built a railway", "They invented the camel saddle"], answer: 1, explain: "With most of the route under one power, merchants faced fewer borders and better security." }
      },
      {
        title: "Samarkand: city of merchants",
        text: "Samarkand, in today's Uzbekistan, was home to the Sogdians, merchants whose language became the business language of the Silk Road. According to tradition, paper-making reached the Islamic world here after Chinese prisoners were captured at the Battle of Talas in 751. Genghis Khan's army sacked the city in 1220, but it recovered and later became the glittering capital of the conqueror Timur (Tamerlane).",
        lat: 39.655, lng: 66.976, alt: 0.3,
        year: 1279,
        wiki: "Samarkand"
      },
      {
        title: "Baghdad: the House of Wisdom",
        text: "Caliph al-Mansur founded Baghdad in 762 as a perfectly round city on the Tigris. It became one of the largest cities in the world and a centre of learning, where scholars at the House of Wisdom translated Greek, Persian and Indian works and advanced mathematics and astronomy. In 1258 a Mongol army led by Hulagu Khan captured and sacked Baghdad, ending the Abbasid Caliphate there.",
        lat: 33.315, lng: 44.366, alt: 0.3,
        year: 1258,
        open: "e:ev49",
        wiki: "House_of_Wisdom"
      },
      {
        title: "Constantinople: silk comes west",
        text: "For centuries only China knew how to make silk, and Romans paid dearly for it. Around AD 552, according to the historian Procopius, monks smuggled silkworm eggs to Constantinople for Emperor Justinian, starting a silk industry in the Byzantine Empire. Standing between Europe and Asia, Constantinople and its great church of Hagia Sophia remained a key market where Silk Road goods reached the Mediterranean.",
        lat: 41.0086, lng: 28.9802, alt: 0.3,
        year: 1280,
        open: "l:hagia-sophia",
        wiki: "Sericulture"
      },
      {
        title: "Venice: Marco Polo's journey",
        text: "In 1271 the 17-year-old Venetian Marco Polo set out with his father and uncle along the Silk Road and reached the court of the Mongol emperor Kublai Khan in China. He spent about 17 years in the khan's service before returning home, largely by sea, in 1295. While a prisoner of war in Genoa, he described his travels to the writer Rustichello da Pisa, and their book inspired later explorers, including Christopher Columbus.",
        lat: 45.438, lng: 12.336, alt: 0.3,
        year: 1295,
        wiki: "Marco_Polo"
      },
      {
        title: "1347: the Black Death travels the trade routes",
        text: "Trade routes carried disease as well as goods. Plague spread from Central Asia to the Black Sea port of Caffa, besieged by a Mongol army in 1346. In October 1347 Genoese ships brought the disease to Messina in Sicily, and within four years the Black Death had swept across Europe, killing between a third and a half of its population. It also devastated the Middle East and North Africa.",
        lat: 38.19, lng: 15.55, alt: 0.4,
        year: 1347,
        open: "e:ev59",
        wiki: "Black_Death",
        question: { q: "Where did the Black Death first arrive in Western Europe in 1347?", options: ["London", "Messina, Sicily", "Paris", "Lisbon"], answer: 1, explain: "Genoese ships from the Black Sea brought the plague to Messina in October 1347." }
      }
    ]
  },

  // ───────────────────────────── 6. Age of Exploration ─────────────────────────────
  {
    id: "age-of-exploration",
    title: "The Age of Exploration",
    subtitle: "Sail with Zheng He, Columbus, da Gama, Magellan, Elcano and Cook as the oceans are linked for the first time.",
    theme: "Exploration",
    level: "Intermediate",
    minutes: 10,
    cover: "Age_of_Discovery",
    steps: [
      {
        title: "1405: Zheng He's treasure fleets",
        text: "Decades before Europe's great voyages, the Ming admiral Zheng He led seven expeditions from China between 1405 and 1433. His first fleet had over 300 ships and about 27,000 men, sailing through South-East Asia and India to Arabia and the coast of East Africa. Then the Ming court ended the voyages and turned inward, leaving the oceans open to others.",
        lat: 32.06, lng: 118.8, alt: 1.0,
        year: 1405,
        open: "e:ev60",
        wiki: "Zheng_He",
        question: { q: "How far west did Zheng He's fleets reach?", options: ["Japan", "The East African coast", "Portugal", "The Americas"], answer: 1, explain: "His ships visited ports such as Mogadishu and Malindi on the East African coast." }
      },
      {
        title: "1488: rounding the tip of Africa",
        text: "Portugal led Europe's push down the African coast, searching for a sea route to the spices of Asia. In 1488 Bartolomeu Dias was blown south past the end of Africa and became the first European known to round the Cape of Good Hope. He reportedly called it the 'Cape of Storms', and King John II renamed it for the hope of reaching India. Africa's true southernmost point is actually Cape Agulhas, about 150 km to the south-east.",
        lat: -34.357, lng: 18.474, alt: 0.4,
        year: 1488,
        wiki: "Bartolomeu_Dias"
      },
      {
        title: "1492: Columbus reaches the Americas",
        text: "Christopher Columbus believed he could reach Asia by sailing west. Sponsored by Spain, he left Palos on 3 August 1492 with three ships, the Santa María, Pinta and Niña. On 12 October 1492 they landed on an island in the Bahamas that its Taíno inhabitants called Guanahani. Columbus thought he had reached the 'Indies', and he made four voyages without realising he had found continents unknown to Europeans.",
        lat: 24.05, lng: -74.5, alt: 0.6,
        year: 1492,
        open: "e:ev64",
        wiki: "Voyages_of_Christopher_Columbus",
        question: { q: "Where did Columbus think he had arrived in 1492?", options: ["Africa", "A new continent", "Asia (the Indies)", "Greenland"], answer: 2, explain: "He believed he had reached islands off Asia, which is why Caribbean peoples were called 'Indians'." }
      },
      {
        title: "1498: Vasco da Gama reaches India",
        text: "Vasco da Gama sailed from Lisbon in July 1497, rounded the Cape of Good Hope and, guided by a local pilot from Malindi, crossed the Indian Ocean. On 20 May 1498 he arrived at Calicut (Kozhikode) on India's Malabar Coast, the first sea voyage from Europe to India. Scurvy and hardship killed many of his crew, but the pepper and spices he brought home made the voyage hugely profitable and launched Portugal's trading empire.",
        lat: 11.25, lng: 75.78, alt: 0.6,
        year: 1498,
        open: "e:ev65",
        wiki: "Vasco_da_Gama"
      },
      {
        title: "1520: Magellan finds a way through",
        text: "Ferdinand Magellan, a Portuguese captain sailing for Spain, left Sanlúcar de Barrameda in September 1519 with five ships and about 270 men to reach the Spice Islands by sailing west. In October–November 1520 his ships threaded the maze of channels at the tip of South America now called the Strait of Magellan. Emerging into a calm ocean, Magellan named it the 'Pacific'. Crossing it took over three months, and many sailors died of scurvy and starvation.",
        lat: -53.5, lng: -70.9, alt: 0.6,
        year: 1520,
        open: "e:ev66",
        wiki: "Magellan_expedition"
      },
      {
        title: "1521: death at Mactan",
        text: "In March 1521 the expedition reached the Philippines. Magellan tried to force local rulers to accept Spanish rule and Christianity, and on 27 April 1521 he was killed in battle on Mactan Island by warriors of the chief Lapulapu. Lapulapu is celebrated today as a national hero in the Philippines. The survivors burned one ship, as there were too few men left to sail it, and continued to the Spice Islands.",
        lat: 10.31, lng: 124.02, alt: 0.3,
        year: 1521,
        wiki: "Battle_of_Mactan"
      },
      {
        title: "1522: Elcano completes the first circumnavigation",
        text: "Only one ship, the Victoria, commanded by the Basque navigator Juan Sebastián Elcano, made it all the way home. It crossed the Indian Ocean, rounded Africa and reached Sanlúcar de Barrameda on 6 September 1522 with just 18 of the original crew. They had sailed right around the world, proving that the oceans are connected and that the Earth is far larger than many had thought. Their cargo of cloves still paid for the voyage.",
        lat: 36.778, lng: -6.352, alt: 0.4,
        year: 1522,
        wiki: "Juan_Sebastián_Elcano",
        question: { q: "How many of the expedition's five ships completed the voyage around the world?", options: ["One", "Two", "Three", "All five"], answer: 0, explain: "Only the Victoria, under Elcano, returned to Spain in 1522." }
      },
      {
        title: "1770: Cook charts eastern Australia",
        text: "Two centuries later, the British navigator James Cook filled in the Pacific map. On his first voyage in HMS Endeavour (1768–1771) he observed the 1769 transit of Venus from Tahiti, mapped New Zealand, and in April 1770 landed at Botany Bay. He then charted the whole east coast of Australia and claimed it for Britain, ignoring the Aboriginal peoples who had lived there for at least 50,000 years.",
        lat: -34.0, lng: 151.2, alt: 0.6,
        year: 1770,
        open: "e:ev74",
        wiki: "First_voyage_of_James_Cook"
      },
      {
        title: "1779: Cook's last voyage",
        text: "On his second voyage Cook became the first known navigator to cross the Antarctic Circle, in January 1773, proving there was no rich southern continent in temperate latitudes. His third voyage brought him to Hawaii in 1778 and north to the Bering Strait in search of a Northwest Passage. On 14 February 1779 he was killed in a clash with Hawaiians at Kealakekua Bay. His accurate charts were used by sailors for generations.",
        lat: 19.48, lng: -155.93, alt: 0.3,
        year: 1779,
        wiki: "James_Cook"
      }
    ]
  },

  // ───────────────────────────── 7. From the Ice Age to today ─────────────────────────────
  {
    id: "ice-age-to-today",
    title: "From the Ice Age to Today",
    subtitle: "Watch ice sheets melt, land bridges drown, the Sahara turn green and back, and the Aral Sea vanish.",
    theme: "Climate",
    level: "Intermediate",
    minutes: 10,
    cover: "Last_Glacial_Maximum",
    steps: [
      {
        title: "20,000 BC: the Last Glacial Maximum",
        text: "Twenty-two thousand years ago the last ice age was at its coldest. The Laurentide Ice Sheet buried almost all of Canada and the northern USA under ice up to 3–4 km thick, and another sheet covered Scandinavia and northern Europe. The world was on average about 6 °C colder than before industrialisation. With so much water locked up as ice, sea level was about 120 m lower than today.",
        lat: 55.0, lng: -85.0, alt: 2.2,
        year: -20000,
        open: "e:ev0",
        wiki: "Last_Glacial_Maximum",
        question: { q: "How much lower was sea level at the Last Glacial Maximum?", options: ["About 1 m", "About 12 m", "About 120 m", "About 1,200 m"], answer: 2, explain: "Ice sheets held so much water that sea level was roughly 120 m below today's." }
      },
      {
        title: "Beringia: a bridge between continents",
        text: "Low seas exposed a plain up to about 1,000 km wide between Siberia and Alaska, called Beringia. It was a cold, dry grassland grazed by woolly mammoths, horses and bison. People crossed it into the Americas; footprints at White Sands, New Mexico, show humans were there 23,000–21,000 years ago. As the ice melted, rising seas flooded Beringia around 11,000 years ago, creating the Bering Strait.",
        lat: 65.5, lng: -170.0, alt: 1.0,
        year: -20000,
        animals: ["woolly-mammoth"],
        wiki: "Beringia"
      },
      {
        title: "10,000 BC: Doggerland",
        text: "Fast-forward to 10,000 BC. The ice sheets are shrinking fast, but Britain is still joined to mainland Europe by Doggerland, a lowland of rivers, marshes and hunting grounds where the North Sea is today. Hunter-gatherers lived there, and fishing trawlers still dredge up their tools and the bones of mammoths. Rising seas, and finally the Storegga tsunami around 6150 BC, drowned it, and Britain became an island.",
        lat: 54.5, lng: 3.0, alt: 0.8,
        year: -10000,
        wiki: "Doggerland"
      },
      {
        title: "Sundaland and Sahul",
        text: "In South-East Asia, low seas joined Sumatra, Java, Borneo and the Malay Peninsula into a land called Sundaland. Australia, New Guinea and Tasmania formed a single continent called Sahul. Even then, deep water always separated the two, so the first Australians must have crossed open sea by boat at least 50,000 years ago. Watch these lands shrink as you move the slider towards 6000 BC.",
        lat: -2.0, lng: 120.0, alt: 2.0,
        year: -10000,
        wiki: "Sahul",
        question: { q: "How did the first people reach Australia, even when sea level was low?", options: ["They walked on a continuous land bridge", "They crossed stretches of open sea by boat", "They crossed sea ice", "They arrived from Antarctica"], answer: 1, explain: "Deep straits east of Sundaland never dried out, so reaching Sahul always required sea crossings." }
      },
      {
        title: "9700 BC: the Holocene begins",
        text: "About 12,900 years ago the North Atlantic suddenly plunged back into near-glacial cold for about 1,200 years, an episode called the Younger Dryas. When it ended, around 11,700 years ago, Greenland warmed by several degrees within a few decades. That moment marks the official start of the Holocene, the warm and stable epoch in which farming and every civilisation have developed.",
        lat: 72.0, lng: -40.0, alt: 1.6,
        year: -9700,
        open: "e:ev7",
        wiki: "Holocene"
      },
      {
        title: "6000 BC: the Green Sahara",
        text: "By 6000 BC the Sahara looked nothing like today. Stronger summer monsoons, caused by small changes in Earth's orbit, brought rain that turned the desert into grassland and savanna dotted with lakes. Mega-Lake Chad covered around 350,000 km², similar to today's Caspian Sea. Rock art across the Sahara shows giraffes, cattle and swimming people. Between about 3500 and 2500 BC the rains failed and the desert returned.",
        lat: 20.0, lng: 12.0, alt: 1.8,
        year: -6000,
        wiki: "African_humid_period",
        question: { q: "What made the Sahara green between about 9000 and 3500 BC?", options: ["Human irrigation", "Stronger monsoon rains caused by changes in Earth's orbit", "Melting glaciers in the Atlas Mountains", "A rise in sea level"], answer: 1, explain: "Orbital changes strengthened the West African monsoon, bringing rain far into the Sahara." }
      },
      {
        title: "1850: the end of the Little Ice Age",
        text: "Between about 1300 and 1850 the climate was slightly cooler than before, a period called the Little Ice Age. Glaciers in the Alps advanced and reached their most recent maximum around 1850, sometimes destroying farms and villages. At that time atmospheric CO₂ was about 285 ppm, almost the same as in the previous 10,000 years. The Industrial Revolution was about to change that.",
        lat: 46.4, lng: 8.5, alt: 0.4,
        year: 1850,
        wiki: "Little_Ice_Age"
      },
      {
        title: "2026: the Aral Sea",
        text: "Not all changes are caused by climate. The Aral Sea in Central Asia was once the world's fourth-largest lake, about 68,000 km². From the 1960s Soviet planners diverted its two feeder rivers, the Amu Darya and Syr Darya, to irrigate cotton, and the lake shrank by about 90%. Fishing towns were left stranded far from the water. The Kok-Aral dam, finished in 2005, has partly restored the small northern part.",
        lat: 45.0, lng: 59.8, alt: 0.6,
        year: 2026,
        open: "e:ev98",
        wiki: "Aral_Sea"
      },
      {
        title: "2026: a planet warming again",
        text: "Today the ice-age sheets are long gone except in Greenland and Antarctica, and sea level is about 120 m higher than at the Last Glacial Maximum. But humans are now changing the climate faster than at any time in the Holocene: CO₂ has risen from about 280 ppm before industrialisation to around 430 ppm, and 2024 was the first calendar year more than 1.5 °C warmer than 1850–1900. Glaciers and sea ice are shrinking again.",
        lat: 30.0, lng: 0.0, alt: 2.6,
        year: 2026,
        wiki: "Climate_change"
      }
    ]
  },

  // ───────────────────────────── 8. The Ring of Fire ─────────────────────────────
  {
    id: "ring-of-fire",
    title: "The Ring of Fire",
    subtitle: "Circle the Pacific Ocean along a 40,000 km belt of volcanoes, trenches and giant earthquakes.",
    theme: "Geography",
    level: "Intermediate",
    minutes: 10,
    cover: "Ring_of_Fire",
    steps: [
      {
        title: "A horseshoe of fire",
        text: "The Ring of Fire is a horseshoe-shaped belt about 40,000 km long around the edge of the Pacific Ocean. Here oceanic plates dive (subduct) beneath other plates, melting rock that feeds volcanoes and building up stress that is released in earthquakes. About 75% of the world's active and dormant volcanoes lie along it, and roughly 90% of all earthquakes happen here.",
        lat: 5.0, lng: -160.0, alt: 2.6,
        year: 2026,
        wiki: "Ring_of_Fire",
        question: { q: "Roughly what share of the world's earthquakes happen along the Ring of Fire?", options: ["About 10%", "About 40%", "About 90%", "All of them"], answer: 2, explain: "Around 90% of earthquakes occur along the Pacific's subduction zones." }
      },
      {
        title: "Chile 1960: the strongest earthquake ever recorded",
        text: "On 22 May 1960 the Nazca Plate lurched beneath South America near Valdivia, Chile, in a magnitude 9.5 earthquake, the most powerful ever measured. A rupture roughly 1,000 km long shook southern Chile, and the tsunami it caused crossed the Pacific, killing people in Hawaii and, about 22 hours later, in Japan. Two days after the quake the Cordón Caulle volcano began to erupt.",
        lat: -38.24, lng: -73.05, alt: 0.6,
        year: 1960,
        open: "e:ev97",
        wiki: "1960_Valdivia_earthquake"
      },
      {
        title: "Alaska 1964: the Good Friday earthquake",
        text: "On 27 March 1964 the Pacific Plate slipped beneath Alaska in a magnitude 9.2 earthquake, the most powerful ever recorded in North America. The shaking lasted about four and a half minutes. Some areas of land rose by more than 10 m while others sank, and tsunamis struck coastal towns in Alaska and as far away as California. The disaster helped prove the then-new theory of plate tectonics.",
        lat: 61.02, lng: -147.65, alt: 0.6,
        year: 1964,
        open: "e:ev101",
        wiki: "1964_Alaska_earthquake"
      },
      {
        title: "Mount St. Helens 1980",
        text: "In spring 1980 a bulge grew on the north side of Mount St. Helens in Washington State. On 18 May an earthquake triggered the largest landslide in recorded history, and the sudden release of pressure produced a sideways blast that flattened forests over about 600 km². The volcano lost about 400 m of its height, and 57 people died. It is fed by the small Juan de Fuca Plate subducting under North America.",
        lat: 46.191, lng: -122.194, alt: 0.3,
        year: 1980,
        open: "e:ev105",
        wiki: "1980_eruption_of_Mount_St._Helens"
      },
      {
        title: "Japan: Mount Fuji",
        text: "Japan sits where four plates meet, which makes it one of the most volcanically and seismically active countries on Earth. Its tallest peak, Mount Fuji (3,776 m), is a classic cone-shaped stratovolcano built from layers of lava and ash. It last erupted in 1707, dusting Edo (Tokyo) in ash, and is still considered active. Hokusai's woodblock prints made it a symbol of Japan.",
        lat: 35.3606, lng: 138.7274, alt: 0.3,
        year: 2026,
        open: "w:mount-fuji",
        wiki: "Mount_Fuji"
      },
      {
        title: "Tōhoku 2011: earthquake and tsunami",
        text: "On 11 March 2011 a magnitude 9.0–9.1 earthquake struck off north-east Japan, the most powerful ever recorded in the country. The sea floor jumped tens of metres, sending a tsunami that reached heights of up to about 40 m on the coast. Around 18,000 people died or went missing, and flooding caused meltdowns at the Fukushima Daiichi nuclear power plant. The quake even shifted Japan's main island Honshu about 2.4 m east.",
        lat: 38.297, lng: 142.373, alt: 0.6,
        year: 2011,
        open: "e:ev126",
        wiki: "2011_Tōhoku_earthquake_and_tsunami",
        question: { q: "What caused most of the deaths in the 2011 Tōhoku disaster?", options: ["Collapsing buildings", "The tsunami", "Volcanic ash", "Fires"], answer: 1, explain: "The vast majority of victims drowned in the tsunami that swept over coastal towns." }
      },
      {
        title: "The Mariana Trench: where plates dive",
        text: "Subduction also creates the deepest places on Earth. In the Mariana Trench, the old, heavy Pacific Plate sinks beneath the Mariana Plate, and its deepest point, the Challenger Deep, is about 10,935 m below the surface; Mount Everest would fit inside with over 2 km to spare. Jacques Piccard and Don Walsh first reached the bottom in the bathyscaphe Trieste on 23 January 1960.",
        lat: 11.3733, lng: 142.5917, alt: 0.6,
        open: "w:challenger-deep",
        wiki: "Challenger_Deep"
      },
      {
        title: "Pinatubo 1991: cooling the planet",
        text: "On 15 June 1991 Mount Pinatubo in the Philippines produced the second-largest eruption of the 20th century. Scientists had predicted it, and the evacuation of tens of thousands of people saved many lives. The eruption blasted about 15 million tonnes of sulphur dioxide into the stratosphere, where it formed a haze that reflected sunlight and cooled the global average temperature by about 0.5 °C over the following year.",
        lat: 15.13, lng: 120.35, alt: 0.4,
        year: 1991,
        open: "e:ev113",
        wiki: "1991_eruption_of_Mount_Pinatubo"
      },
      {
        title: "Krakatoa 1883",
        text: "On 27 August 1883 the volcanic island of Krakatoa, between Java and Sumatra, exploded. The sound was heard about 4,800 km away on the island of Rodrigues, and the pressure wave circled the globe several times. Tsunamis up to 30 m high killed more than 36,000 people on nearby coasts. In 1927 a new volcano, Anak Krakatau ('Child of Krakatoa'), rose from the sea where the island had stood.",
        lat: -6.102, lng: 105.423, alt: 0.4,
        year: 1883,
        open: "e:ev80",
        wiki: "1883_eruption_of_Krakatoa",
        question: { q: "Roughly how far away was the Krakatoa explosion heard?", options: ["48 km", "480 km", "4,800 km", "48,000 km"], answer: 2, explain: "It was heard on Rodrigues Island in the Indian Ocean, about 4,800 km away." }
      },
      {
        title: "Hunga Tonga 2022",
        text: "On 15 January 2022 the undersea volcano Hunga Tonga–Hunga Ha'apai in the South Pacific produced the most powerful explosion recorded by modern instruments. Its plume rose about 57 km, higher than any eruption plume measured before, reaching into the mesosphere. The blast sent a pressure wave around the world several times and tsunamis across the Pacific, and injected huge amounts of water vapour into the stratosphere.",
        lat: -20.536, lng: -175.382, alt: 0.6,
        year: 2022,
        open: "e:ev134",
        wiki: "2022_Hunga_Tonga–Hunga_Haʻapai_eruption_and_tsunami"
      }
    ]
  },

  // ───────────────────────────── 9. Great rivers ─────────────────────────────
  {
    id: "great-rivers",
    title: "Great Rivers of the World",
    subtitle: "From the Nile to the Mississippi: the rivers that feed nations, carve landscapes and shape history.",
    theme: "Geography",
    level: "Beginner",
    minutes: 9,
    cover: "List_of_river_systems_by_length",
    steps: [
      {
        title: "The Nile: Africa's lifeline",
        text: "The Nile is about 6,650 km long, usually listed as the world's longest river, and its basin covers 11 countries. At Khartoum in Sudan the White Nile, from the lakes of East Africa, meets the Blue Nile from the Ethiopian Highlands, which supplies most of the water and silt during the summer flood. Without the Nile, Egypt would be almost entirely desert; even today nearly all Egyptians live along its banks.",
        lat: 15.62, lng: 32.51, alt: 1.2,
        year: 2026,
        open: "w:nile",
        wiki: "Nile",
        question: { q: "Where do the White Nile and Blue Nile meet?", options: ["Cairo", "Khartoum", "Aswan", "Addis Ababa"], answer: 1, explain: "The two branches join at Khartoum, the capital of Sudan." }
      },
      {
        title: "The Amazon: the biggest river by far",
        text: "The Amazon is about 6,400 km long, close to the Nile, but it carries far more water than any other river: on average about 209,000 m³ every second, around a fifth of all the fresh water rivers deliver to the oceans. Near Manaus, the dark Rio Negro and the sandy Solimões flow side by side for kilometres without mixing, the 'Meeting of Waters'. Remarkably, no bridge crosses the Amazon's main stem.",
        lat: -3.136, lng: -59.903, alt: 0.6,
        open: "w:amazon-river",
        wiki: "Amazon_River"
      },
      {
        title: "The Congo: the deepest river",
        text: "The Congo is Africa's second-longest river at about 4,700 km and has the world's second-largest flow after the Amazon. It is the only major river to cross the Equator twice, so part of its basin always has a rainy season, keeping its flow steady. In its lower rapids it is more than 220 m deep, the deepest river known. Kinshasa and Brazzaville, two capital cities, face each other across it.",
        lat: -4.3, lng: 15.3, alt: 0.6,
        wiki: "Congo_River",
        question: { q: "What record does the Congo River hold?", options: ["Longest river", "Deepest river", "Shortest river", "Saltiest river"], answer: 1, explain: "Measurements in its lower course found depths of more than 220 m." }
      },
      {
        title: "The Zambezi and Victoria Falls",
        text: "The Zambezi, Africa's fourth-longest river at about 2,570 km, plunges over Victoria Falls on the border of Zambia and Zimbabwe. The falls are about 1,708 m wide and 108 m tall, forming the largest single sheet of falling water in the world. Their local name, Mosi-oa-Tunya, means 'the smoke that thunders', after the spray that rises hundreds of metres. David Livingstone was the first European to see them, in 1855.",
        lat: -17.9243, lng: 25.8572, alt: 0.2,
        open: "w:victoria-falls",
        wiki: "Zambezi"
      },
      {
        title: "The Okavango: a river that never reaches the sea",
        text: "Not every river ends in the ocean. The Okavango flows from the highlands of Angola into the Kalahari Desert of Botswana, where it spreads into a vast inland delta and its water evaporates or sinks into the sand. Because the floodwater takes months to arrive, the delta is wettest in the dry season, drawing elephants, hippos and lions. In 2014 it became the 1,000th site on the UNESCO World Heritage List.",
        lat: -19.28, lng: 22.9, alt: 0.6,
        open: "w:okavango-delta",
        animals: ["african-elephant", "hippopotamus"],
        wiki: "Okavango_Delta"
      },
      {
        title: "The Danube: a river of ten countries",
        text: "The Danube flows about 2,850 km from Germany's Black Forest to the Black Sea. It passes through or along ten countries, more than any other river in the world, and through four capital cities: Vienna, Bratislava, Budapest and Belgrade. For centuries it was a frontier of the Roman Empire and later a key trade route. Its delta in Romania and Ukraine is one of Europe's largest wetlands.",
        lat: 47.5, lng: 19.04, alt: 0.8,
        wiki: "Danube"
      },
      {
        title: "Ganges and Brahmaputra: the largest delta",
        text: "The Ganges rises in the Himalayas and is sacred to Hindus, who bathe in it at cities such as Varanasi. It joins the Brahmaputra and the Meghna to build the Ganges–Brahmaputra delta, the largest river delta on Earth, home to well over 100 million people in Bangladesh and India. On its seaward edge lie the Sundarbans, about 10,000 km² of mangrove forest where tigers swim between tidal islands.",
        lat: 21.95, lng: 89.18, alt: 0.6,
        open: "w:sundarbans",
        animals: ["tiger"],
        wiki: "Ganges"
      },
      {
        title: "The Yangtze and the Three Gorges Dam",
        text: "The Yangtze is the longest river in Asia, about 6,300 km, and its basin is home to roughly a third of China's population. Rice farming along it began around 9,000 years ago. The Three Gorges Dam, fully operational in 2012, is the world's largest power station by capacity at 22,500 MW. Its reservoir, though, required more than a million people to move and flooded ancient sites.",
        lat: 30.8231, lng: 111.0033, alt: 0.3,
        open: "l:three-gorges-dam",
        wiki: "Yangtze"
      },
      {
        title: "The Mississippi: America's great river",
        text: "Just north of St. Louis, the Missouri joins the Mississippi. Together they form a river system of about 6,275 km, the longest in North America, draining all or part of 31 US states and two Canadian provinces. Steamboats once carried cotton, grain and passengers along it, as Mark Twain described. Today barges still move huge amounts of grain to the port of New Orleans, and engineers fight to keep the river in its channel.",
        lat: 38.6247, lng: -90.1848, alt: 0.5,
        open: "l:gateway-arch",
        wiki: "Mississippi_River"
      }
    ]
  },

  // ───────────────────────────── 10. Big cats ─────────────────────────────
  {
    id: "big-cats",
    title: "Big Cats of the World",
    subtitle: "Track lions, tigers, jaguars, snow leopards and cheetahs across four continents.",
    theme: "Nature",
    level: "Beginner",
    minutes: 8,
    cover: "Big_cat",
    steps: [
      {
        title: "Meet the big cats",
        text: "'Big cats' usually means the members of the genus Panthera, the lion, tiger, jaguar, leopard and snow leopard, plus the cheetah, which belongs to a different branch of the cat family. Most Panthera cats can roar thanks to a special voice box, but the snow leopard and the cheetah cannot. All of them are top predators, and all are threatened by habitat loss, poaching and conflict with people.",
        lat: 10.0, lng: 40.0, alt: 2.6,
        year: 2026,
        animals: ["lion", "tiger", "jaguar", "snow-leopard", "cheetah"],
        wiki: "Big_cat"
      },
      {
        title: "Lions of the Serengeti",
        text: "Lions are the only truly social cats, living in prides of related females, their cubs and a few males. The Serengeti in Tanzania holds one of the largest lion populations left, around 3,000. A lion's roar can be heard up to about 8 km away, yet lions rest or sleep for up to 20 hours a day. Africa now has only about 20,000–25,000 wild lions, and they have vanished from over 90% of their historic range.",
        lat: -2.33, lng: 34.83, alt: 0.5,
        open: "a:lion",
        animals: ["lion"],
        wiki: "Lion",
        question: { q: "What makes lions different from all other wild cats?", options: ["They live in social groups called prides", "They are the fastest", "They can climb trees", "They only hunt at night"], answer: 0, explain: "Lions are the only cat species that lives in large, permanent family groups." }
      },
      {
        title: "Asiatic lions of Gir",
        text: "Lions once roamed from Greece through the Middle East to India. Today the last wild Asiatic lions live only in and around Gir National Park in Gujarat, India. Hunting had reduced them to perhaps a few dozen animals around 1900, but strict protection has brought them back: the 2025 census counted 891 lions, up from 674 in 2020. Scientists want some moved to a second site so a single disease cannot wipe them out.",
        lat: 21.13, lng: 70.79, alt: 0.4,
        animals: ["lion"],
        wiki: "Asiatic_lion"
      },
      {
        title: "Tigers of Ranthambore",
        text: "The tiger is the largest cat, with males weighing up to about 300 kg. No two tigers have the same stripes, and the pattern is even on the skin. India is home to roughly three-quarters of the world's wild tigers, and reserves such as Ranthambore, with its ruined fort and lakes, are famous for tigers that hunt in daylight. Worldwide, an estimated 5,500 tigers live in the wild.",
        lat: 26.02, lng: 76.5, alt: 0.4,
        open: "a:tiger",
        animals: ["tiger"],
        wiki: "Tiger"
      },
      {
        title: "Swimming tigers of the Sundarbans",
        text: "Unlike most cats, tigers love water. In the Sundarbans, the mangrove forest of the Ganges delta shared by India and Bangladesh, tigers swim between islands and cross tidal channels. It is the only mangrove forest in the world inhabited by tigers. Tigers once ranged from eastern Turkey to Bali, but the Caspian, Javan and Bali tigers were all lost in the 20th century.",
        lat: 21.95, lng: 89.18, alt: 0.5,
        open: "w:sundarbans",
        animals: ["tiger"],
        wiki: "Sundarbans"
      },
      {
        title: "Snow leopards of the Himalaya",
        text: "The snow leopard lives high in the mountains of Central and South Asia, often above 3,000 m, across 12 countries. Its thick tail, nearly as long as its body, helps it balance on cliffs and wraps around its face like a scarf in the cold. Wide, furry paws work like snowshoes. Rarely seen, it is called the 'ghost of the mountains'; Hemis National Park in Ladakh is one of the best places to try to spot one.",
        lat: 34.0, lng: 77.45, alt: 0.5,
        open: "a:snow-leopard",
        animals: ["snow-leopard"],
        wiki: "Snow_leopard",
        question: { q: "What does the snow leopard use its long, thick tail for?", options: ["Swimming", "Balance and warmth", "Signalling to prey", "Storing fat"], answer: 1, explain: "The tail helps it balance on steep slopes and is wrapped around the body for warmth." }
      },
      {
        title: "Jaguars of the Pantanal",
        text: "The jaguar is the largest cat in the Americas and the third-largest in the world. It has one of the most powerful bites of any cat and often kills by biting through the skull of its prey; it can even crack turtle shells. In the Pantanal of Brazil, the world's largest tropical wetland, jaguars swim rivers to hunt caimans. Unlike leopards, jaguars have small spots inside many of their rosettes.",
        lat: -17.37, lng: -56.77, alt: 0.5,
        open: "a:jaguar",
        animals: ["jaguar"],
        wiki: "Jaguar"
      },
      {
        title: "Cheetahs of the Maasai Mara",
        text: "The cheetah is the fastest land animal, reaching close to 100 km/h in short sprints; a captive cheetah named Sarah ran 100 m in 5.95 seconds in 2012. Its light body, long legs and semi-retractable claws work like a sprinter's spikes. Cheetahs hunt in daylight on open plains such as Kenya's Maasai Mara, where lions and hyenas often steal their kills. Only about 6,500–7,100 adults remain.",
        lat: -1.49, lng: 35.14, alt: 0.4,
        open: "a:cheetah",
        animals: ["cheetah"],
        wiki: "Cheetah",
        question: { q: "Which big cat cannot roar but can purr?", options: ["Lion", "Tiger", "Cheetah", "Jaguar"], answer: 2, explain: "Cheetahs purr, chirp and yelp but lack the voice box that lets lions, tigers and jaguars roar." }
      },
      {
        title: "Cheetahs return to India",
        text: "Hunting and habitat loss wiped out cheetahs in India, and the species was declared extinct there in 1952. On 17 September 2022, eight cheetahs from Namibia were released into Kuno National Park in Madhya Pradesh, followed by twelve from South Africa in 2023, the first intercontinental reintroduction of a large carnivore. Some animals have died, but cubs have been born in India, and the project continues.",
        lat: 25.6, lng: 77.4, alt: 0.4,
        animals: ["cheetah"],
        wiki: "African_cheetah_translocation_to_India"
      }
    ]
  },

  // ───────────────────────────── 11. Ocean giants ─────────────────────────────
  {
    id: "ocean-giants",
    title: "Ocean Giants",
    subtitle: "Dive with blue whales, humpbacks, great white sharks, orcas and sea turtles.",
    theme: "Nature",
    level: "Beginner",
    minutes: 8,
    cover: "Blue_whale",
    steps: [
      {
        title: "The blue whale: the largest animal ever",
        text: "The blue whale is the largest animal known to have lived, bigger than any dinosaur: up to about 30 m long and 190 tonnes. Its heart alone weighs about 180 kg. Yet it feeds on krill, tiny shrimp-like animals, gulping up to about 16 tonnes of them a day during the feeding season. In summer and autumn blue whales gather to feed in places like Monterey Bay, California.",
        lat: 36.8, lng: -122.0, alt: 0.6,
        year: 2026,
        open: "a:blue-whale",
        animals: ["blue-whale"],
        wiki: "Blue_whale",
        question: { q: "What does the giant blue whale mainly eat?", options: ["Seals", "Large fish", "Krill", "Squid"], answer: 2, explain: "Blue whales filter enormous quantities of krill through their baleen plates." }
      },
      {
        title: "Whaling and recovery",
        text: "In the 20th century, industrial whaling killed around 360,000 blue whales in the Southern Hemisphere alone. Antarctic blue whales fell to perhaps 360 animals by the early 1970s, under 1% of their original number. Blue whales were protected from 1966 and a worldwide moratorium on commercial whaling took effect in 1986. Today blue whales can be watched off Mirissa in Sri Lanka, where they live in the Indian Ocean year-round.",
        lat: 5.95, lng: 80.46, alt: 0.6,
        animals: ["blue-whale"],
        wiki: "Whaling"
      },
      {
        title: "Humpback whales: singers of the sea",
        text: "Humpback whales migrate thousands of kilometres between cold feeding waters and warm breeding grounds. Many North Pacific humpbacks spend summer in Alaska and travel to Hawaii in winter to give birth. Males sing long, complex songs that change over time and spread from population to population. Groups also hunt together by blowing spiralling 'bubble nets' to trap fish.",
        lat: 20.85, lng: -156.65, alt: 0.6,
        open: "a:humpback-whale",
        animals: ["humpback-whale"],
        wiki: "Humpback_whale"
      },
      {
        title: "A conservation success story",
        text: "Whaling also brought humpbacks close to extinction, but since protection their numbers have climbed steadily, and in 2008 the IUCN moved the species to 'Least Concern'. Hervey Bay in Queensland, Australia, is a resting stop for humpbacks migrating south to Antarctica. Each whale's black-and-white tail pattern is unique, so researchers can identify individuals from photographs.",
        lat: -25.2, lng: 152.9, alt: 0.6,
        animals: ["humpback-whale"],
        wiki: "Hervey_Bay",
        question: { q: "How do scientists recognise individual humpback whales?", options: ["By their songs", "By the pattern under their tail flukes", "By their size", "By tags fitted at birth"], answer: 1, explain: "The black-and-white pattern on the underside of each tail fluke is unique, like a fingerprint." }
      },
      {
        title: "Great white sharks",
        text: "The great white shark grows to 4–6 m and can live for more than 70 years. It senses the tiny electric fields of prey with special pores in its snout and keeps parts of its body warmer than the water. A tagged shark named Nicole swam from South Africa to Australia and back, about 20,000 km, in under nine months. Since 2017, orcas hunting sharks for their livers have driven great whites away from parts of South Africa's coast, including Gansbaai.",
        lat: -34.58, lng: 19.35, alt: 0.5,
        open: "a:great-white-shark",
        animals: ["great-white-shark"],
        wiki: "Great_white_shark"
      },
      {
        title: "Orcas: the ocean's top predator",
        text: "The orca, or killer whale, is actually the largest member of the dolphin family. Orcas live in family pods led by older females, and each pod has its own dialect of calls. Different populations specialise in different prey: the Southern Resident orcas near the San Juan Islands eat mainly Chinook salmon, while others hunt seals or even whales. With fewer than 80 animals, the Southern Residents are endangered.",
        lat: 48.52, lng: -123.15, alt: 0.4,
        open: "a:orca",
        animals: ["orca"],
        wiki: "Orca"
      },
      {
        title: "Green sea turtles of Raine Island",
        text: "Green sea turtles can weigh up to about 190 kg and, as adults, graze seagrass like underwater lawnmowers. Raine Island, on the northern Great Barrier Reef, is one of the largest green turtle nesting sites in the world. The temperature of the sand decides whether hatchlings are male or female, and a 2018 study found about 99% of young turtles from these warming northern beaches are female.",
        lat: -11.59, lng: 144.03, alt: 0.4,
        open: "a:green-sea-turtle",
        animals: ["green-sea-turtle"],
        wiki: "Green_sea_turtle",
        question: { q: "What decides whether a sea turtle hatchling is male or female?", options: ["Its mother's age", "The temperature of the sand", "The moon phase", "The size of the egg"], answer: 1, explain: "Warmer nests produce more females, which is why rising temperatures are a concern." }
      },
      {
        title: "Galápagos: where ocean currents meet",
        text: "Around the Galápagos Islands, cold, nutrient-rich currents meet warm tropical water, creating a feast for marine life. Near the northern islands of Darwin and Wolf, divers see schools of hundreds of hammerhead sharks and whale sharks, the largest fish in the sea, which can exceed 12 m. Green turtles nest on the beaches, and the islands are protected by one of the largest marine reserves in the eastern Pacific.",
        lat: -0.67, lng: -90.55, alt: 0.8,
        open: "w:galapagos-islands",
        animals: ["green-sea-turtle"],
        wiki: "Galápagos_Marine_Reserve"
      }
    ]
  },

  // ───────────────────────────── 12. Wonders of the deserts ─────────────────────────────
  {
    id: "desert-wonders",
    title: "Wonders of the Deserts",
    subtitle: "Cross sand seas, salt flats and the hottest and driest places on Earth.",
    theme: "Geography",
    level: "Beginner",
    minutes: 8,
    cover: "Desert",
    steps: [
      {
        title: "What makes a desert?",
        text: "A desert is defined by dryness, not heat: it is a place that receives very little precipitation, usually less than about 250 mm a year. That means the largest desert on Earth is actually Antarctica, about 14 million km² of ice where very little snow falls. Deserts cover around a third of the world's land and form under high-pressure zones, behind mountains, or beside cold ocean currents.",
        lat: -82.0, lng: 0.0, alt: 2.2,
        year: 2026,
        wiki: "Desert",
        question: { q: "What is the largest desert on Earth?", options: ["The Sahara", "The Gobi", "Antarctica", "The Arabian Desert"], answer: 2, explain: "Deserts are defined by low precipitation, and Antarctica is the largest at about 14 million km²." }
      },
      {
        title: "The Sahara",
        text: "The Sahara is the largest hot desert, about 9.2 million km², roughly the size of the United States. Only a small part is sand dunes; most is rocky plateaus and gravel plains. Some dunes reach around 180 m, and its highest point is Emi Koussi, a volcano in Chad, at 3,415 m. Dromedary camels, which store fat in their humps, have carried trade across it for about 2,000 years.",
        lat: 23.4, lng: 13.0, alt: 1.6,
        open: "w:sahara",
        animals: ["dromedary"],
        wiki: "Sahara"
      },
      {
        title: "The Eye of the Sahara",
        text: "In the desert of Mauritania lies the Richat Structure, a set of rings about 40 km across that is easy to spot from space. Astronauts on the Gemini missions in the 1960s used it as a landmark. It looks like an impact crater but is actually an eroded dome: layers of rock pushed up and then worn away, with harder layers forming the rings.",
        lat: 21.124, lng: -11.402, alt: 0.3,
        open: "w:richat-structure",
        wiki: "Richat_Structure"
      },
      {
        title: "The Danakil Depression",
        text: "In the Afar region of Ethiopia, three tectonic plates are pulling apart, and the land has sunk more than 100 m below sea level. Dallol, in the Danakil Depression, recorded an average annual temperature of 34.6 °C between 1960 and 1966, the highest of any inhabited place. Its hot springs form yellow and green pools so acidic their pH is near zero. Afar miners still cut blocks of salt by hand and carry them out by camel caravan.",
        lat: 14.2417, lng: 40.2997, alt: 0.3,
        open: "w:danakil-depression",
        wiki: "Danakil_Depression"
      },
      {
        title: "Sossusvlei and the Namib",
        text: "The Namib, along the coast of Namibia, is thought to be the world's oldest desert, dry for 55–80 million years. At Sossusvlei, red dunes coated in iron oxide rise to about 325 m; one called 'Big Daddy' is among the tallest. Beetles in the Namib survive by collecting droplets from the morning fog that rolls in from the cold Atlantic, which is also what keeps this coast so dry.",
        lat: -24.733, lng: 15.367, alt: 0.3,
        open: "w:sossusvlei",
        wiki: "Namib",
        question: { q: "Why is the Namib coast so dry even though it is beside the ocean?", options: ["A cold ocean current stops rain clouds forming", "It is very high up", "Mountains block the wind from the east", "It is near the North Pole"], answer: 0, explain: "The cold Benguela Current cools the air above it, making fog but very little rain." }
      },
      {
        title: "The Atacama: driest place outside the poles",
        text: "In northern Chile, the Atacama Desert is squeezed between the Andes and the cold Pacific, and has been extremely dry for at least 3 million years. Some weather stations there have never recorded rain. Its high, clear, dry skies make it one of the best places on Earth for astronomy, home to the ALMA radio telescope at about 5,000 m. After rare heavy rains, the desert bursts into flower.",
        lat: -24.5, lng: -69.25, alt: 0.6,
        open: "w:atacama-desert",
        wiki: "Atacama_Desert"
      },
      {
        title: "Salar de Uyuni: a giant mirror",
        text: "Salar de Uyuni in Bolivia is the world's largest salt flat, about 10,582 km² at 3,656 m above sea level, left behind when prehistoric lakes dried up. Its surface is so flat, varying by less than a metre, that it is used to calibrate satellites. After rain, a thin layer of water turns it into a vast mirror reflecting the sky. Beneath its crust lies one of the world's largest lithium deposits.",
        lat: -20.1338, lng: -67.4891, alt: 0.4,
        open: "w:salar-de-uyuni",
        wiki: "Salar_de_Uyuni"
      },
      {
        title: "Lençóis Maranhenses: dunes and lagoons",
        text: "On Brazil's north-east coast, white dunes stretch for kilometres and look like a desert, but the region receives around 1,200 mm of rain a year. Between January and June rain fills the hollows between the dunes, creating thousands of blue and green lagoons that peak around mid-year. Some lagoons even contain fish that survive the dry season buried in mud. It became a UNESCO World Heritage Site in 2024.",
        lat: -2.53, lng: -43.12, alt: 0.4,
        open: "w:lencois-maranhenses",
        wiki: "Lençóis_Maranhenses_National_Park"
      },
      {
        title: "Uluru: the heart of Australia",
        text: "In the dry centre of Australia, Uluru rises 348 m above the plain and is about 9.4 km around. Its sandstone glows deep red at sunrise and sunset because of iron oxide in the rock. Uluru is sacred to the Anangu, its traditional owners, to whom ownership was returned in 1985. Out of respect for their wishes, climbing Uluru was permanently banned from 26 October 2019.",
        lat: -25.3444, lng: 131.0369, alt: 0.3,
        open: "w:uluru",
        animals: ["red-kangaroo"],
        wiki: "Uluru"
      }
    ]
  },

  // ───────────────────────────── 13. Treasures under the ground ─────────────────────────────
  {
    id: "treasures-underground",
    title: "Treasures Under the Ground",
    subtitle: "Discover where the oil, lithium, cobalt, copper, gold and rare earths that power modern life come from.",
    theme: "Geography",
    level: "Intermediate",
    minutes: 10,
    cover: "Mining",
    steps: [
      {
        title: "Oil: the Ghawar field",
        text: "Crude oil formed from plankton buried and heated over millions of years, and it is still the world's largest single source of energy. Saudi Arabia's Ghawar field, about 280 km long and discovered in 1948, is the largest conventional oil field ever found. In 2023 the world produced about 96 million barrels a day; the top producers were the United States (about 20%), Saudi Arabia and Russia (about 12% each).",
        lat: 25.02, lng: 49.56, alt: 0.8,
        year: 2026,
        resources: ["oil"],
        open: "c:SAU",
        wiki: "Ghawar_Field",
        question: { q: "Which country has been the world's largest oil producer since 2018?", options: ["Saudi Arabia", "Russia", "United States", "Venezuela"], answer: 2, explain: "Shale (tight) oil from basins like the Permian made the United States the top producer." }
      },
      {
        title: "Oil: where the oil age began",
        text: "On 27 August 1859 Edwin Drake struck oil at Titusville, Pennsylvania, in the first commercial well drilled specifically for oil. At first oil was refined into kerosene for lamps; cars and planes came later. Today the United States is the largest producer again thanks to shale oil from places like the Permian Basin in Texas, extracted by horizontal drilling and hydraulic fracturing. Venezuela, though, holds the largest proven reserves.",
        lat: 41.61, lng: -79.66, alt: 0.5,
        year: 1859,
        open: "e:ev78",
        resources: ["oil"],
        wiki: "Drake_Well_Museum"
      },
      {
        title: "Lithium: Australia's hard-rock mines",
        text: "Lithium is the lightest metal, light enough to float on water, and batteries now use more than 80% of it. World production rose roughly sixfold between 2015 and 2023. Australia is the largest producer, supplying close to half of the world's mined lithium in 2023, mostly from hard-rock spodumene mines such as Greenbushes in Western Australia. Much of the ore is shipped to China for refining.",
        lat: -33.87, lng: 116.06, alt: 0.6,
        year: 2026,
        resources: ["lithium"],
        open: "c:AUS",
        wiki: "Greenbushes_mine"
      },
      {
        title: "Lithium: the Lithium Triangle",
        text: "Where Chile, Argentina and Bolivia meet lies the 'Lithium Triangle', holding over half of the world's identified lithium resources. Here lithium is dissolved in salty brine beneath salt flats. The brine is pumped into huge ponds and left in the sun for months until the lithium is concentrated. Chile's Salar de Atacama is the richest brine deposit in the world, but the process uses large amounts of water in one of the driest places on Earth.",
        lat: -23.42, lng: -68.27, alt: 0.8,
        resources: ["lithium"],
        open: "c:CHL",
        wiki: "Lithium_Triangle",
        question: { q: "How is lithium extracted in the Lithium Triangle?", options: ["Deep underground tunnels", "Evaporating salty brine in ponds", "Dredging rivers", "Melting glaciers"], answer: 1, explain: "Brine is pumped from under the salt flats and the sun evaporates the water, concentrating the lithium." }
      },
      {
        title: "Cobalt: the Congo's Copperbelt",
        text: "Cobalt helps many lithium-ion batteries stay stable and last longer. In 2023 the Democratic Republic of the Congo mined about 74% of the world's cobalt, mainly around Kolwezi in the Central African Copperbelt, where it comes out alongside copper. Large industrial mines operate here, but so do artisanal miners digging by hand, sometimes including children, which has drawn international criticism. Indonesia is now the second-largest producer.",
        lat: -10.71, lng: 25.39, alt: 0.6,
        resources: ["cobalt"],
        open: "c:COD",
        wiki: "Mining_industry_of_the_Democratic_Republic_of_the_Congo"
      },
      {
        title: "Copper: Escondida, Chile",
        text: "Copper was the first metal people smelted, and alloyed with tin it gave the Bronze Age its name. Today it carries electricity in wires, motors and power grids, and an electric car contains roughly two to four times as much copper as a petrol car. Chile produces almost a quarter of the world's mined copper, and Escondida, high in the Atacama Desert, is the largest copper mine by output.",
        lat: -24.27, lng: -69.07, alt: 0.5,
        resources: ["copper"],
        wiki: "Escondida"
      },
      {
        title: "Gold: the Witwatersrand",
        text: "In 1886 gold was found on the Witwatersrand ridge in South Africa, and Johannesburg grew up almost overnight. This basin has produced around a third of all the gold ever mined, and nearby Mponeng is one of the deepest mines on Earth, reaching about 4 km down where rock temperatures exceed 60 °C. Since 2007, though, China has been the world's largest gold producer. All the gold ever mined, about 216,000 tonnes, would fit in a cube about 22 m on each side.",
        lat: -26.2, lng: 28.0, alt: 0.5,
        resources: ["gold"],
        open: "c:ZAF",
        wiki: "Witwatersrand_Gold_Rush",
        question: { q: "Roughly how big would a cube of all the gold ever mined be?", options: ["About 2 m on each side", "About 22 m on each side", "About 220 m on each side", "About 2 km on each side"], answer: 1, explain: "About 216,000 tonnes of gold would make a cube only about 22 m wide, because gold is so dense." }
      },
      {
        title: "Rare earths: Bayan Obo",
        text: "Rare-earth elements such as neodymium make the strongest permanent magnets, used in electric-car motors, wind turbines and phones. Bayan Obo in Inner Mongolia is the world's largest rare-earth deposit. In 2023 China mined about 68% of the world's rare earths and refined around 90%, giving it great power over supply chains. Despite the name, rare earths are not especially rare; they are just hard to find in concentrated, easy-to-separate form.",
        lat: 41.77, lng: 109.97, alt: 0.6,
        resources: ["rare-earths"],
        open: "c:CHN",
        wiki: "Rare-earth_element"
      },
      {
        title: "Diamonds: Jwaneng, Botswana",
        text: "Diamonds formed deep in Earth's mantle, most of them 1 to 3.5 billion years ago, and were carried up in volcanic pipes of kimberlite. Botswana's Jwaneng is the world's richest diamond mine by value, and diamond revenue has helped make Botswana one of Africa's most prosperous countries since independence. Russia produces about a third of the world's rough diamonds by volume, and Botswana is second.",
        lat: -24.52, lng: 24.7, alt: 0.5,
        resources: ["diamonds"],
        open: "c:BWA",
        wiki: "Jwaneng_diamond_mine"
      }
    ]
  },

  // ───────────────────────────── 14. Climate change today ─────────────────────────────
  {
    id: "climate-change-today",
    title: "Climate Change Today",
    subtitle: "See how sea ice, glaciers, coral reefs and forests have changed since 1980.",
    theme: "Climate",
    level: "Intermediate",
    minutes: 10,
    cover: "Climate_change",
    steps: [
      {
        title: "The Keeling Curve",
        text: "In 1958 the scientist Charles David Keeling began measuring carbon dioxide in the air from an observatory high on Mauna Loa, Hawaii, far from local pollution. His record, the Keeling Curve, showed CO₂ rising every year. In 1980 it stood at about 339 ppm; by 2026 it has passed 430 ppm, more than 50% above the roughly 280 ppm before the Industrial Revolution and higher than at any time in at least 800,000 years.",
        lat: 19.536, lng: -155.576, alt: 0.4,
        year: 1980,
        open: "e:ev96",
        wiki: "Keeling_Curve",
        question: { q: "What was the CO₂ level before the Industrial Revolution?", options: ["About 180 ppm", "About 280 ppm", "About 380 ppm", "About 480 ppm"], answer: 1, explain: "Ice cores show about 280 ppm before industrialisation; today it is above 420 ppm." }
      },
      {
        title: "Arctic sea ice in 1980",
        text: "Satellites have measured Arctic sea ice continuously since 1979. Each September, at the end of the summer melt, the ice reaches its minimum. In 1980 the September ice covered about 7.7 million km² of the Arctic Ocean. Polar bears depend on this ice as a platform for hunting seals at their breathing holes.",
        lat: 82.0, lng: -150.0, alt: 1.8,
        year: 1980,
        animals: ["polar-bear"],
        wiki: "Arctic_sea_ice_decline"
      },
      {
        title: "2012: record-low sea ice",
        text: "In September 2012 the Arctic sea ice shrank to a record low of about 3.6 million km², less than half of its 1980 extent. Overall, the summer ice cover has shrunk by roughly 40% since 1979, and the remaining ice is also much thinner and younger. Less white ice means more dark ocean absorbing sunlight, which warms the Arctic further: the Arctic is heating up several times faster than the global average.",
        lat: 82.0, lng: -150.0, alt: 1.8,
        year: 2012,
        open: "e:ev127",
        animals: ["polar-bear"],
        wiki: "Arctic_sea_ice_decline",
        question: { q: "Why does melting sea ice speed up Arctic warming?", options: ["Ice releases CO₂", "Dark ocean water absorbs more sunlight than white ice", "Polar bears produce heat", "Melting ice is warm"], answer: 1, explain: "White ice reflects most sunlight; open water absorbs it, a feedback that amplifies warming." }
      },
      {
        title: "Melting mountain glaciers",
        text: "Glaciers around the world are shrinking. Swiss glaciers lost about 6% of their remaining volume in 2022 and another 4% in 2023, the two worst years on record, and about a quarter of their volume vanished between 2015 and 2025. Kilimanjaro lost about 85% of its ice between 1912 and 2011. Mountain glaciers supply water to hundreds of millions of people, especially in Asia and the Andes.",
        lat: 46.4, lng: 8.5, alt: 0.4,
        year: 2022,
        wiki: "Retreat_of_glaciers_since_1850"
      },
      {
        title: "The Greenland ice sheet",
        text: "Greenland's ice sheet covers about 1.71 million km² and is more than 3 km thick in places. If it all melted, global sea levels would rise by about 7 m. NASA's GRACE satellites show it lost on average about 270 billion tonnes of ice a year between 2002 and 2023, raising sea level by around 0.8 mm every year. Together with warming, expanding seawater, this has pushed global sea level up by more than 10 cm since 1993.",
        lat: 72.0, lng: -40.0, alt: 1.2,
        year: 2023,
        open: "w:greenland-ice-sheet",
        wiki: "Greenland_ice_sheet"
      },
      {
        title: "The Aral Sea: a human-made disaster",
        text: "Not every environmental change comes from global warming. The Aral Sea, once the world's fourth-largest lake, shrank by about 90% after Soviet planners diverted its rivers to irrigate cotton from the 1960s. By 2014 its eastern basin had dried out completely, leaving a new desert, the Aralkum, whose salty dust storms harm health and crops. Compare the lake in 1960 and today with the time slider.",
        lat: 45.0, lng: 59.8, alt: 0.6,
        year: 2014,
        open: "e:ev98",
        wiki: "Aral_Sea"
      },
      {
        title: "Coral bleaching",
        text: "When the sea stays too warm for too long, corals expel the colourful algae living in their tissues and turn white. Bleached corals can recover if the water cools, but they often die. The first global bleaching event was recorded in 1998, and a marine heatwave in 2016 killed a large part of the coral on the northern Great Barrier Reef. The fourth global bleaching event, from 2023 to 2025, affected about 84% of the world's reefs in 82 countries and territories.",
        lat: -14.5, lng: 145.5, alt: 0.8,
        year: 2016,
        open: "e:ev129",
        wiki: "Coral_bleaching"
      },
      {
        title: "The Amazon's arc of deforestation",
        text: "Since 1970, roads, cattle ranching and soy farming have cleared roughly a fifth of Brazil's Amazon rainforest, mostly in an 'arc' along its southern and eastern edges. Clearing peaked at about 29,000 km² in 1995 and 27,800 km² in 2004. Stronger enforcement cut it sharply, and Brazil's satellite monitoring measured 5,796 km² in the year to July 2025, the lowest in over a decade. Trees store carbon, so deforestation also adds to global warming.",
        lat: -10.5, lng: -58.0, alt: 1.2,
        year: 2025,
        open: "e:ev116",
        wiki: "Deforestation_of_the_Amazon_rainforest",
        question: { q: "What has been the main driver of Amazon deforestation in Brazil?", options: ["Wildfires caused by lightning", "Cattle ranching and farming", "Building cities", "Mining for gold only"], answer: 1, explain: "Most cleared land in the Brazilian Amazon has been turned into cattle pasture and cropland such as soy." }
      },
      {
        title: "2015: the Paris Agreement",
        text: "In December 2015, almost every country in the world adopted the Paris Agreement. They agreed to try to keep global warming well below 2 °C above pre-industrial levels and to pursue efforts to limit it to 1.5 °C. Each country sets its own plan to cut emissions and must update it every five years. Since then, solar and wind power and electric vehicles have grown faster than almost anyone predicted.",
        lat: 48.95, lng: 2.43, alt: 0.6,
        year: 2015,
        open: "e:ev128",
        wiki: "Paris_Agreement"
      },
      {
        title: "2024: the hottest year on record",
        text: "According to the World Meteorological Organization, 2024 was the hottest year since records began, about 1.55 °C warmer than the 1850–1900 average and the first calendar year above 1.5 °C. Extreme heat struck many regions; during the Hajj pilgrimage in Mecca, temperatures above 50 °C contributed to over a thousand deaths. Every fraction of a degree matters, because it makes heatwaves, droughts, wildfires and floods more frequent and intense.",
        lat: 21.42, lng: 39.83, alt: 1.6,
        year: 2026,
        open: "e:ev139",
        wiki: "2024_in_climate_change"
      }
    ]
  }
];

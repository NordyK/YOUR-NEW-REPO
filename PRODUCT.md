# Product

<!-- impeccable:product-schema 1 -->

> Written from the owner's written brief without an interview (the owner asked for uninterrupted work).
> Items marked *(inferred)* are assumptions to confirm.

## Platform

web

## Users
- Learners of world geography and history: students, teachers preparing a lesson, and curious adults who want to explore a place or an era. *(inferred from "a professional tool for learning world geography and history")*
- Typical situation: a laptop or tablet session of 10–40 minutes, exploring freely or following a lesson; phones are used for quick look-ups. *(inferred)*

## Product Purpose
Earth Explorer (folder `earth-explorer/`) is an interactive 3D globe for learning where things are and what happened there and when. Natural wonders, landmarks, wildlife ranges, natural resources, country borders and news are all placed on one globe, and a time machine runs from 20,000 BC to today. Success means a learner can answer "where is it, what is it, and how did this place change over time?" from inside the tool. They read detailed articles, see real photos and data, and check what they learned.

## Positioning
Most tools specialise in one thing: maps (Google Earth), history atlases, encyclopedias or news maps. Earth Explorer puts the place, the era and the sources together. Moving the timeline changes the borders, the landscape, the wildlife, the landmarks and the news of that period on one globe. Every item opens real sources: articles, photos and period newspapers.

## Operating Context
- Runs in the browser as a static site (GitHub Pages target); no accounts, no server, no API keys.
- Content comes from curated data files in the repo plus live public sources: Wikipedia and Wikimedia Commons, Wikidata, Natural Earth, historical-basemaps, NASA, NOAA, USGS, World Bank, Library of Congress and news APIs.
- Used both for open exploration and for guided learning (tours, quizzes). *(inferred from "learning")*

## Capabilities and Constraints
- Existing: zoomable satellite globe; 87 wonders, 121 landmarks, 42 animals (approximate ranges, animated 3D miniatures), 16 resources; a timeline with historical borders, landscape change, events and world indicators; real photo galleries; nearby Wikipedia places; shareable links.
- Requested by the owner: full articles and detailed information; current news on the globe; *historical* news tied to a place and period, using real sources rather than invented headlines; real era-specific borders; better animal models; ideas adopted from the repos the owner named (Orbitra, Earth Enterprise, himawari.js, geo-maps, WebWorldWind, openglobus, TerraExplorer), rewritten as our own code.
- Constraint: never fabricate news, quotes or facts. Historical news must come from real archives or events databases, with links to the source.
- Constraint: third-party code is studied, not copied; licences are respected and credited.

## Brand Commitments
- Name: Earth Explorer.
- The owner wants it "professional" and "very good looking". *(binding but non-specific)*

## Evidence on Hand
- Curated data: `earth-explorer/js/data/*.js` (wonders, landmarks, animals, resources, timeline).
- No testimonials, users or usage numbers exist; do not invent any.

## Product Principles
1. Every claim traceable: each fact, photo and headline links back to its source.
2. Place and time are one interface: anything shown on the globe responds to the year.
3. Depth on demand: glanceable on the globe, detailed in the article view.
4. Learning, not just browsing: guide, compare and test, not only display. *(inferred)*
5. Fast and calm: the globe stays responsive and the interface stays out of the way of the content.

## Accessibility & Inclusion
- Keyboard-accessible controls, visible focus, reduced-motion support and readable text sizes (established in earlier passes). WCAG 2.2 AA is the working target. *(inferred)*

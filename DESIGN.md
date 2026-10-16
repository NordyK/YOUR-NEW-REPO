---
name: Earth Explorer
description: A night-time atlas of the planet, with ink panels over the globe, one brass accent, and a scholarly article reader.
colors:
  ink-space: "#05080f"
  ink-panel: "#0c1220"
  ink-raised: "#131b2d"
  ink-hover: "#1c2740"
  hairline: "rgba(214, 226, 255, 0.11)"
  hairline-strong: "rgba(214, 226, 255, 0.2)"
  text: "#eef2fa"
  text-secondary: "#b4bdd0"
  text-tertiary: "#8f9ab2"
  brass: "#f0b54a"
  brass-hover: "#f6c66b"
  brass-ink: "#1b1306"
  brass-text: "#ffe2ad"
  brass-soft: "rgba(240, 181, 74, 0.16)"
  brass-wash: "rgba(240, 181, 74, 0.1)"
  series-a: "#3987e5"
  series-b: "#d95926"
  correct: "#4fbf7a"
  wrong: "#e05a5a"
typography:
  display:
    fontFamily: "Newsreader, Georgia, serif"
    fontSize: "clamp(34px, 4.4vw, 50px)"
    fontWeight: 600
    lineHeight: 1.05
    letterSpacing: "-0.02em"
  headline:
    fontFamily: "Newsreader, Georgia, serif"
    fontSize: "28px"
    fontWeight: 600
    lineHeight: 1.2
  card-title:
    fontFamily: "Newsreader, Georgia, serif"
    fontSize: "29px"
    fontWeight: 600
    lineHeight: 1.1
    letterSpacing: "-0.015em"
  year:
    fontFamily: "Newsreader, Georgia, serif"
    fontSize: "30px"
    fontWeight: 600
    lineHeight: 1
    letterSpacing: "-0.01em"
    fontFeature: "lnum, tnum"
  title:
    fontFamily: "Newsreader, Georgia, serif"
    fontSize: "22px"
    fontWeight: 600
    letterSpacing: "-0.01em"
  subhead:
    fontFamily: "Newsreader, Georgia, serif"
    fontSize: "17px"
    fontWeight: 600
  standfirst:
    fontFamily: "Public Sans, system-ui, sans-serif"
    fontSize: "19.5px"
    fontWeight: 400
    lineHeight: 1.55
  body-reading:
    fontFamily: "Public Sans, system-ui, sans-serif"
    fontSize: "17px"
    fontWeight: 400
    lineHeight: 1.65
  body:
    fontFamily: "Public Sans, system-ui, sans-serif"
    fontSize: "14px"
    fontWeight: 400
    lineHeight: 1.45
  body-small:
    fontFamily: "Public Sans, system-ui, sans-serif"
    fontSize: "13.5px"
    fontWeight: 400
    lineHeight: 1.3
  label:
    fontFamily: "Public Sans, system-ui, sans-serif"
    fontSize: "12.5px"
    fontWeight: 700
  running-head:
    fontFamily: "Public Sans, system-ui, sans-serif"
    fontSize: "12px"
    fontWeight: 400
    letterSpacing: "0.08em"
    fontFeature: "tnum"
  apparatus-head:
    fontFamily: "Public Sans, system-ui, sans-serif"
    fontSize: "11px"
    fontWeight: 700
    letterSpacing: "0.12em"
rounded:
  hairline: "2px"
  xs: "6px"
  sm: "8px"
  seg: "10px"
  md: "12px"
  pill: "99px"
spacing:
  2xs: "4px"
  xs: "6px"
  sm: "8px"
  md: "10px"
  lg: "12px"
  xl: "16px"
  2xl: "18px"
  reader-section: "28px"
  reader-top: "44px"
  reader-gutter: "64px"
components:
  panel:
    backgroundColor: "rgba(12, 18, 32, 0.94)"
    textColor: "{colors.text}"
    rounded: "{rounded.md}"
  button-primary:
    backgroundColor: "{colors.brass}"
    textColor: "{colors.brass-ink}"
    rounded: "{rounded.sm}"
    padding: "0 13px"
    height: "36px"
  button-primary-hover:
    backgroundColor: "{colors.brass-hover}"
  button-secondary:
    backgroundColor: "transparent"
    textColor: "{colors.text}"
    rounded: "{rounded.sm}"
    padding: "0 13px"
    height: "36px"
  button-secondary-hover:
    backgroundColor: "{colors.ink-hover}"
  icon-button:
    backgroundColor: "transparent"
    textColor: "{colors.text}"
    rounded: "{rounded.sm}"
    size: "36px"
  chip:
    backgroundColor: "transparent"
    textColor: "{colors.text}"
    rounded: "{rounded.pill}"
    padding: "0 12px"
    height: "34px"
  chip-pressed:
    backgroundColor: "{colors.brass-soft}"
    textColor: "{colors.brass-text}"
  input:
    backgroundColor: "{colors.ink-raised}"
    textColor: "{colors.text}"
    rounded: "{rounded.sm}"
    padding: "0 10px"
    height: "36px"
  rail-tab:
    backgroundColor: "transparent"
    textColor: "{colors.text-secondary}"
    typography: "{typography.label}"
    rounded: "{rounded.sm}"
    width: "76px"
  rail-tab-selected:
    backgroundColor: "{colors.brass-soft}"
    textColor: "{colors.brass-text}"
  list-item:
    backgroundColor: "transparent"
    textColor: "{colors.text}"
    typography: "{typography.body-small}"
    rounded: "{rounded.sm}"
    padding: "6px 8px"
  list-item-hover:
    backgroundColor: "{colors.ink-raised}"
  list-item-selected:
    backgroundColor: "{colors.brass-soft}"
  seg-control:
    backgroundColor: "{colors.ink-raised}"
    textColor: "{colors.text-secondary}"
    rounded: "{rounded.seg}"
    padding: "3px"
  seg-option-selected:
    backgroundColor: "{colors.ink-hover}"
    textColor: "{colors.text}"
    height: "34px"
  timeline-play:
    backgroundColor: "{colors.brass}"
    textColor: "{colors.brass-ink}"
    rounded: "{rounded.pill}"
    size: "42px"
  chart-tooltip:
    backgroundColor: "{colors.ink-space}"
    textColor: "{colors.text}"
    rounded: "{rounded.sm}"
    padding: "6px 9px"
  reader-running-head:
    backgroundColor: "{colors.ink-panel}"
    textColor: "{colors.text-secondary}"
    typography: "{typography.running-head}"
    padding: "10px 20px"
  reader-column:
    backgroundColor: "{colors.ink-space}"
    textColor: "{colors.text}"
    typography: "{typography.body-reading}"
    width: "68ch"
  reader-margin:
    textColor: "{colors.text-secondary}"
    width: "248px"
  apparatus-key:
    backgroundColor: "transparent"
    textColor: "{colors.brass}"
    rounded: "{rounded.pill}"
    size: "18px"
---

# Design System: Earth Explorer

## Overview

**Creative North Star: "The Night Atlas"**

Earth Explorer is a night-time atlas laid over a living globe. The planet is the brightest thing on screen; everything else is dark ink glass that floats above it and gets out of its way. One warm brass accent marks what you can act on and what is selected. Every other colour on the globe belongs to the map itself: layer colours, event colours, conservation status, and the temperature-tinted time track.

The chrome is dense and quiet: a top bar, a vertical icon rail with a list panel, a detail card on the right and a time machine along the bottom, all set in Public Sans at 13 to 14px with Newsreader serif titles. When a learner opens an article, the globe gives way to a full-screen reader built like an encyclopedia entry with scholarly apparatus. It has a single 68ch reading column, a 248px margin of keyed notes (Where, Key figures, In time, Nearby, Sources), hairline rules, and no boxes inside the column.

The system rejects the "Wikipedia page pasted in a modal" and blog-card layouts for reading. It never uses decorative colour on the chrome: brass is earned by action, selection or a key.

**Key Characteristics:**
- Ink panels at 94% opacity with a 10px backdrop blur, floating over the globe on a 10px inset.
- One brass accent (#f0b54a) for primary actions, selection, focus rings, keys and year links.
- Newsreader serif for titles, the year and section heads; Public Sans for everything that works.
- Hairline rules (1px, one device pixel on 2x screens in the reader) instead of boxes.
- Tabular lining numerals for every figure, year and coordinate.
- Lucide line icons (24-grid, 2px stroke) at 15 to 20px; no glyph or emoji icons.

## Colors

A cold, blue-black ink scale with one warm brass accent; the map supplies all other colour.

### Primary
- **Lantern Brass** (#f0b54a): primary buttons, the timeline play button and slider thumb ring, pressed chips, the selected rail tab's inset bar, list markers, focus outlines, links in panels, apparatus keys, and year rows in the reader margin. It is also `accent-color` and the text caret.
- **Brass Hover** (#f6c66b): the hover fill for brass buttons and the play button.
- **Brass Ink** (#1b1306): text and icons on brass fills.
- **Pale Brass Text** (#ffe2ad): text on brass-tinted surfaces (selected rail tab, pressed chip, notice).
- **Brass Glaze** (16% brass): selected list rows, the selected rail tab and pressed chips. **Brass Wash** (10% brass with a 32% brass border) is used only for the `notice` callout in panels.

### Secondary
- **Series Blue** (#3987e5) and **Series Rust** (#d95926): the only data-series colours. A single-series chart uses Series Blue with no legend; two series use Blue then Rust with a legend, and the compare table carries matching swatches.

### Tertiary
- **Answer Green** (#4fbf7a) and **Answer Red** (#e05a5a): quiz option borders over a 12 to 14% tint of the same hue, reserved for right/wrong feedback.

### Neutral
- **Deep Space** (#05080f): page background, the reader page, chart tooltips; also the theme colour.
- **Panel Ink** (#0c1220): every floating panel (as rgba at 0.94) and the reader running head.
- **Raised Ink** (#131b2d): inputs, selects, the segmented control track, row hover, image placeholders.
- **Hover Ink** (#1c2740): button, chip and row hover; the selected segment; tag and bar tracks.
- **Hairline** (rgba(214,226,255,0.11)) and **Strong Hairline** (rgba(214,226,255,0.2)): borders, list separators, reader rules. Strong hairline is used for secondary button borders, dropdowns and the margin top rule on narrow screens.
- **Starlight** (#eef2fa): primary text. **Mist** (#b4bdd0, about 8.9:1 on Panel Ink): secondary text. **Slate** (#8f9ab2, about 6:1): tertiary text, placeholders, axis labels, captions.

### Named Rules
**The One Brass Rule.** Brass on the chrome means "act here" or "this is selected". Never use it for decoration, section tints or emphasis in running text.

**The Map Owns Colour Rule.** Hues other than brass and ink belong to map content: layer markers, event chips, IUCN status badges and the temperature track. Chrome never borrows them.

**The Two Series Rule.** Charts draw at most two series, Blue then Rust. A single series has no legend.

## Typography

**Display Font:** Newsreader (self-hosted variable 200-800, with Georgia, serif)
**Body Font:** Public Sans (self-hosted variable 200-800, with system-ui, sans-serif)

**Character:** A bookish serif for names of places, years and section heads, paired with a plain civic sans for everything you operate. The serif always appears at weight 600; the sans carries weight only through 700 labels and figure values.

### Hierarchy
- **Display** (Newsreader 600, clamp(34px, 4.4vw, 50px), 1.05, -0.02em, balanced wrap): the reader article title only.
- **Card Title** (Newsreader 600, 29px, 1.1): the detail card's item name.
- **Year** (Newsreader 600, 30px, 1; 24px under 760px): the timeline year, with tabular lining numerals.
- **Headline** (Newsreader 600, 28px, 1.2): reader H2, which always sits under a hairline rule. Reader H3 is 21px.
- **Title** (Newsreader 600, 22px): panel heads and the welcome card. News headlines are 19px; detail-card H3 is 17px.
- **Standfirst** (Public Sans 400, 19.5px, 1.55): the reader's lead paragraph.
- **Body Reading** (Public Sans 400, 17px, 1.65, about 68ch): the reader column. Summaries in the detail card are 15px/1.55, capped at 65ch.
- **Body** (Public Sans 400, 14px, 1.45): the UI base. List names and figures are 13.5px; descriptions 13px.
- **Label** (Public Sans 700, 12.5px): group titles, news categories, chart captions (13px).
- **Running Head** (Public Sans 400, 12px, uppercase, 0.08em, tabular): the reader's kind · place · year line only.
- **Apparatus Head** (Public Sans 700, 11px, uppercase, 0.12em): the head of each margin note, beside its brass key.

### Named Rules
**The Tabular Figures Rule.** Years, coordinates, statistics, axis labels and table cells use `lining-nums tabular-nums`.

**The Serif Is a Name Rule.** Newsreader sets names, titles, years and section heads. It never sets controls, labels or paragraphs. Blockquotes and the italic reader description are the only exceptions.

## Layout

The globe fills the viewport (`position: fixed; inset: 0`). Chrome floats over it as separate panels inset 10px from every edge with 10px gaps. The top bar is 56px high (52px under 760px). The sidebar is a 76px icon rail plus a 332px list panel. The right-hand detail card is 404px wide. The timeline dock is 190px high. The sidebar and detail card fill the space between the top bar and the dock.

Spacing works at small steps: 4, 6, 8, 10, 12, 16 and 18px. Panels pad 16px 14px. The detail card's body pads 14px 18px 22px. Rows pad 6 to 9px.

The reader is a full-screen layer on Deep Space. Below a running head, a centred grid (max 1140px) holds a `minmax(0, 68ch)` column and a 248px margin, 64px apart, with 44px top padding. The margin is sticky at 24px. Wikipedia thumbnail figures float right at 45% of the column width with a 28px gutter. Headings, tables and galleries clear the floats.

**Breakpoints:**
- **1100px:** the sidebar narrows to 300px and the card to 370px. Timeline jumps after the fourth are hidden, along with the Labels and Spin chips.
- **980px (reader):** the page becomes one column (28px 18px padding). The margin drops below the article under a strong hairline, and an inline place/time strip appears under the lead. The running head drops the place. Reader tools become 44px icon buttons with short labels; Back to globe stays a brass button. Floated figures unfloat.
- **760px:** a mobile toolbar. The menu toggle appears and the brand and Borders chip hide. Search collapses to a 42px icon that expands across the bar on focus. The map-style select becomes a 42px layers icon button. The rail turns horizontal above the list (max 64vh). The detail card becomes a 64vh bottom sheet. The timeline hides its jumps, era line and sparklines.
- **pointer: coarse:** chips and jump buttons are at least 40px high, list rows 44px, icon buttons 42px, and map dots 30px.

## Elevation & Depth

This is a hybrid system. Panels that float over the 3D globe carry one ambient shadow and a backdrop blur, so they read as glass above the planet. Inside a panel, and in the whole reader, depth comes from tonal steps (Deep Space, Panel Ink, Raised Ink, Hover Ink) and hairlines, never from additional shadows.

### Shadow Vocabulary
- **Float** (`box-shadow: 0 12px 32px rgba(0,0,0,0.45), 0 2px 6px rgba(0,0,0,0.35)`): every `.panel` over the globe, the search dropdown, and lightbox images.
- **Marker lift** (`box-shadow: 0 2px 6px rgba(0,0,0,0.55)`): globe marker dots and the timeline slider thumb, which sit directly on imagery.
- **Glass** (`backdrop-filter: blur(10px) saturate(120%)`): panels only.

### Named Rules
**The Glass Over Globe Rule.** Only surfaces that float over the globe get Float and Glass. Nothing inside a panel or the reader is lifted.

**The Flat Reader Rule.** The reading column has no cards, shadows or tinted boxes. Structure comes from 1px hairlines (0.5px on 2x screens), tonal steps and type.

## Shapes

Corners soften by containment. Panels and dropdowns use 12px, controls (buttons, inputs, selects, rows) use 8px, the segmented track 10px with 7px segments, and thumbnails and paper scans 6 to 8px. Chips, tags, status badges, bars, switches and the timeline play button are full pills; markers and apparatus keys are circles. Flags get a 2px corner and swatches 3px. The reader's lead photo and floated figures are square-cornered plates with no frame or tint. Rules are 1px hairlines everywhere; the only thicker strokes are state marks: the 2px inset brass bar on the selected rail tab, the 2px brass underline on the selected segment, and the 2px brass border on the current thumbnail.

## Components

### Buttons
Quiet, compact and outlined; brass only for the one primary action in a group.
- **Shape:** gently rounded (8px), at least 36px high, 0 13px padding, 15px leading icon with a 7px gap, 13px text.
- **Primary:** Lantern Brass fill, Brass Ink text at 700. Hover is Brass Hover.
- **Secondary:** transparent with a Strong Hairline border. Hover fills Hover Ink.
- **Icon button:** 36px square (42px on coarse pointers), Hairline border, Hover Ink on hover.
- **Focus:** a 2px brass outline at 2px offset, everywhere.

### Chips
- **Style:** 34px pills (30px in filter rows and event strips), Hairline border, transparent, 13px text, optional 15px icon.
- **State:** pressed toggles (`aria-pressed`) take Brass Glaze, a 55% brass border and Pale Brass Text. Event chips colour only their icon with the event hue.

### Cards / Containers
- **Panel:** Panel Ink at 0.94, Hairline border, 12px corners, Float shadow, Glass blur.
- **Detail card (#info):** a 404px right panel. It has a 16:10 hero photo with a gradient credit strip, a 64×46 thumbnail row, a Card Title, a meta line with dot separators, and a 15px summary. Key figures are a hairline-ruled definition grid (labels in Mist, values at 600 tabular), with brass-marker fact lists and an actions row. The sticky close button sits on a 72% Deep Space disc.
- **Notice:** the one tinted box: Brass Wash, a 32% brass border, Pale Brass Text and 8px corners. Use it in panels only.

### Inputs / Fields
- **Style:** Raised Ink fill, Hairline border, 8px corners, 36 to 38px high, Slate placeholders. The search field has a 15px leading icon at 36px inset.
- **Focus:** the border turns brass; the caret is brass.
- **Switch:** a 38×22 pill track in Hover Ink that turns brass when on, with a Brass Ink knob.

### Navigation
- **Rail:** a 76px vertical tablist of 20px icons over 11.5px labels in Mist. Hover is Starlight on Raised Ink. The selected tab has Brass Glaze, Pale Brass Text, a brass icon and a 2px inset brass bar on the left. Under 760px the rail runs horizontally with 64px tabs and a bottom hairline.
- **Segmented control:** a Raised Ink track (3px padding, 10px corners) holding 34px segments in Mist. The selected segment fills Hover Ink with Starlight text and a 2px brass underline. Disabled segments are at 45% opacity.
- **List item:** a 28px circular icon dot tinted with the layer colour, a name at 13.5px with a 12px Mist subline, and an optional pill tag. Hover is Raised Ink; selected is Brass Glaze. Rows with a value use hairline separators and a 6px progress bar.

### Timeline (time machine)
A full-width dock. It holds a 42px brass play disc, the serif Year with a Mist era line, and pill jump buttons. Below them is an 8px track tinted by global temperature (blue colder, red warmer), with a white thumb in a 5px brass ring. One-pixel tick marks show the years that have a border map, with tabular axis ticks in Slate. Then comes a hairline-ruled strip of statistic cells (11.5px key, 16px 700 tabular value, red/green delta, 56×26 sparkline) and a scrolling row of event chips.

### Charts
Thin marks over a recessive grid (rgba(214,226,255,0.09)), with 10.5px tabular Slate axis labels. The current year is a dashed Slate rule. The hover crosshair is a 1px Mist line with a tooltip on Deep Space (Strong Hairline border, 8px corners) that shows the year in Slate and one 10×3 swatch row per series. Every chart has a "Show data" disclosure that opens a hairline table. Colour follows the Two Series Rule.

### Article Reader (signature)
- **Running head:** a Panel Ink bar with a bottom hairline. Its left side is the kind · place · year line in Running Head type with Slate separators. Its right side holds the Contents (a native select overlaid on a button), Listen, Wikipedia and Back to globe (primary) tools.
- **Column:** Display title, an italic Newsreader description in Mist, the Standfirst, then the full-column lead photo (max 520px high) with a 12.5px Slate credit line. Body type is 17/1.65. Links are text-coloured with a 28% brass underline that turns full on hover. List markers are brass. Blockquotes use a Strong Hairline left rule in 19px Newsreader. Tables are hairline-ruled with Mist headers.
- **Keyed apparatus:** a superscript brass letter (10.5px, 700) marks the first mention of a place, figure or date in the text. Each letter links to the matching margin note, whose head carries the same letter in an 18px brass-outlined circle. Notes without an anchor get no key. Year rows are brass and set the time machine; nearby rows show tabular distances.
- **Narrow screens:** the inline place/time strip (coordinates, Show on globe and up to three year rows) sits between hairlines under the lead.

### Globe Markers
26px circular dots (22px small, 18px Wikipedia, 19px when far) on near-black, with a 2px border and icon in the layer colour. Event markers have a dashed border. A selected marker fills with its colour and gets a white border. Labels are 12px 700 on a dark 6px tag, shown on hover, on focus, or when zoomed in with labels on.

### Motion
One easing curve, `cubic-bezier(0.16, 1, 0.3, 1)`. Panels and the welcome card slide in 24px over 0.35 to 0.4s, the reader rises 8px over 0.3s, the sidebar slides over 0.35s, and colour changes take 0.12 to 0.2s. Under `prefers-reduced-motion` the loader spin, card and reader entrances, and the sidebar, marker, hero and switch transitions are removed, and contents jumps scroll instantly.

## Do's and Don'ts

### Do:
- **Do** float chrome over the globe as Panel Ink glass (0.94, 10px blur, 12px corners, Float shadow), inset 10px from the edges.
- **Do** reserve Lantern Brass (#f0b54a) for the primary action, selection, focus, keys and year links.
- **Do** set every number in tabular lining figures.
- **Do** separate content with 1px hairlines, at one device pixel on 2x screens in the reader.
- **Do** chart one series in Series Blue without a legend. Add Series Rust and a legend only for a second series, and always ship the crosshair tooltip and the "Show data" table.
- **Do** key every margin note that has an anchor in the text with a brass index letter at its first mention.
- **Do** use Lucide line icons from the shared subset, at 15px in controls and 20px in the rail.
- **Do** give every animation a reduced-motion path.

### Don't:
- **Don't** put cards, shadows or tinted boxes inside the reader column. Photos are plain plates.
- **Don't** lift elements inside a panel with extra shadows. Depth inside panels is tonal.
- **Don't** use map hues (layer, event or status colours) on chrome, or brass as decoration.
- **Don't** put a small uppercase kicker or eyebrow above a title. Tracked uppercase is only for the reader's running head and margin-note heads.
- **Don't** use glyph or emoji icons, or a system display face. Newsreader and Public Sans are self-hosted.
- **Don't** draw more than two series in a chart, or a legend for one series.
- **Don't** set controls or paragraphs in Newsreader.

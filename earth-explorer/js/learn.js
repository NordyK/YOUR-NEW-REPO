// Learn: guided tours (narrated camera + time-machine stories) and quizzes generated from our datasets.
import { icon } from './icons.js';

const QUIZZES = [
  { id: 'where', title: 'Where in the world?', text: 'Find wonders and landmarks by clicking the globe. The closer you are, the more points you score.', icon: 'crosshair' },
  { id: 'flags', title: 'Flags', text: 'Match the flag to its country.', icon: 'flag' },
  { id: 'capitals', title: 'Capitals', text: 'Pick the capital city of each country.', icon: 'landmark' },
  { id: 'country', title: 'Name the country', text: 'A country lights up on the globe. Which one is it?', icon: 'map' },
  { id: 'when', title: 'When was it built?', text: 'Put famous landmarks in their era.', icon: 'hourglass' },
  { id: 'habitat', title: 'Animal homes', text: 'Where in the world does each animal live?', icon: 'paw-print' },
];
const ROUNDS = 10;

export function createLearn(ctx) {
  const { esc, fmtYear, TOURS, WONDERS, LANDMARKS, ANIMALS, COUNTRIES, countryInfo } = ctx;
  const store = (() => { try { return JSON.parse(localStorage.getItem('ee-learn') || '{}'); } catch { return {}; } })();
  store.tours ||= {}; store.quiz ||= {};
  const save = () => { try { localStorage.setItem('ee-learn', JSON.stringify(store)); } catch { /* storage unavailable */ } };
  let tour = null, quiz = null;
  const shuffle = (a) => { const b = [...a]; for (let i = b.length - 1; i > 0; i--) { const j = Math.floor(Math.random() * (i + 1)); [b[i], b[j]] = [b[j], b[i]]; } return b; };
  const pick = (a, n) => shuffle(a).slice(0, n);

  // ---------------------------------------------------------------- panel
  function renderPanel(el) {
    const done = Object.keys(store.tours).filter((k) => store.tours[k].done).length;
    el.innerHTML = `<div class="side-head"><h2>Learn</h2></div>
      <p class="side-desc">Guided stories that move the camera and the time machine for you, and quizzes built from everything on the globe.</p>
      <div class="group-title">Guided tours <span class="muted">${done}/${TOURS.length} completed</span></div>
      ${TOURS.length ? TOURS.map((t) => `<button class="item tour-row" data-tour="${esc(t.id)}">
          <span class="dot-ic" style="--c:#f0b54a">${icon(t.theme === 'Nature' ? 'paw-print' : t.theme === 'Climate' ? 'thermometer' : t.theme === 'Exploration' ? 'compass' : t.theme === 'Geography' ? 'mountain' : 'scroll')}</span>
          <span class="nm">${esc(t.title)}<small>${esc(t.theme)} · ${t.steps.length} stops · ${t.minutes} min${store.tours[t.id]?.done ? ' · completed' : store.tours[t.id]?.step ? ` · stop ${store.tours[t.id].step + 1}` : ''}</small></span>
          ${store.tours[t.id]?.done ? `<span class="tag">${icon('circle-check')}</span>` : ''}</button>`).join('') : '<p class="side-desc">Tours are loading.</p>'}
      <div class="group-title">Quizzes</div>
      ${QUIZZES.map((q) => `<button class="item" data-quiz="${q.id}"><span class="dot-ic" style="--c:#9db1ff">${icon(q.icon)}</span><span class="nm">${esc(q.title)}<small>${esc(q.text)}</small></span>${store.quiz[q.id] != null ? `<span class="tag" title="Best score">${store.quiz[q.id]}${q.id === 'where' ? '' : '/' + ROUNDS}</span>` : ''}</button>`).join('')}`;
    el.querySelectorAll('[data-tour]').forEach((b) => (b.onclick = () => startTour(b.dataset.tour)));
    el.querySelectorAll('[data-quiz]').forEach((b) => (b.onclick = () => startQuiz(b.dataset.quiz)));
  }

  // ---------------------------------------------------------------- tours
  function startTour(id) {
    const t = TOURS.find((x) => x.id === id);
    if (!t) return;
    endQuiz(true);
    const resume = store.tours[id]?.step && !store.tours[id]?.done ? store.tours[id].step : 0;
    tour = { t, i: resume, answered: {} };
    ctx.tourOn(true);
    showStep();
  }
  function endTour() {
    if (!tour) return;
    clearTimeout(tour.auto); clearTimeout(tour.descend);
    if ('speechSynthesis' in window) speechSynthesis.cancel();
    tour = null;
    ctx.tourOn(false);
    ctx.closeInfo();
    ctx.refreshPanel();
  }
  function showStep() {
    const { t, i } = tour;
    const s = t.steps[i];
    store.tours[t.id] = { ...(store.tours[t.id] || {}), step: i };
    save();
    if (s.year != null) ctx.setYear(s.year, true);
    ctx.setLayers({ animals: s.animals || null, resources: s.resources || null });
    // documentary camera: long hops pull back to show the journey, then descend on the stop
    const prev = tour.lastPos, alt = s.alt ?? 0.8;
    const far = prev && ctx.km(prev, s) > 2500;
    tour.lastPos = { lat: s.lat, lng: s.lng };
    clearTimeout(tour.descend);
    if (far) { ctx.fly({ lat: s.lat, lng: s.lng, altitude: Math.max(1.9, alt) }, 1800); tour.descend = setTimeout(() => ctx.fly({ lat: s.lat, lng: s.lng, altitude: alt }, 1600), 1900); }
    else ctx.fly({ lat: s.lat, lng: s.lng, altitude: alt }, 2200);
    const q = s.question;
    const ans = tour.answered[i];
    ctx.openInfo(`${s.wiki ? ctx.gallery(s.wiki, s.title, 'compass') : ''}<div class="info-main tour-step">
      <div class="tour-progress" role="progressbar" aria-valuemin="1" aria-valuemax="${t.steps.length}" aria-valuenow="${i + 1}" aria-label="Tour progress"><span style="width:${((i + 1) / t.steps.length) * 100}%"></span></div>
      <div class="meta">${icon('compass')}<span>${esc(t.title)}</span><span class="sep">·</span><span class="num">Stop ${i + 1} of ${t.steps.length}</span>${s.year != null ? `<span class="sep">·</span><span class="num">${esc(fmtYear(s.year))}</span>` : ''}</div>
      <h2>${esc(s.title)}</h2>
      <p class="summary">${esc(s.text)}</p>
      ${q ? `<div class="quiz-q"><h3>${esc(q.q)}</h3>${q.options.map((o, k) => `<button class="opt ${ans != null ? (k === q.answer ? 'right' : k === ans ? 'wrong' : '') : ''}" data-ans="${k}" ${ans != null ? 'disabled' : ''}>${esc(o)}</button>`).join('')}
        ${ans != null ? `<p class="feedback ${ans === q.answer ? 'ok' : 'no'}">${ans === q.answer ? icon('circle-check') + 'Correct. ' : icon('circle-x') + 'Not quite. '}${esc(q.explain || '')}</p>` : ''}</div>` : ''}
      <div class="actions tour-nav">
        <button class="btn" data-tour-prev ${i === 0 ? 'disabled' : ''}>${icon('chevron-left')}Back</button>
        ${i < t.steps.length - 1 ? `<button class="btn primary" data-tour-next>Next stop${icon('arrow-right')}</button>` : `<button class="btn primary" data-tour-finish>${icon('circle-check')}Finish tour</button>`}
        ${s.open ? `<button class="btn" data-tour-open="${esc(s.open)}">${icon('info')}Open its card</button>` : ''}
        ${s.wiki ? `<button class="btn" data-read="" data-title="${esc(s.wiki.replace(/_/g, ' '))}">${icon('book-open')}Read more</button>` : ''}
        ${'speechSynthesis' in window ? `<button class="btn" data-tour-narrate aria-pressed="${!!tour.narrate}">${icon('radio')}${tour.narrate ? 'Stop narration' : 'Narrate'}</button>` : ''}
        <button class="btn" data-tour-exit>${icon('x')}Leave tour</button>
      </div></div>`, null);
    const root = document.getElementById('info-body');
    root.querySelector('[data-tour-prev]')?.addEventListener('click', () => { tour.i--; showStep(); });
    root.querySelector('[data-tour-next]')?.addEventListener('click', () => { tour.i++; showStep(); });
    root.querySelector('[data-tour-finish]')?.addEventListener('click', () => { store.tours[t.id] = { done: true, step: 0 }; save(); ctx.toast(`Tour complete: ${t.title}`); endTour(); });
    root.querySelector('[data-tour-exit]')?.addEventListener('click', endTour);
    root.querySelector('[data-tour-open]')?.addEventListener('click', (e) => ctx.openByKey(e.currentTarget.dataset.tourOpen));
    root.querySelectorAll('[data-ans]').forEach((b) => (b.onclick = () => { tour.answered[i] = +b.dataset.ans; showStep(); }));
    root.querySelector('[data-tour-narrate]')?.addEventListener('click', () => { tour.narrate = !tour.narrate; if (!tour.narrate) speechSynthesis.cancel(); showStep(); });
    if (tour.narrate && tour.spoken !== i) narrate(s, i);
  }
  // Narration: read the stop aloud, then move on by itself (stopping at questions so the learner can answer).
  function narrate(s, i) {
    tour.spoken = i;
    speechSynthesis.cancel();
    const u = new SpeechSynthesisUtterance(`${s.title}. ${s.text}${s.question ? ' ' + s.question.q : ''}`);
    u.lang = 'en-GB';
    u.onend = () => {
      if (!tour || !tour.narrate || tour.i !== i || s.question) return;
      tour.auto = setTimeout(() => { if (tour && tour.narrate && tour.i === i && i < tour.t.steps.length - 1) { tour.i++; showStep(); } }, 1600);
    };
    speechSynthesis.speak(u);
  }

  // ---------------------------------------------------------------- quizzes
  const countryPool = () => COUNTRIES.filter((c) => c.iso2 && countryInfo(c.iso3)?.capital?.length && c.props.POP_EST > 300000);
  function makeQuestion(kind) {
    if (kind === 'where') {
      const pool = [
        ...WONDERS.map((w) => ({ name: w.name, hint: `${w.type}, ${w.country}`, lat: w.lat, lng: w.lng })),
        ...LANDMARKS.filter((l) => l.destroyed == null).map((l) => ({ name: l.name, hint: [l.city, l.country].filter(Boolean).join(', '), lat: l.lat, lng: l.lng })),
      ];
      return { kind, ...pick(pool, 1)[0] };
    }
    if (kind === 'flags' || kind === 'capitals' || kind === 'country') {
      const four = pick(countryPool(), 4), right = four[0];
      const opts = shuffle(four);
      return { kind, c: right, options: opts.map((c) => (kind === 'capitals' ? countryInfo(c.iso3).capital[0] : c.longName)), answer: opts.indexOf(right) };
    }
    if (kind === 'when') {
      const l = pick(LANDMARKS.filter((x) => x.built != null), 1)[0];
      const era = (y) => (y < -3000 ? 'before 3000 BC' : y < -1000 ? '3000–1000 BC' : y < 1 ? '1000 BC – AD 1' : y < 1000 ? 'AD 1 – 1000' : y < 1500 ? '1000–1500' : y < 1800 ? '1500–1800' : y < 1950 ? '1800–1950' : 'after 1950');
      const eras = ['before 3000 BC', '3000–1000 BC', '1000 BC – AD 1', 'AD 1 – 1000', '1000–1500', '1500–1800', '1800–1950', 'after 1950'];
      const right = era(l.built);
      const opts = shuffle([right, ...pick(eras.filter((e) => e !== right), 3)]);
      return { kind, l, options: opts, answer: opts.indexOf(right), explain: `${l.name} was completed in ${fmtYear(l.built)}.` };
    }
    if (kind === 'habitat') {
      const a = pick(ANIMALS.filter((x) => x.extinct == null && x.hotspots?.length), 1)[0];
      const cont = (lat, lng) => (lat < -60 ? 'Antarctica & Southern Ocean' : lng < -30 ? (lat > 12 ? 'North America' : 'South America') : lng < 60 && lat > 35 ? 'Europe' : lng < 52 && lat < 35 ? 'Africa' : lng > 110 && lat < -10 ? 'Australia & Oceania' : 'Asia');
      const right = cont(a.hotspots[0].lat, a.hotspots[0].lng);
      const all = ['Africa', 'Asia', 'Europe', 'North America', 'South America', 'Australia & Oceania', 'Antarctica & Southern Ocean'];
      const opts = shuffle([right, ...pick(all.filter((x) => x !== right), 3)]);
      return { kind, a, options: opts, answer: opts.indexOf(right), explain: `${a.name}: ${a.habitat}. Best seen at ${a.hotspots[0].name}.` };
    }
  }
  function startQuiz(kind) {
    if (tour) endTour();
    quiz = { kind, round: 0, score: 0, q: null, answered: null, meta: QUIZZES.find((q) => q.id === kind) };
    ctx.tourOn(true);
    next();
  }
  function endQuiz(silent) {
    if (!quiz) return;
    ctx.pickOnGlobe(null); ctx.highlightCountry(null); ctx.showGuess(null);
    quiz = null;
    ctx.tourOn(false);
    if (!silent) { ctx.closeInfo(); ctx.refreshPanel(); }
  }
  function next() {
    quiz.round++;
    quiz.answered = null;
    ctx.showGuess(null); ctx.highlightCountry(null);
    if (quiz.round > ROUNDS) return finish();
    quiz.q = makeQuestion(quiz.kind);
    if (quiz.q.kind === 'country') { ctx.highlightCountry(quiz.q.c.iso3); const ce = ctx.countryCenter(quiz.q.c); if (ce) ctx.fly({ lat: ce.lat, lng: ce.lng, altitude: Math.min(2.4, Math.max(0.6, ce.span / 22)) }, 1400); }
    if (quiz.q.kind === 'where') { ctx.fly({ altitude: 2.6 }, 1000); ctx.pickOnGlobe((pt) => answerWhere(pt)); }
    render();
  }
  function answerWhere(pt) {
    if (quiz?.answered != null || quiz?.q.kind !== 'where') return;
    const km = ctx.km(pt, quiz.q);
    const pts = Math.round(1000 * Math.exp(-km / 1500));
    quiz.score += pts;
    quiz.answered = { km, pts };
    ctx.pickOnGlobe(null);
    ctx.showGuess({ guess: pt, truth: quiz.q });
    render();
  }
  function render() {
    const q = quiz.q, m = quiz.meta;
    let bodyHtml = '';
    if (q.kind === 'where') {
      bodyHtml = `<h2>${esc(q.name)}</h2><p class="summary muted">${esc(q.hint)}</p>
        ${quiz.answered ? `<p class="feedback ${quiz.answered.pts > 500 ? 'ok' : 'no'}">${icon(quiz.answered.pts > 500 ? 'circle-check' : 'circle-x')}You were <b class="num">${Math.round(quiz.answered.km).toLocaleString('en-US')} km</b> away: <b class="num">${quiz.answered.pts}</b> points.</p>` : `<p class="notice">${icon('crosshair')}Click the globe where you think it is.</p>`}`;
    } else {
      const prompt = q.kind === 'flags' ? `<img class="quiz-flag" src="https://flagcdn.com/w320/${q.c.iso2.toLowerCase()}.png" alt="A national flag">` : q.kind === 'capitals' ? `<h2>${esc(q.c.longName)}</h2><p class="muted">What is its capital?</p>` : q.kind === 'country' ? `<h2>Which country is lit up?</h2>` : q.kind === 'when' ? `<h2>${esc(q.l.name)}</h2><p class="muted">${esc([q.l.city, q.l.country].filter(Boolean).join(', '))}. When was it completed?</p>` : `<h2>${esc(q.a.name)}</h2><p class="muted"><i>${esc(q.a.scientific)}</i>. Where does it live in the wild?</p>`;
      bodyHtml = `${prompt}<div class="quiz-q">${q.options.map((o, k) => `<button class="opt ${quiz.answered != null ? (k === q.answer ? 'right' : k === quiz.answered ? 'wrong' : '') : ''}" data-qa="${k}" ${quiz.answered != null ? 'disabled' : ''}>${esc(o)}</button>`).join('')}</div>
        ${quiz.answered != null ? `<p class="feedback ${quiz.answered === q.answer ? 'ok' : 'no'}">${icon(quiz.answered === q.answer ? 'circle-check' : 'circle-x')}${quiz.answered === q.answer ? 'Correct.' : `It was ${esc(q.options[q.answer])}.`} ${esc(q.explain || '')}</p>` : ''}`;
    }
    ctx.openInfo(`<div class="info-main quiz">
      <div class="tour-progress" role="progressbar" aria-valuemin="1" aria-valuemax="${ROUNDS}" aria-valuenow="${quiz.round}" aria-label="Quiz progress"><span style="width:${(quiz.round / ROUNDS) * 100}%"></span></div>
      <div class="meta">${icon(m.icon)}<span>${esc(m.title)}</span><span class="sep">·</span><span class="num">Question ${quiz.round} of ${ROUNDS}</span><span class="sep">·</span><span class="num">Score ${quiz.score}</span></div>
      ${bodyHtml}
      <div class="actions">${quiz.answered != null ? `<button class="btn primary" data-qnext>${quiz.round < ROUNDS ? 'Next question' : 'See result'}${icon('arrow-right')}</button>` : ''}<button class="btn" data-qexit>${icon('x')}Stop quiz</button></div></div>`, null);
    const root = document.getElementById('info-body');
    root.querySelectorAll('[data-qa]').forEach((b) => (b.onclick = () => { quiz.answered = +b.dataset.qa; if (quiz.answered === q.answer) quiz.score++; render(); }));
    root.querySelector('[data-qnext]')?.addEventListener('click', next);
    root.querySelector('[data-qexit]')?.addEventListener('click', () => endQuiz(false));
  }
  function finish() {
    const m = quiz.meta, best = store.quiz[quiz.kind];
    const isBest = best == null || quiz.score > best;
    if (isBest) { store.quiz[quiz.kind] = quiz.score; save(); }
    const max = quiz.kind === 'where' ? ROUNDS * 1000 : ROUNDS;
    ctx.openInfo(`<div class="info-main quiz">
      <div class="meta">${icon('trophy')}<span>${esc(m.title)}</span></div>
      <h2>You scored <span class="num">${quiz.score}</span> of <span class="num">${max}</span></h2>
      <p class="summary">${quiz.score / max >= 0.8 ? 'Excellent: you know your world.' : quiz.score / max >= 0.5 ? 'Good work. Another round will lock it in.' : 'A good start. Explore the globe and try again.'}${isBest && best != null ? ' New personal best.' : best != null ? ` Your best is ${best}.` : ''}</p>
      <div class="actions"><button class="btn primary" data-again>${icon('refresh-cw')}Play again</button><button class="btn" data-qexit>Done</button></div></div>`, null);
    const root = document.getElementById('info-body');
    const kind = quiz.kind;
    root.querySelector('[data-again]').onclick = () => startQuiz(kind);
    root.querySelector('[data-qexit]').onclick = () => endQuiz(false);
  }

  return { renderPanel, startTour, startQuiz, active: () => !!(tour || quiz), stop: () => { endTour(); endQuiz(false); } };
}

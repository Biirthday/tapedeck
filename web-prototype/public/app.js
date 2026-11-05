const $ = (id) => document.getElementById(id);
const audio = $('audio');
const rail = $('rail');
const stage = $('stage');

const state = {
  tapes: [],
  scroll: 0,         // rendered position along the rail (in tapes)
  target: 0,         // where the scroll is easing to
  hover: -1,
  album: null,
  track: 0,
  lyrics: [],
};

const SPACING = { x: 74, y: -30, z: -125 };
const VISIBLE = { min: -2.6, max: 17 };
const pad = (n, w = 3) => String(n).padStart(w, '0');
const fmt = (s) => (Number.isFinite(s) ? `${pad(Math.floor(s / 60), 2)}:${pad(Math.floor(s % 60), 2)}` : '00:00');

function saved() {
  try { return JSON.parse(localStorage.getItem('tapedeck') || '{}'); } catch { return {}; }
}
function save(extra = {}) {
  try {
    localStorage.setItem('tapedeck', JSON.stringify({
      ...saved(), scroll: state.target, album: state.album?.id, track: state.track,
      volume: audio.volume, time: audio.currentTime, ...extra,
    }));
  } catch {}
}

/* ---------- Tapes ---------- */

function buildTape(album, i) {
  const el = document.createElement('div');
  el.className = 'tape';
  el.innerHTML = `
    <div class="face front">
      <i class="screw a"></i><i class="screw b"></i><i class="screw c"></i><i class="screw d"></i>
      <div class="label">
        <span class="ttl"></span>
        <span class="side-ab">A</span>
        <span class="year"></span>
      </div>
      <div class="window"><i class="reel"></i><i class="reel"></i></div>
      <div class="tape-bottom"><i></i><i></i></div>
    </div>
    <div class="face top"></div>
    <div class="face side"></div>`;
  el.querySelector('.ttl').textContent = album.album;
  el.querySelector('.year').textContent = album.year;
  el.addEventListener('pointerenter', () => setHover(i));
  el.addEventListener('pointerleave', () => state.hover === i && setHover(-1));
  el.addEventListener('click', (e) => {
    if (stage.dataset.dragged === '1') return;
    e.stopPropagation();
    loadAlbum(i, 0, true);
  });
  return { el, album, i, lift: 0, painted: false, shown: false };
}

// Shell tint taken from the cover, so tapes get the colored plastic look.
function paint(t) {
  t.painted = true;
  const src = `/cover/${t.album.id}`;
  t.el.querySelector('.label').style.backgroundImage = `url("${src}")`;
  const img = new Image();
  img.onload = () => {
    const c = document.createElement('canvas');
    c.width = c.height = 6;
    const ctx = c.getContext('2d', { willReadFrequently: true });
    ctx.drawImage(img, 0, 0, 6, 6);
    const d = ctx.getImageData(0, 0, 6, 6).data;
    let r = 0, g = 0, b = 0;
    for (let k = 0; k < d.length; k += 4) { r += d[k]; g += d[k + 1]; b += d[k + 2]; }
    const n = d.length / 4;
    const mix = (v) => Math.round((v / n) * 0.38 + 10);
    const shell = `rgb(${mix(r)}, ${mix(g)}, ${mix(b)})`;
    t.shell = shell;
    t.el.style.setProperty('--shell', shell);
    if (state.album?.id === t.album.id) $('mini').style.setProperty('--shell', shell);
  };
  img.src = src;
}

function setHover(i) {
  state.hover = i;
  const a = state.tapes[i]?.album || state.album;
  $('hoverTitle').textContent = a ? a.album : ' ';
  $('hoverArtist').textContent = a ? a.artist : ' ';
  if (i >= 0) setBackdrop(state.tapes[i].album.id);
  else if (state.album) setBackdrop(state.album.id);
}

let bgFront = 'bgA';
let bgId = null;
function setBackdrop(albumId) {
  if (albumId === bgId) return;
  bgId = albumId;
  const back = bgFront === 'bgA' ? 'bgB' : 'bgA';
  $(back).style.backgroundImage = `url("/cover/${albumId}")`;
  $(back).classList.add('on');
  $(bgFront).classList.remove('on');
  bgFront = back;
}

function frame() {
  state.scroll += (state.target - state.scroll) * 0.12;
  if (Math.abs(state.target - state.scroll) < 0.0005) state.scroll = state.target;

  for (const t of state.tapes) {
    const p = t.i - state.scroll;
    const visible = p > VISIBLE.min && p < VISIBLE.max;
    if (visible !== t.shown) {
      t.shown = visible;
      if (visible) rail.appendChild(t.el); else t.el.remove();
    }
    if (!visible) continue;
    if (!t.painted) paint(t);
    const isCurrent = state.album?.id === t.album.id;
    const goal = t.i === state.hover ? 1 : isCurrent ? 0.55 : 0;
    t.lift += (goal - t.lift) * 0.18;
    const fade = p > VISIBLE.max - 4 ? (VISIBLE.max - p) / 4 : p < -1.4 ? (p - VISIBLE.min) / 1.2 : 1;
    t.el.style.opacity = Math.max(0, Math.min(1, fade)).toFixed(3);
    t.el.style.transform =
      `translate3d(${p * SPACING.x}px, ${p * SPACING.y - t.lift * 46}px, ${p * SPACING.z}px) rotateY(-24deg) rotateX(${t.lift * -4}deg)`;
  }
  $('pos').textContent = pad(Math.min(state.tapes.length, Math.max(1, Math.round(state.scroll) + 2)));
  requestAnimationFrame(frame);
}

function scrollTo(v) {
  state.target = Math.max(-1.5, Math.min(state.tapes.length - 3, v));
}

/* ---------- Stage input ---------- */

stage.addEventListener('wheel', (e) => {
  e.preventDefault();
  scrollTo(state.target + (e.deltaY + e.deltaX) * 0.0045);
  clearTimeout(scrollSave);
  scrollSave = setTimeout(save, 400);
}, { passive: false });
let scrollSave;

let drag = null;
stage.addEventListener('pointerdown', (e) => {
  drag = { x: e.clientX, start: state.target, moved: false };
  stage.dataset.dragged = '0';
});
window.addEventListener('pointermove', (e) => {
  if (!drag) return;
  const dx = e.clientX - drag.x;
  if (Math.abs(dx) > 5) {
    drag.moved = true;
    stage.dataset.dragged = '1';
    stage.classList.add('dragging');
  }
  if (drag.moved) scrollTo(drag.start - dx / 90);
});
window.addEventListener('pointerup', () => {
  if (drag?.moved) save();
  drag = null;
  stage.classList.remove('dragging');
  setTimeout(() => { stage.dataset.dragged = '0'; }, 0);
});

// Type an artist's first letters to jump along the rail.
let typed = '';
let typedTimer;
function typeahead(ch) {
  typed += ch.toLowerCase();
  clearTimeout(typedTimer);
  typedTimer = setTimeout(() => { typed = ''; $('hint').textContent = 'CLICK TO LISTEN.'; }, 1200);
  $('hint').textContent = `JUMP: ${typed.toUpperCase()}_`;
  const hit = state.tapes.find((t) => t.album.artist.toLowerCase().startsWith(typed))
    || state.tapes.find((t) => t.album.album.toLowerCase().startsWith(typed));
  if (hit) { scrollTo(hit.i - 1); setHover(hit.i); }
}

window.addEventListener('keydown', (e) => {
  if (e.ctrlKey || e.metaKey || e.altKey) return;
  if (e.key === ' ') { e.preventDefault(); togglePlay(); flash('play'); }
  else if (e.key === 'ArrowRight' || e.key === 'ArrowDown') { e.preventDefault(); scrollTo(Math.round(state.target) + 1); }
  else if (e.key === 'ArrowLeft' || e.key === 'ArrowUp') { e.preventDefault(); scrollTo(Math.round(state.target) - 1); }
  else if (e.key === 'Enter' && state.hover >= 0) loadAlbum(state.hover, 0, true);
  else if (e.key === '.' || e.key === '>') { next(); flash('next'); }
  else if (e.key === ',' || e.key === '<') { prev(); flash('prev'); }
  else if (e.key.length === 1 && /\S/.test(e.key)) typeahead(e.key);
});

function flash(id) {
  $(id).classList.add('down');
  setTimeout(() => $(id).classList.remove('down'), 120);
}

/* ---------- Playback ---------- */

function loadAlbum(i, track = 0, autoplay = false, time = 0) {
  const t = state.tapes[i];
  if (!t) return;
  for (const o of state.tapes) o.el.classList.toggle('current', o === t);
  state.album = t.album;
  $('now').classList.add('loaded');
  $('nowAlbum').textContent = t.album.album;
  $('nowArtist').textContent = [t.album.artist, t.album.year].filter(Boolean).join(' · ');
  const list = $('tracks');
  list.innerHTML = '';
  t.album.tracks.forEach((tr, n) => {
    const li = document.createElement('li');
    li.innerHTML = '<b></b><span></span>';
    li.querySelector('b').textContent = `A${pad(n + 1, 2)}`;
    li.querySelector('span').textContent = tr.title;
    if (tr.lyrics) li.insertAdjacentHTML('beforeend', '<em class="lrc">LRC</em>');
    li.addEventListener('click', () => playTrack(n, true));
    list.appendChild(li);
  });

  const slot = $('slot');
  slot.classList.remove('loaded');
  $('miniLabel').style.backgroundImage = `url("/cover/${t.album.id}")`;
  $('mini').style.setProperty('--shell', t.shell || '#1b1b1b');
  requestAnimationFrame(() => requestAnimationFrame(() => slot.classList.add('loaded')));
  setBackdrop(t.album.id);
  if (state.hover < 0) setHover(-1);
  playTrack(track, autoplay, time);
}

async function playTrack(n, autoplay, time = 0) {
  const a = state.album;
  if (!a) return;
  state.track = (n + a.tracks.length) % a.tracks.length;
  const tr = a.tracks[state.track];
  audio.src = `/audio/${a.id}/${state.track}`;
  if (time) audio.addEventListener('loadedmetadata', () => { audio.currentTime = time; }, { once: true });
  [...$('tracks').children].forEach((li, k) => li.classList.toggle('on', k === state.track));
  $('tracks').children[state.track]?.scrollIntoView({ block: 'nearest' });
  $('dTrack').textContent = `A${pad(state.track + 1, 2)} ${tr.title}`;
  $('lyric').textContent = '';
  state.lyrics = [];
  if (autoplay) audio.play().catch(() => {});
  save();
  updateMediaSession(a, tr);
  if (tr.lyrics) {
    const text = await fetch(`/lyrics/${a.id}/${state.track}`).then((r) => r.text()).catch(() => '');
    if (state.album === a && a.tracks[state.track] === tr) state.lyrics = parseLrc(text);
  }
}

function parseLrc(text) {
  const out = [];
  for (const line of text.split('\n')) {
    const stamps = [...line.matchAll(/\[(\d+):(\d+(?:\.\d+)?)\]/g)];
    const words = line.replace(/\[[^\]]*\]/g, '').trim();
    for (const m of stamps) out.push({ t: Number(m[1]) * 60 + Number(m[2]), words });
  }
  return out.sort((x, y) => x.t - y.t);
}

function togglePlay() {
  if (!state.album) {
    const i = state.hover >= 0 ? state.hover : Math.max(0, Math.round(state.target) + 1);
    return loadAlbum(i, 0, true);
  }
  if (audio.paused) audio.play().catch(() => {}); else audio.pause();
}
function next() { if (state.album) playTrack(state.track + 1, true); }
function prev() {
  if (!state.album) return;
  if (audio.currentTime > 3) audio.currentTime = 0; else playTrack(state.track - 1, true);
}

$('play').addEventListener('click', togglePlay);
$('next').addEventListener('click', next);
$('prev').addEventListener('click', prev);
$('slot').addEventListener('click', () => {
  if (!state.album) return;
  const t = state.tapes.find((o) => o.album.id === state.album.id);
  if (t) scrollTo(t.i - 1);
});
$('progress').addEventListener('click', (e) => {
  const r = e.currentTarget.getBoundingClientRect();
  if (audio.duration) audio.currentTime = ((e.clientX - r.left) / r.width) * audio.duration;
});

audio.addEventListener('play', () => document.body.classList.add('playing'));
audio.addEventListener('pause', () => { document.body.classList.remove('playing'); save(); });
audio.addEventListener('ended', () => {
  if (state.track < state.album.tracks.length - 1) return next();
  // Side done: roll to the next tape on the rail.
  const t = state.tapes.find((o) => o.album.id === state.album.id);
  const nextTape = state.tapes[(t.i + 1) % state.tapes.length];
  scrollTo(nextTape.i - 1);
  loadAlbum(nextTape.i, 0, true);
});
let lastSave = 0;
audio.addEventListener('timeupdate', () => {
  $('dTime').textContent = fmt(audio.currentTime);
  $('progressBar').style.width = audio.duration ? `${(audio.currentTime / audio.duration) * 100}%` : '0';
  let line = '';
  for (const l of state.lyrics) { if (l.t <= audio.currentTime + 0.2) line = l.words; else break; }
  if ($('lyric').textContent !== line) $('lyric').textContent = line;
  if (performance.now() - lastSave > 5000) { lastSave = performance.now(); save(); }
});

function updateMediaSession(a, tr) {
  if (!('mediaSession' in navigator)) return;
  navigator.mediaSession.metadata = new MediaMetadata({
    title: tr.title, artist: a.artist, album: a.album,
    artwork: [{ src: `/cover/${a.id}`, sizes: '600x600', type: 'image/jpeg' }],
  });
}
if ('mediaSession' in navigator) {
  navigator.mediaSession.setActionHandler('play', () => audio.play());
  navigator.mediaSession.setActionHandler('pause', () => audio.pause());
  navigator.mediaSession.setActionHandler('nexttrack', next);
  navigator.mediaSession.setActionHandler('previoustrack', prev);
}

/* ---------- Volume knob + speaker ---------- */

const knob = $('knob');
function setVolume(v) {
  audio.volume = Math.max(0, Math.min(1, v));
  knob.style.setProperty('--rot', `${-135 + audio.volume * 270}deg`);
}
let knobDrag = null;
knob.parentElement.addEventListener('pointerdown', (e) => {
  knobDrag = { y: e.clientY, v: audio.volume };
  e.currentTarget.setPointerCapture(e.pointerId);
});
knob.parentElement.addEventListener('pointermove', (e) => {
  if (knobDrag) setVolume(knobDrag.v + (knobDrag.y - e.clientY) / 150);
});
knob.parentElement.addEventListener('pointerup', () => { knobDrag = null; save(); });
knob.parentElement.addEventListener('wheel', (e) => {
  e.preventDefault();
  setVolume(audio.volume - e.deltaY * 0.0008);
  save();
}, { passive: false });

// The grille breathes with the music. The graph is created on first play
// because browsers keep the AudioContext suspended until a user gesture.
let analyser;
audio.addEventListener('play', () => {
  if (analyser) return analyser.context.resume();
  const ctx = new AudioContext();
  analyser = ctx.createAnalyser();
  analyser.fftSize = 256;
  ctx.createMediaElementSource(audio).connect(analyser);
  analyser.connect(ctx.destination);
  const buf = new Uint8Array(analyser.frequencyBinCount);
  const grille = $('grille');
  const tick = () => {
    analyser.getByteFrequencyData(buf);
    let sum = 0;
    for (let k = 0; k < 24; k++) sum += buf[k];
    grille.style.setProperty('--level', audio.paused ? 0 : (sum / (24 * 255)) ** 1.5);
    requestAnimationFrame(tick);
  };
  tick();
});

/* ---------- Boot ---------- */

const library = await fetch('/api/library').then((r) => r.json());
state.tapes = library.map(buildTape);
$('count').textContent = pad(library.length);
$('total').textContent = pad(library.length);

const s = saved();
setVolume(s.volume ?? 0.8);
state.scroll = state.target = Math.min(s.scroll ?? -1, library.length - 3);
const resume = state.tapes.findIndex((t) => t.album.id === s.album);
if (resume >= 0) loadAlbum(resume, s.track || 0, false, s.time || 0);
requestAnimationFrame(frame);

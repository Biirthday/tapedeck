// Tapedeck: a cassette-style player for a local music library.
// No dependencies besides Node and ffmpeg/ffprobe on the PATH.
import http from 'node:http';
import fs from 'node:fs';
import fsp from 'node:fs/promises';
import path from 'node:path';
import os from 'node:os';
import crypto from 'node:crypto';
import { execFile, spawn } from 'node:child_process';
import { promisify } from 'node:util';

const run = promisify(execFile);
const ROOT = path.dirname(new URL(import.meta.url).pathname);
const MUSIC = path.resolve(process.env.MUSIC_DIR || path.join(os.homedir(), 'Music'));
const PORT = Number(process.env.PORT || 4747);
const CACHE = path.join(ROOT, '.cache');
const SKIP_DIRS = new Set(['1.lyrics']);
const AUDIO = new Set(['.flac', '.mp3', '.m4a', '.ogg', '.opus', '.wav']);
const COVER_NAMES = ['folder.jpg', 'cover.jpg', 'front.jpg', 'folder.png', 'cover.png'];
const MIME = {
  '.html': 'text/html; charset=utf-8', '.js': 'text/javascript', '.css': 'text/css',
  '.flac': 'audio/flac', '.mp3': 'audio/mpeg', '.ogg': 'audio/ogg', '.opus': 'audio/ogg',
  '.wav': 'audio/wav', '.jpg': 'image/jpeg', '.png': 'image/png', '.svg': 'image/svg+xml',
};

await fsp.mkdir(path.join(CACHE, 'covers'), { recursive: true });
await fsp.mkdir(path.join(CACHE, 'audio'), { recursive: true });

const id = (s) => crypto.createHash('sha1').update(s).digest('hex').slice(0, 12);

async function probeTags(file) {
  try {
    const { stdout } = await run('ffprobe', ['-v', 'error', '-show_entries', 'format_tags', '-of', 'json', file]);
    const tags = JSON.parse(stdout).format?.tags || {};
    const lower = {};
    for (const [k, v] of Object.entries(tags)) lower[k.toLowerCase()] = v;
    return lower;
  } catch {
    return {};
  }
}

async function walk(dir, out = []) {
  const entries = await fsp.readdir(dir, { withFileTypes: true });
  const files = [];
  for (const e of entries) {
    if (e.isDirectory()) {
      if (!SKIP_DIRS.has(e.name) && !e.name.startsWith('.')) await walk(path.join(dir, e.name), out);
    } else if (AUDIO.has(path.extname(e.name).toLowerCase())) {
      files.push(e.name);
    }
  }
  if (files.length) out.push({ dir, files: files.sort((a, b) => a.localeCompare(b, undefined, { numeric: true })) });
  return out;
}

function titleFromFile(name) {
  return path.basename(name, path.extname(name)).replace(/^\d+[\s.\-_]+/, '').replace(/_$/, '?');
}

let library = [];
const albumById = new Map();

async function scan() {
  const cacheFile = path.join(CACHE, 'library.json');
  let prev = {};
  try { prev = JSON.parse(await fsp.readFile(cacheFile, 'utf8')); } catch {}
  const groups = await walk(MUSIC);
  const albums = [];
  for (const { dir, files } of groups) {
    const rel = path.relative(MUSIC, dir);
    const albumId = id(rel);
    const key = `${rel}|${files.join('|')}`;
    let meta = prev[albumId]?.key === key ? prev[albumId].meta : null;
    if (!meta) {
      const tags = await probeTags(path.join(dir, files[0]));
      const parts = rel.split(path.sep);
      meta = {
        album: tags.album || parts.at(-1),
        artist: tags.album_artist || tags.albumartist || tags.artist || (parts.length > 1 ? parts[0] : 'Unknown'),
        year: (tags.originalyear || tags.date || '').slice(0, 4),
        genre: tags.genre || '',
      };
    }
    albums.push({
      id: albumId, key, rel, meta,
      tracks: files.map((f, i) => ({
        n: i,
        title: titleFromFile(f),
        file: f,
        lyrics: fs.existsSync(path.join(dir, path.basename(f, path.extname(f)) + '.lrc')),
      })),
    });
  }
  albums.sort((a, b) => a.meta.artist.localeCompare(b.meta.artist) || a.meta.year.localeCompare(b.meta.year));
  await fsp.writeFile(cacheFile, JSON.stringify(Object.fromEntries(albums.map((a) => [a.id, { key: a.key, meta: a.meta }]))));
  library = albums;
  albumById.clear();
  for (const a of albums) albumById.set(a.id, a);
  console.log(`[tapedeck] ${albums.length} tapes from ${MUSIC}`);
}

async function coverFor(album) {
  const dir = path.join(MUSIC, album.rel);
  for (const name of COVER_NAMES) {
    const p = path.join(dir, name);
    if (fs.existsSync(p)) return p;
  }
  const out = path.join(CACHE, 'covers', `${album.id}.jpg`);
  if (fs.existsSync(out)) return out;
  try {
    await run('ffmpeg', ['-v', 'error', '-y', '-i', path.join(dir, album.tracks[0].file), '-an', '-frames:v', '1', '-vf', 'scale=600:-1', out]);
    return out;
  } catch {
    return null;
  }
}

const transcoding = new Map();
async function playableFile(album, track) {
  const src = path.join(MUSIC, album.rel, track.file);
  if (path.extname(src).toLowerCase() !== '.m4a') return src;
  // ALAC in .m4a is not decodable by browsers; transcode once to FLAC.
  const out = path.join(CACHE, 'audio', `${album.id}-${track.n}.flac`);
  if (fs.existsSync(out)) return out;
  if (!transcoding.has(out)) {
    const tmp = `${out}.part`;
    transcoding.set(out, run('ffmpeg', ['-v', 'error', '-y', '-i', src, '-vn', '-c:a', 'flac', '-f', 'flac', tmp])
      .then(() => fsp.rename(tmp, out))
      .finally(() => transcoding.delete(out)));
  }
  await transcoding.get(out);
  return out;
}

async function sendFile(req, res, file, cacheable = false) {
  const stat = await fsp.stat(file);
  const type = MIME[path.extname(file).toLowerCase()] || 'application/octet-stream';
  const headers = { 'Content-Type': type, 'Accept-Ranges': 'bytes' };
  if (cacheable) headers['Cache-Control'] = 'max-age=86400';
  const range = /bytes=(\d*)-(\d*)/.exec(req.headers.range || '');
  if (range) {
    const start = range[1] ? Number(range[1]) : stat.size - Number(range[2]);
    const end = range[1] && range[2] ? Math.min(Number(range[2]), stat.size - 1) : stat.size - 1;
    if (start >= stat.size || start > end) {
      res.writeHead(416, { 'Content-Range': `bytes */${stat.size}` });
      return res.end();
    }
    res.writeHead(206, { ...headers, 'Content-Range': `bytes ${start}-${end}/${stat.size}`, 'Content-Length': end - start + 1 });
    return fs.createReadStream(file, { start, end }).pipe(res);
  }
  res.writeHead(200, { ...headers, 'Content-Length': stat.size });
  fs.createReadStream(file).pipe(res);
}

function json(res, data, status = 200) {
  res.writeHead(status, { 'Content-Type': 'application/json' });
  res.end(JSON.stringify(data));
}

const server = http.createServer(async (req, res) => {
  try {
    const url = new URL(req.url, 'http://x');
    const parts = url.pathname.split('/').filter(Boolean).map(decodeURIComponent);

    if (url.pathname === '/api/library') {
      return json(res, library.map(({ id, meta, tracks }) => ({
        id, ...meta, tracks: tracks.map(({ n, title, lyrics }) => ({ n, title, lyrics })),
      })));
    }
    if (url.pathname === '/api/rescan' && req.method === 'POST') {
      await scan();
      return json(res, { tapes: library.length });
    }

    const album = albumById.get(parts[1]);
    if (parts[0] === 'cover') {
      const file = album && await coverFor(album);
      if (!file) { res.writeHead(404); return res.end(); }
      return sendFile(req, res, file, true);
    }
    if (parts[0] === 'audio' || parts[0] === 'lyrics') {
      const track = album?.tracks[Number(parts[2])];
      if (!track) { res.writeHead(404); return res.end(); }
      if (parts[0] === 'audio') return sendFile(req, res, await playableFile(album, track));
      const lrc = path.join(MUSIC, album.rel, path.basename(track.file, path.extname(track.file)) + '.lrc');
      res.writeHead(200, { 'Content-Type': 'text/plain; charset=utf-8' });
      return fs.createReadStream(lrc).on('error', () => res.end('')).pipe(res);
    }

    const rel = url.pathname === '/' ? 'index.html' : url.pathname.slice(1);
    const file = path.join(ROOT, 'public', path.normalize(rel));
    if (!file.startsWith(path.join(ROOT, 'public')) || !fs.existsSync(file)) { res.writeHead(404); return res.end(); }
    return sendFile(req, res, file);
  } catch (err) {
    console.error(err);
    if (!res.headersSent) res.writeHead(500);
    res.end();
  }
});

await scan();
server.listen(PORT, '127.0.0.1', () => console.log(`[tapedeck] http://localhost:${PORT}`));

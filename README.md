# Tapedeck

Native cassette-wall player for a local music library, written in Rust
(macroquad for the window and 3D, rodio/symphonia for audio, lofty for tags).

Each album folder becomes a tape. Hover lifts a tape, click loads it into the deck:
it is pulled from the rail, shown, and flies into the deck, where it visibly winds
from one reel to the other as the track plays. The library can also be browsed as
a grid of covers (VIEW, top right, or Tab).

## Run

    cargo run --release
    # or install the binary into ~/.cargo/bin
    cargo install --path .

The library defaults to `~/Music` (folders named `1.lyrics` are skipped).
Set `MUSIC_DIR` to use another folder; F5 rescans it while running. Playback
state (tape, track, position, volume, view, queue, play counts) is saved in
`~/.config/tapedeck/state.json`.

On Linux the player registers over MPRIS (`org.mpris.MediaPlayer2.tapedeck`):
status bars show the track, artist and cover, and media keys or `playerctl`
control play/pause, next/previous, seeking and volume.

Formats: FLAC, MP3, ALAC/AAC (.m4a), Ogg Vorbis, WAV (Opus is not supported:
the bundled decoders have no Opus codec, so .opus files are not listed).

Playback is lossless up to the sound server: decoding matches ffmpeg sample for
sample for FLAC (16/24-bit, 44.1-96 kHz) and ALAC, and the output stream is
opened at each track's own rate and channel count in 32-bit float, so the
player never resamples or truncates. The deck display shows the file's format
and the path it takes, e.g. `FLAC 24/96  OUT 96K  PW>48K` when PipeWire's graph
runs at 48 kHz and converts. To let PipeWire follow the track's rate instead,
allow those rates (the DAC must support them, and other streams playing at the
same time keep the graph at their rate):

    # ~/.config/pipewire/pipewire.conf.d/rates.conf
    context.properties = { default.clock.allowed-rates = [ 44100 48000 88200 96000 ] }

Check tools: `cargo run --release --example verify_decode -- FILE...` compares
decoded PCM with ffmpeg; `--example verify_output -- FILE...` plays silently and
prints the format the sound server receives. `TAPEDECK_PROFILE=1` prints the
average time each stage of a frame takes, and the frame rate. Covers come from
`folder.jpg`/`cover.jpg` or the art embedded in the first track. Lyrics come
from `.lrc` files next to each track or from the matching path under
`1.lyrics` in the music library. Timed lyrics appear karaoke-style in the
stage's upper left. Scroll over the lyrics to browse and click a line to
seek to its time. Untimed lyrics can be scrolled for reading.
Short albums show their cover and a summary (tracks, running
time, genre) under the tracklist, and the deck has stereo VU meters. Fonts are taken from the system via
`fc-match` (JetBrains Mono, Noto Sans CJK).

## Controls

Press `?` for the shortcut sheet.

- Wheel / drag / Left and Right: move along the rail (hold an arrow to keep going)
- Up / Down: previous / next track of the tape in the deck; on a turned tape,
  pick a track on its J-card (Enter plays it). In the grid all four arrows move
  through albums
- Click or Enter: load the tape under the cursor. The playing tape leaves the
  rail for the deck; click its empty gap (or the deck's EJECT key) to return
  it and stop playback.
  Home brings the playing tape back into view
- Middle-click: queue a whole tape from the rail, or one track from the side
  list or a J-card. Queued tracks play before the tape continues; the UP NEXT
  badge shows what comes next. Click the badge to open the queue in the side
  list (click a track to play it now, drag it to change its place, right-click
  to drop it); its x clears it. Shift+middle-click puts it at the head of the
  queue, to play next. The queue is saved with the rest of the state
- Right-click: bring a tape forward and turn it around to read its J-card.
  Click a track to play it, or scroll over the card to page through long
  tracklists. Right-click again or press Esc to turn it back. In the grid,
  right-click a cover to open its tracklist.
- Space: play / pause, `,` and `.`: previous / next track, `[` and `]`: back /
  ahead 10 seconds, `-` and `=`: volume
- With an empty queue, finishing or skipping the last track starts the next
  tape in the rail's current order.
- Type to search artist, album, year, genre or track title: matching tapes rise,
  the rest sink into the dark. Left/Right (or Tab) step through the matches,
  Enter plays the focused one (from the matching track), Backspace edits,
  Esc clears. While searching, Space/`,`/`.` type instead of controlling the deck.
- Ruler under the rail: one tick per tape, colored by its cover. Click or drag
  to travel; it marks the visible stretch, the playing tape and search hits
- SORT (top right): by artist, year, most played, most recently played or cover
  color. Cover colors are cached in `~/.cache/tapedeck/colors.json` so the color
  sort is instant
- SFX (bottom right of the stage): mechanical deck sounds on key presses and when
  a tape goes in or out
- Mixtapes: click the blank tape at the head of the rail to start one and type
  its name. Right-click tracks in the side list to record them onto it (the
  REC badge shows the target; click it to stop). On a mixtape, right-click a
  track to take it off. Delete twice on a mixtape erases it. Mixtapes live in
  `~/.config/tapedeck/mixtapes.json`
- F11: toggle fullscreen (remembered for the next start)
- Deck keys: REW, PLAY (stays down while the tape runs), FF and EJECT
- Knob: volume; turn it around its center (clockwise raises) or scroll over it.
  Click or drag the progress bar to seek

`web-prototype/` holds the first browser version (Node + HTML), kept for reference.

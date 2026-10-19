#!/usr/bin/env bash
# Stages kuulo for the README screenshots and takes them.
#
#   stage.sh library   synthesize the sample library under the stage root (sox, ffmpeg)
#   stage.sh config    a scratch config, project, tags, marks and exports over that library
#   stage.sh run [settings]   launch the window on the staged project, or with the presets page open
#   stage.sh shot [name [title w h]]   float the running window at the capture size and write <name>.png here
#   stage.sh social    shot the running window at two to one and compose docs/brand/social-preview.png over it
#   stage.sh all       library, config, then hero.png, the social preview and presets.png
#
# A shot lands beside this script, or in $OUT. Everything else lives under $STAGE (default /tmp/kuulo-stage): a fake home whose paths read
# like a real setup, and XDG config, data and cache folders so the user's own kuulo config
# is never touched. The window is floated and captured through Hyprland and grim.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
STAGE=${STAGE:-/tmp/kuulo-stage}
home="$STAGE/home"
lib="$home/Audio/Pigeon Wizard"
game="$home/Games/Pigeon Wizard"
cli="$repo/target/release/kuulo-cli"
app="$repo/target/release/kuulo"
export XDG_CONFIG_HOME="$STAGE/xdg/config" XDG_DATA_HOME="$STAGE/xdg/data" XDG_CACHE_HOME="$STAGE/xdg/cache"

rate=48000
# Deterministic variation per file: v <seed> <min> <max> prints a number in the range.
v() { python3 -I -c "import sys,random; i,a,b=sys.argv[1:]; random.seed(int(i)*7919); print(round(random.uniform(float(a),float(b)),4))" "$@"; }

mono() { sox -n -r $rate -c 1 -b 24 "$@"; }
stereo() { sox -n -r $rate -c 2 -b 24 "$@"; }

library() {
  rm -rf "$lib"
  mkdir -p "$lib"/{Footsteps,Impacts,UI,Ambience,Voice,Music}
  local i
  for i in 1 2 3 4 5 6; do
    mono "$lib/Footsteps/footstep_gravel_0$i.wav" synth "$(v $i 0.22 0.34)" brownnoise highpass 150 lowpass "$(v $i 1800 3200)" fade h 0.004 0 "$(v $i 0.15 0.26)" gain "$(v $i -9 -4)"
  done
  for i in 1 2 3 4; do
    mono "$lib/Footsteps/footstep_wood_0$i.wav" synth "$(v 1$i 0.18 0.28)" brownnoise band "$(v 1$i 220 420)" 200 fade h 0.002 0 "$(v 1$i 0.12 0.2)" gain "$(v 1$i -6 -2)"
    mono "$lib/Footsteps/footstep_grass_0$i.wav" synth "$(v 2$i 0.3 0.42)" pinknoise highpass 400 lowpass 5000 fade t 0.03 0 "$(v 2$i 0.2 0.3)" gain "$(v 2$i -14 -9)"
  done
  for i in 1 2 3; do
    hit "$lib/Impacts/impact_metal_heavy_0$i.wav" 3$i 1.6 "$(v 3$i 2200 3600)"
  done
  for i in 1 2; do
    mono "$lib/Impacts/impact_wood_crate_0$i.wav" synth "$(v 4$i 0.5 0.7)" brownnoise band 300 300 fade h 0.002 0 "$(v 4$i 0.3 0.5)" gain -3
    mono "$lib/Impacts/debris_rubble_0$i.wav" synth "$(v 5$i 1.4 2.2)" brownnoise highpass 300 lowpass 2500 tremolo "$(v 5$i 9 14)" 70 fade t 0.05 0 "$(v 5$i 0.8 1.4)" gain -8
  done
  take "$lib/Impacts/sword_hits_take03.wav"
  for i in 1 2 3; do
    mono "$lib/UI/ui_click_soft_0$i.wav" synth 0.07 sine "$(v 6$i 1100 1700)" fade h 0.002 0 0.05 gain -8
  done
  mono "$lib/UI/ui_confirm.wav" synth 0.12 sine 880 : synth 0.18 sine 1320 fade h 0.003 0 0.1 gain -10
  mono "$lib/UI/ui_cancel.wav" synth 0.12 sine 660 : synth 0.18 sine 440 fade h 0.003 0 0.1 gain -10
  mono "$lib/UI/ui_hover.wav" synth 0.04 sine 2400 fade h 0.002 0 0.03 gain -16
  stereo "$lib/Ambience/forest_day_loop.wav" synth 42 pinknoise pinknoise lowpass 1200 tremolo 0.07 25 gain -18
  stereo "$lib/Ambience/wind_plateau.wav" synth 65 brownnoise brownnoise lowpass 500 tremolo 0.11 45 gain -12
  stereo -C 8 "$lib/Ambience/cave_drips_loop.flac" synth 30 pinknoise pinknoise lowpass 300 gain -24
  sox -n -r 44100 -c 2 "$lib/Ambience/rain_light.ogg" synth 50 whitenoise whitenoise highpass 2000 lowpass 9000 tremolo 0.2 10 gain -22
  voice "$lib/Voice/vo_guard_alert_01.wav" 71 7 125
  voice "$lib/Voice/vo_guard_alert_02.wav" 72 5 118
  voice "$lib/Voice/vo_merchant_greet_01.wav" 73 11 190
  voice "$lib/Voice/vo_merchant_greet_02.wav" 74 9 200
  voice "$lib/Voice/vo_player_hurt_01.wav" 75 2 150
  voice "$lib/Voice/vo_player_hurt_02.wav" 76 3 160
  mono "$lib/Music/stinger_victory.wav" synth 2.4 sine 261.6 sine 329.6 sine 392 sine 523.3 remix - fade t 0.01 0 1.8 gain -8
  mono "$lib/Music/stinger_defeat.wav" synth 3 sine 220 sine 261.6 sine 311.1 remix - fade t 0.4 0 2.4 gain -10
  music "$lib/Music/loop_explore_a_120bpm.wav"
  ffmpeg -loglevel error -y -i "$lib/Music/loop_explore_a_120bpm.wav" -codec:a libmp3lame -q:a 2 "$lib/Music/loop_explore_a_120bpm.mp3"
  rm "$lib/Music/loop_explore_a_120bpm.wav"
  echo "library: $(find "$lib" -type f | wc -l) files under $lib"
}

# One metallic hit: a bright noise burst over a ring that decays slower.
hit() {
  local out=$1 seed=$2 len=$3 ring=$4 tmp
  tmp=$(mktemp -d)
  mono "$tmp/burst.wav" synth "$len" whitenoise highpass 1500 fade l 0.001 0 "$len" gain -6
  mono "$tmp/ring.wav" synth "$len" sine "$ring" sine "$(v "$seed" 500 900)" remix - fade l 0.002 0 "$len" gain -9
  sox -m "$tmp/burst.wav" "$tmp/ring.wav" "$out" gain -n -1
  rm -r "$tmp"
}

# A take: four sword hits with room between them, stereo with a little width.
take() {
  local out=$1 tmp t i
  tmp=$(mktemp -d)
  i=0
  for t in 0.42 1.46 2.88 4.31; do
    i=$((i+1))
    hit "$tmp/h$i.wav" 8$i 1.3 "$(v 8$i 2600 3800)"
    sox "$tmp/h$i.wav" "$tmp/p$i.wav" pad "$t" gain "$(v 9$i -5 -1)"
  done
  sox -m "$tmp"/p1.wav "$tmp"/p2.wav "$tmp"/p3.wav "$tmp"/p4.wav "$tmp/mix.wav" pad 0 0.6
  sox "$tmp/mix.wav" -c 2 "$out" remix 1 1 delay 0 0.0007 gain -n -1
  rm -r "$tmp"
}

# Syllables of a buzzing tone at a loose rhythm: the envelope of a spoken line.
voice() {
  local out=$1 seed=$2 n=$3 pitch=$4 tmp k parts=()
  tmp=$(mktemp -d)
  for k in $(seq 1 "$n"); do
    mono "$tmp/s$k.wav" synth "$(v "$seed$k" 0.09 0.22)" sawtooth "$(v "$seed$k" "$((pitch-20))" "$((pitch+40))")" gain -14 lowpass 1400 fade t 0.02 0 0.04 pad 0 "$(v "$seed${k}9" 0.02 0.14)"
    parts+=("$tmp/s$k.wav")
  done
  sox "${parts[@]}" "$out" pad 0.1 0.3
  rm -r "$tmp"
}

# Eight bars at 120 bpm: a pulse and a bass line.
music() {
  local out=$1 tmp k parts=()
  tmp=$(mktemp -d)
  for k in $(seq 0 31); do
    local f=110
    case $((k % 8)) in 2|6) f=146.8;; 4) f=130.8;; 5) f=164.8;; esac
    mono "$tmp/n$k.wav" synth 0.5 square "$f" sine "$(python3 -I -c "print($f*2)")" remix - lowpass 900 fade t 0.01 0 0.35 gain -12
    parts+=("$tmp/n$k.wav")
  done
  sox "${parts[@]}" "$out"
  rm -r "$tmp"
}

config() {
  rm -rf "$STAGE/xdg" "$game"
  mkdir -p "$game"
  "$cli" projects new "$game/Pigeon Wizard.kuulo" --output "$game/Assets/Audio" >/dev/null
  "$cli" roots add "$lib" --name "Pigeon Wizard SFX" >/dev/null
  "$cli" scan >/dev/null
  # The default tags keep their colours; the stage's own two get fixed ones, so every run looks alike.
  "$cli" tags add combat --color red >/dev/null
  "$cli" tags add loop --color grey >/dev/null
  local f
  for f in "$lib"/Footsteps/*.wav; do "$cli" tag "$f" +sfx +foley >/dev/null; done
  for f in "$lib"/Impacts/*.wav; do "$cli" tag "$f" +sfx >/dev/null; done
  "$cli" tag "$lib/Impacts/sword_hits_take03.wav" +combat >/dev/null
  "$cli" tag "$lib/Impacts/impact_metal_heavy_01.wav" +combat >/dev/null
  "$cli" tag "$lib/Impacts/impact_metal_heavy_02.wav" +combat >/dev/null
  for f in "$lib"/UI/*.wav; do "$cli" tag "$f" +ui >/dev/null; done
  for f in "$lib"/Ambience/*; do "$cli" tag "$f" +ambient >/dev/null; done
  "$cli" tag "$lib/Ambience/forest_day_loop.wav" +loop >/dev/null
  "$cli" tag "$lib/Ambience/cave_drips_loop.flac" +loop >/dev/null
  for f in "$lib"/Voice/*.wav; do "$cli" tag "$f" +voice >/dev/null; done
  for f in "$lib"/Music/*; do "$cli" tag "$f" +music >/dev/null; done
  "$cli" tag "$lib/Music/loop_explore_a_120bpm.mp3" +loop >/dev/null
  for f in Impacts/sword_hits_take03.wav Impacts/impact_metal_heavy_02.wav Footsteps/footstep_gravel_03.wav UI/ui_confirm.wav Ambience/forest_day_loop.wav Voice/vo_guard_alert_01.wav; do
    "$cli" fav "$lib/$f" >/dev/null
  done
  find "$lib" -type f | while read -r f; do "$cli" analyze "$f" --store >/dev/null; done
  "$cli" export "$lib/Footsteps/footstep_gravel_03.wav" --preset sfx --into sfx/foley >/dev/null
  "$cli" export "$lib/UI/ui_confirm.wav" --preset sfx --into ui >/dev/null
  "$cli" export "$lib/Impacts/impact_metal_heavy_02.wav" --preset sfx --into sfx/combat >/dev/null
  "$cli" config --default-preset sfx --ui-zoom 1.0 >/dev/null
  echo "config: project $game/Pigeon Wizard.kuulo, xdg under $STAGE/xdg"
}

# The window the user had focused, to give focus back to after the stage touches theirs.
active() { hyprctl activewindow -j | python3 -I -c "import json,sys; print(json.load(sys.stdin).get('address',''))"; }
refocus() { [ -n "$1" ] && hyprctl eval "hl.dispatch(hl.dsp.focus({ window = 'address:$1' }))" >/dev/null || true; }

run() {
  local extra=() prev title=kuulo
  [ "${1:-}" = settings ] && extra=(--settings presets) && title="kuulo settings"
  prev=$(active)
  pkill -x kuulo 2>/dev/null || true
  sleep 0.3
  HOME="$home" "$app" --project "$game/Pigeon Wizard.kuulo" --open "$lib/Impacts/sword_hits_take03.wav" --in 1.43 --out 2.62 --fade-in 0.008 --fade-out 0.18 "${extra[@]}" >/dev/null 2>&1 &
  disown
  window "$title" >/dev/null || true
  refocus "$prev"
}

# The address of the kuulo window with this title, once Hyprland lists it.
window() {
  local title=$1 i
  for i in $(seq 1 60); do
    hyprctl clients -j | python3 -I -c "import json,sys; c=[x for x in json.load(sys.stdin) if x['class']=='kuulo' and x['title']==sys.argv[1]]; print(c[0]['address']) if c else sys.exit(1)" "$title" && return
    sleep 0.25
  done
  echo "no kuulo window titled $title" >&2; return 1
}

# The capture goes on the monitor with the highest scale, so the pixels are the crispest
# this machine draws: the window is centred there by absolute position, snapped to a
# multiple of four logical pixels so the position is whole at any scale up to 1.25 and
# grim's region starts on the window's own first pixel.
shot() {
  local name=${1:-hero} title=${2:-kuulo} w=${3:-1500} h=${4:-900} addr geo d pos prev
  prev=$(active)
  addr=$(window "$title")
  pos=$(hyprctl monitors -j | python3 -I -c "import json,sys; m=max(json.load(sys.stdin), key=lambda m: m['scale']); w,h=m['width']/m['scale'],m['height']/m['scale']; print(*(int(c)//4*4 for c in (m['x']+(w-$w)/2, m['y']+(h-$h)/2)))")
  for d in "float({ window = 'address:$addr', action = 'on' })" \
           "resize({ window = 'address:$addr', exact = true, x = $w, y = $h })" \
           "move({ window = 'address:$addr', x = ${pos% *}, y = ${pos#* } })" \
           "set_prop({ window = 'address:$addr', prop = 'border_size', value = '0' })" \
           "set_prop({ window = 'address:$addr', prop = 'rounding', value = '0' })"; do
    hyprctl eval "hl.dispatch(hl.dsp.window.$d)" >/dev/null
  done
  hyprctl eval "hl.dispatch(hl.dsp.cursor.move({ x = ${pos% *}, y = $(( ${pos#* } - 40 )) }))" >/dev/null
  # Focus draws a line inside the window; the user's own window keeps it for the capture.
  refocus "$prev"
  sleep 2
  geo=$(hyprctl clients -j | python3 -I -c "import json,sys; c=[x for x in json.load(sys.stdin) if x['address']=='$addr'][0]; print(f\"{c['at'][0]},{c['at'][1]} {c['size'][0]}x{c['size'][1]}\")")
  grim -g "$geo" "${OUT:-$here}/$name.png"
  echo "shot: ${OUT:-$here}/$name.png ($geo)"
}

# The repository's social card: the window at two to one, faded into the background
# along a diagonal from the top left corner, with the logo and tagline on the fade.
# The layout is in pixels of a 2000 wide shot and scales with the shot.
social() {
  OUT=$STAGE shot social kuulo 1600 800 >/dev/null
  python3 -I - "$STAGE/social.png" "$repo" <<'EOF'
import sys, tomllib
import numpy as np
from PIL import Image, ImageDraw, ImageFont
shot, repo = sys.argv[1:]
img = Image.open(shot).convert('RGB'); W, H = img.size; k = W / 2000
ys, xs = np.mgrid[0:H, 0:W]
a = np.clip((1.25 - (xs / W + ys / H)) / (1.25 - 0.62), 0, 1); a = a * a * (3 - 2 * a)
rgb = np.asarray(img, dtype=np.float32)
card = Image.fromarray((rgb * (1 - a[..., None]) + np.float32([22, 22, 22]) * a[..., None]).astype(np.uint8)).convert('RGBA')
logo = Image.open(f'{repo}/docs/brand/kuulo-logo.png').convert('RGBA')
lh = round(120 * k); logo = logo.resize((round(logo.width * lh / logo.height), lh), Image.LANCZOS)
card.alpha_composite(logo, (round(84 * k), round(80 * k)))
fonts = f'{repo}/crates/kuulo/assets/fonts/'
head = ImageFont.truetype(fonts + 'Inter-SemiBold.ttf', round(60 * k))
sub = ImageFont.truetype(fonts + 'Inter-Regular.ttf', round(31 * k))
d = ImageDraw.Draw(card)
d.text((round(90 * k), round(240 * k)), 'Listen fast, export clean.', font=head, fill=(232, 230, 226))
d.text((round(90 * k), round(326 * k)), 'A local sound library browser for game audio,', font=sub, fill=(165, 165, 165))
d.text((round(90 * k), round(368 * k)), 'film, theatre and podcasts. Free software, GPL.', font=sub, fill=(165, 165, 165))
# Until 1.0 the card announces the version, on the wordmark's baseline (the logo pads its ink by 24 of 262).
version = tomllib.load(open(f'{repo}/Cargo.toml', 'rb'))['workspace']['package']['version']
if version.startswith('0.'):
    d.text((round(84 * k) + logo.width + round(28 * k), round(80 * k + lh * (1 - 24 / 262))), f'v{version}', font=ImageFont.truetype(fonts + 'Inter-Regular.ttf', round(36 * k)), fill=(165, 165, 165), anchor='ls')
card.convert('RGB').save(f'{repo}/docs/brand/social-preview.png', optimize=True)
print(f'social: {repo}/docs/brand/social-preview.png ({W}x{H})')
EOF
}

case ${1:-all} in
  library) library ;;
  config) config ;;
  run) run "${2:-}" ;;
  shot) shot "${2:-hero}" "${3:-kuulo}" "${4:-1500}" "${5:-900}" ;;
  social) social ;;
  all) library; config; run; shot hero; social; run settings; shot presets "kuulo settings" 960 820 ;;
  *) sed -n 2,12p "$0" ;;
esac

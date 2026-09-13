#!/usr/bin/env bash
# 最小复现：往 Konsole 打一张高 sixel，再往它**底部几行**写字，看整张图会怎样。
#
# 这是「REPL 打完图之后把输入框画回屏幕底部」的等价动作。要回答的问题是：
# 覆盖图的最后几行，Konsole 是只擦那几行，还是把整张图丢掉。
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
WORK=/home/shorin/.cache/miyu-chafa-overlap
IMG=/home/shorin/.cache/miyu-chafa-sandbox/images/tall.png
DISP=:99

rm -rf "$WORK"; mkdir -p "$WORK"; cd "$WORK"
pkill -f "Xvfb $DISP" 2>/dev/null; sleep 1
Xvfb "$DISP" -screen 0 1242x1340x24 >/dev/null 2>&1 &
XVFB=$!
sleep 2

mkdir -p ~/.local/share/konsole
cat > ~/.local/share/konsole/ChafaProbe.profile <<'PROFILE'
[Appearance]
ColorScheme=Breeze
Font=Monospace,11,-1,5,50,0,0,0,0,0
[General]
Name=ChafaProbe
Parent=FALLBACK/
[Terminal Features]
EnableSixelRendering=true
PROFILE

cat > "$WORK/inner.sh" <<INNER
#!/bin/sh
printf '\033[2J\033[H'
i=0; while [ \$i -lt 36 ]; do echo; i=\$((i+1)); done
chafa --polite on --size 60x30 "$IMG"
touch "$WORK/step1"
while [ ! -f "$WORK/go2" ]; do sleep 0.2; done
# 模拟 REPL 的 tail 重绘：在屏幕底部 5 行清行 + 写字
printf '\033[62;1H\033[2K tail 1'
printf '\033[63;1H\033[2K tail 2'
printf '\033[64;1H\033[2K tail 3'
printf '\033[65;1H\033[2K tail 4'
printf '\033[66;1H\033[2K tail 5'
touch "$WORK/step2"
while [ ! -f "$WORK/done" ]; do sleep 0.2; done
INNER
chmod +x "$WORK/inner.sh"

env -u KITTY_WINDOW_ID -u KITTY_PID -u KITTY_INSTALLATION_DIR -u TERM_PROGRAM \
  DISPLAY=$DISP konsole --profile ChafaProbe --hide-menubar --hide-tabbar \
  -e "$WORK/inner.sh" >/dev/null 2>&1 &

for _ in $(seq 1 60); do [ -f "$WORK/step1" ] && break; sleep 0.5; done
sleep 2
DISPLAY=$DISP import -window root "$WORK/1-图刚打完.png" 2>/dev/null
touch "$WORK/go2"
for _ in $(seq 1 40); do [ -f "$WORK/step2" ] && break; sleep 0.5; done
sleep 2
DISPLAY=$DISP import -window root "$WORK/2-底部5行被覆盖后.png" 2>/dev/null
touch "$WORK/done"
sleep 1
pkill -f "konsole --profile ChafaProbe" 2>/dev/null
kill $XVFB 2>/dev/null

echo "两张截图："
for f in "$WORK"/*.png; do
  printf '  %-40s ' "$(basename "$f")"
  magick "$f" -format '%wx%h  非黑像素占比 %[fx:mean]\n' info: 2>/dev/null || echo
done

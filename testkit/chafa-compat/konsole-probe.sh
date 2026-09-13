#!/usr/bin/env bash
# 无头 Konsole 里复现「图画出来又被擦」：relay.py 录字节，截图看屏幕。
#
# 跑在 Xvfb 上，不占用你正在用的桌面。产物：
#   ~/.cache/miyu-chafa-probe/relay-out.log   程序→终端 的全部字节
#   ~/.cache/miyu-chafa-probe/shot-*.png      不同时刻的屏幕
#
# 用法：konsole-probe.sh [图片名，默认 tall.png]
set -uo pipefail
IMG="${1:-tall.png}"
HERE="$(cd "$(dirname "$0")" && pwd)"
WORK=/home/shorin/.cache/miyu-chafa-probe
SB=/home/shorin/.cache/miyu-chafa-sandbox/miyu-sb
DISP=:99

rm -rf "$WORK"; mkdir -p "$WORK"; cd "$WORK"

pkill -f "Xvfb $DISP" 2>/dev/null
Xvfb "$DISP" -screen 0 1400x1100x24 >/dev/null 2>&1 &
XVFB=$!
sleep 2

# Konsole 的 sixel 支持要在 profile 里开，这里造一份专用 profile
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

# 清掉 kitty 的环境痕迹：留着的话 chafa 会以为自己在 kitty 里，选 kitty 格式而不是 sixel
env -u KITTY_WINDOW_ID -u KITTY_PID -u KITTY_INSTALLATION_DIR -u KITTY_PUBLIC_KEY \
    -u TERM_PROGRAM -u KITTY_LISTEN_ON \
  DISPLAY=$DISP konsole --profile ChafaProbe --hide-menubar --hide-tabbar \
  -e "$HERE/relay.py" --send-after 6 "显示 /home/shorin/.cache/miyu-chafa-sandbox/images/$IMG" \
     --quit-after 50 "$SB" normal >/dev/null 2>&1 &
KONSOLE=$!

for t in 1 2 3 4 5 6; do

  sleep 6
  DISPLAY=$DISP import -window root "$WORK/shot-$t.png" 2>/dev/null && echo "拍了 shot-$t.png"
done

wait $KONSOLE 2>/dev/null
kill $XVFB 2>/dev/null

echo "=== 录到的字节 ==="
ls -la "$WORK"/relay-*.log 2>/dev/null
echo "=== 事件时间线 ==="
cd "$WORK" && python3 "$HERE/relay_report.py" 2>&1 | head -60

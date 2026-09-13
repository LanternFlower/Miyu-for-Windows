#!/usr/bin/env bash
# 在无头 Konsole 里分阶段复现，每一步拍一张，看图是被哪一步弄没的。
#
# 阶段（对应 Miyu 打完图之后真实发出的字节，见 relay-out.log）：
#   1  光标推到屏幕中部，打一张 30 行高的 sixel
#   2  跳到最后一行滚 5 行            —— resume_at 的 overflow 分支
#   3  设 DECSTBM 1..N-6 + 清行 + 复位 —— tail 每 30ms 重绘一次的动作
#   4  重复阶段 3 五次                 —— 真实情况下它一直在刷
#
# Qt 会优先连 Wayland，必须把 WAYLAND_DISPLAY 摘掉并逼它走 xcb，
# 否则窗口开在你的真实桌面上、Xvfb 里拍到的是一片黑。
set -u
IMG=${1:-/home/shorin/.cache/miyu-chafa-sandbox/images/tall.png}
WORK=/home/shorin/.cache/miyu-chafa-stages
DISP=:99
rm -rf "$WORK"; mkdir -p "$WORK"

pkill -f "Xvfb $DISP" >/dev/null 2>&1; sleep 1
Xvfb "$DISP" -screen 0 1242x1340x24 >/dev/null 2>&1 &
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
W="$WORK"
rows=\$(tput lines); bottom=\$((rows-6))
printf '\033[2J\033[H'
# 推到屏幕中部，让图的下沿正好压到底部 —— 这正是 REPL 里的情形
i=0; while [ \$i -lt \$((rows-31)) ]; do echo; i=\$((i+1)); done
chafa --polite on --size 60x30 "$IMG"
echo "\$rows" > "\$W/rows"; touch "\$W/s1"; while [ ! -f "\$W/g2" ]; do sleep 0.2; done

printf '\033[%d;1H\n\n\n\n\n' "\$rows"          # 阶段2：底部滚 5 行
touch "\$W/s2"; while [ ! -f "\$W/g3" ]; do sleep 0.2; done

printf '\033[1;%dr' "\$bottom"                  # 阶段3：设滚动区
printf '\033[%d;1H\033[2K' "\$bottom"
printf '\033[r'
touch "\$W/s3"; while [ ! -f "\$W/g4" ]; do sleep 0.2; done

i=0                                              # 阶段4：像 tail 那样反复刷
while [ \$i -lt 5 ]; do
  printf '\033[1;%dr\033[%d;1H\033[2K\033[r' "\$bottom" "\$bottom"
  i=\$((i+1)); sleep 0.1
done
touch "\$W/s4"; while [ ! -f "\$W/done" ]; do sleep 0.2; done
INNER
chmod +x "$WORK/inner.sh"

env -u WAYLAND_DISPLAY -u KITTY_WINDOW_ID -u KITTY_PID -u KITTY_INSTALLATION_DIR \
    -u TERM_PROGRAM QT_QPA_PLATFORM=xcb DISPLAY=$DISP \
  dbus-run-session -- konsole --profile ChafaProbe --hide-menubar --hide-tabbar \
  -p TerminalColumns=138 -p TerminalRows=67 \
  -e "$WORK/inner.sh" >"$WORK/konsole.log" 2>&1 &

shoot() {  # $1 阶段名  $2 等待的标记
  for _ in $(seq 1 60); do [ -f "$WORK/$2" ] && break; sleep 0.5; done
  sleep 1.5
  DISPLAY=$DISP import -window root "$WORK/$1.png" 2>/dev/null
  printf '%-24s ' "$1"
  magick "$WORK/$1.png" -format '%wx%h  亮度均值 %[fx:mean]\n' info: 2>/dev/null || echo "(截图失败)"
}

shoot "1-图刚打完" s1; touch "$WORK/g2"
shoot "2-滚5行后"  s2; touch "$WORK/g3"
shoot "3-设滚动区清行后" s3; touch "$WORK/g4"
shoot "4-反复刷tail后" s4; touch "$WORK/done"

sleep 1
pkill -f "konsole --profile ChafaProbe" >/dev/null 2>&1
pkill -f "Xvfb $DISP" >/dev/null 2>&1
echo "终端 $(cat "$WORK/rows" 2>/dev/null) 行；截图在 $WORK"
exit 0

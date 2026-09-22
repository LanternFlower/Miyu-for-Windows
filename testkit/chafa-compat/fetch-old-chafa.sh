#!/usr/bin/env bash
# 从 Arch 归档拉历史版本的 chafa 真二进制，供 matrix.py 做版本 × 参数矩阵。
#
# 为什么要真二进制：chafa 的选项随版本增减，认不出的选项会让它退出码 2、
# 一张图都不出。开发机是 Arch（永远是最新版），这个坑在本机 100% 复现不了。
# 这几个版本对应真实发行版：
#   1.12.4  Debian 12 / openSUSE Leap 15.5
#   1.14.5  Debian 13 / Ubuntu 24.04+ / Fedora 41 / Alpine 3.21
#   1.16.2  Alpine 3.22
#   1.18.0  Alpine 3.23（差一个补丁版本就没有 --probe-mode）
set -euo pipefail
cd "$(dirname "$0")"
mkdir -p old
cd old

ARCHIVE=https://archive.archlinux.org/packages
for pkg in chafa-1.12.4-3 chafa-1.14.5-1 chafa-1.16.2-1 chafa-1.18.0-1; do
  file="${pkg}-x86_64.pkg.tar.zst"
  [ -f "$file" ] || curl -sL --max-time 180 -o "$file" "$ARCHIVE/c/chafa/$file"
done
# 1.14 起链接 libjxl.so.0.11，本机通常是更新的 soname，一并拉一份。
[ -f libjxl.pkg.tar.zst ] || curl -sL --max-time 180 -o libjxl.pkg.tar.zst \
  "$ARCHIVE/l/libjxl/libjxl-0.11.2-2-x86_64.pkg.tar.zst"
mkdir -p jxl && tar --zstd -xf libjxl.pkg.tar.zst -C jxl usr/lib

for file in chafa-*.pkg.tar.zst; do
  ver="${file#chafa-}"; ver="${ver%%-*}"
  rm -rf "v$ver"; mkdir -p "v$ver"
  tar --zstd -xf "$file" -C "v$ver" usr/bin/chafa usr/lib
  printf '%-9s ' "$ver"
  LD_LIBRARY_PATH="$PWD/v$ver/usr/lib:$PWD/jxl/usr/lib" "./v$ver/usr/bin/chafa" --version | head -1
done

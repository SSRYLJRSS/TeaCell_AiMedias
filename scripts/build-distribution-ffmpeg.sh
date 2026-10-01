#!/usr/bin/env bash
# GPL-3.0-or-later. Build local-file FFmpeg tools solely from the accompanying fixed sources.
set -eo pipefail
inputs=$(cd "$1" && pwd)
mkdir -p "$2"
output=$(cd "$2" && pwd)
prefix="$output/ffmpeg-prefix"
mkdir -p "$prefix"
export PKG_CONFIG_PATH="$prefix/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
export PKG_CONFIG_LIBDIR="$prefix/lib/pkgconfig"
export CFLAGS="-O2 -I$prefix/include"
export LDFLAGS="-L$prefix/lib"
export CC=${CC:-cc}
export CXX=${CXX:-c++}
jobs=4
cmake_bin=${CMAKE_COMMAND:-cmake}
source_dir() {
  local directory
  directory=$(find "$inputs/sources" -mindepth 1 -maxdepth 1 -type d -iname "$1-*" | head -n 1)
  test -n "$directory" || { echo "Missing source directory: $1" >&2; return 1; }
  printf '%s\n' "$directory"
}
windows=false
case "$(uname -s)" in MINGW*|MSYS*) windows=true; export CC=gcc CXX=g++; export LDFLAGS="$LDFLAGS -static -static-libgcc -static-libstdc++" ;; esac

cd "$(source_dir zlib)"
./configure --static --prefix="$prefix"
make -j"$jobs" && make install
for name in ogg vorbis; do
  "$cmake_bin" -S "$(source_dir "$name")" -B "$output/build-$name" -G Ninja -DCMAKE_BUILD_TYPE=Release \
    -DCMAKE_POLICY_VERSION_MINIMUM=3.5 -DCMAKE_INSTALL_PREFIX="$prefix" -DCMAKE_INSTALL_LIBDIR=lib \
    -DCMAKE_PREFIX_PATH="$prefix" -DBUILD_SHARED_LIBS=OFF -DBUILD_TESTING=OFF -DINSTALL_DOCS=OFF
  "$cmake_bin" --build "$output/build-$name" --parallel "$jobs"
  "$cmake_bin" --install "$output/build-$name"
done
# Upstream Vorbis CMake omits libm in the generated static pkg-config metadata.
# Preserve the explicit system math link dependency when FFmpeg probes static archives.
if ! $windows; then
  printf '\nLibs.private: -lm\n' >> "$prefix/lib/pkgconfig/vorbis.pc"
fi
cd "$(source_dir x264)"
host=()
if $windows; then host=(--host=x86_64-w64-mingw32); fi
./configure --prefix="$prefix" --enable-static --enable-pic --disable-cli --disable-lavf --disable-swscale "${host[@]}"
make -j"$jobs" && make install
cd "$(source_dir libvpx)"
vpx_target=()
if $windows; then vpx_target=(--target=x86_64-win64-gcc); fi
./configure --prefix="$prefix" --enable-static --disable-shared --enable-pic --disable-examples --disable-tools \
  --disable-unit-tests --disable-docs "${vpx_target[@]}"
make -j"$jobs" && make install
meson setup "$output/build-dav1d" "$(source_dir dav1d)" --prefix "$prefix" --libdir lib --default-library static \
  --buildtype release -Denable_tools=false -Denable_tests=false
meson compile -C "$output/build-dav1d" -j "$jobs"
meson install -C "$output/build-dav1d"
cd "$(source_dir ffmpeg)"
platform=()
if $windows; then platform=(--target-os=mingw32 --arch=x86_64 --cc=gcc --cxx=g++ --enable-w32threads); fi
./configure --prefix="$prefix" --pkg-config-flags=--static --extra-cflags="$CFLAGS" --extra-ldflags="$LDFLAGS" \
  --enable-gpl --enable-version3 --enable-static --disable-shared --disable-autodetect --disable-debug --disable-doc \
  --disable-ffplay --disable-network --disable-indevs --disable-outdevs --enable-indev=lavfi \
  --enable-libx264 --enable-libvpx --enable-libvorbis --enable-libdav1d --enable-zlib "${platform[@]}"
make -j"$jobs" && make install
mkdir -p "$output/ffmpeg/bin"
suffix=""
if $windows; then suffix=.exe; fi
cp "$prefix/bin/ffmpeg$suffix" "$prefix/bin/ffprobe$suffix" "$output/ffmpeg/bin/"
cp COPYING.GPLv3 "$output/ffmpeg/LICENSE.txt"
cp ffbuild/config.log "$output/ffmpeg-config.log"
"$output/ffmpeg/bin/ffmpeg$suffix" -buildconf > "$output/ffmpeg-buildconf.txt" 2>&1

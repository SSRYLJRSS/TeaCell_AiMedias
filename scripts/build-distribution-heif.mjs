#!/usr/bin/env node
/* global process, console */
/** Build all statically linked HEIF components from the accompanying sources; no vcpkg downloads. */
import { cpSync, existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { spawnSync } from "node:child_process";

const inputs = resolve(process.argv[2]);
const output = resolve(process.argv[3]);
const manifest = JSON.parse(readFileSync(join(inputs, "source-manifest.json"), "utf8"));
const source = (name) => join(inputs, "sources", manifest.sources.find((entry) => entry.name === name).sourceDirectory);
const prefix = join(output, "heif");
mkdirSync(prefix, { recursive: true });
const windows = process.platform === "win32";
const common = ["-DCMAKE_BUILD_TYPE=Release", "-DCMAKE_POLICY_VERSION_MINIMUM=3.5", `-DCMAKE_INSTALL_PREFIX=${prefix}`,
  "-DCMAKE_INSTALL_LIBDIR=lib", "-DCMAKE_POSITION_INDEPENDENT_CODE=ON", "-DCMAKE_MSVC_RUNTIME_LIBRARY=MultiThreadedDLL"];
const calls = [];
function cmake(args) {
  calls.push(args);
  console.log(`cmake ${args.join(" ")}`);
  const run = spawnSync(process.env.CMAKE_COMMAND ?? "cmake", args, { stdio: "inherit", windowsHide: true });
  if (run.status !== 0 || run.error) throw new Error(`CMake failed: ${run.error?.message ?? run.status}`);
}
function build(name, directory, flags) {
  const buildDir = join(output, `build-${name}`);
  cmake(["-S", directory, "-B", buildDir, "-G", "Ninja", ...(windows ? ["-DCMAKE_C_COMPILER=cl", "-DCMAKE_CXX_COMPILER=cl"] : []), ...common, ...flags]);
  cmake(["--build", buildDir, "--config", "Release", "--parallel", "4"]);
  cmake(["--install", buildDir, "--config", "Release"]);
}
build("de265", source("libde265"), ["-DBUILD_SHARED_LIBS=OFF", ...(windows ? ["-DFORCE_FULL_VISIBILITY=ON"] : []), "-DENABLE_SDL=OFF", "-DENABLE_DECODER=OFF", "-DENABLE_ENCODER=OFF",
  "-DENABLE_SHERLOCK265=OFF", "-DENABLE_INTERNAL_DEVELOPMENT_TOOLS=OFF", "-DENABLE_AVX512=OFF", "-DENABLE_AVX2=OFF"]);
build("x265", join(source("x265"), "source"), ["-DENABLE_SHARED=OFF", "-DENABLE_CLI=OFF", "-DENABLE_ASSEMBLY=OFF", "-DENABLE_LIBNUMA=OFF"]);
const de265 = join(prefix, "lib", windows ? "libde265.lib" : "libde265.a");
const x265 = join(prefix, "lib", windows ? "x265-static.lib" : "libx265.a");
if (windows && !existsSync(x265) && existsSync(join(prefix, "lib", "x265.lib"))) cpSync(join(prefix, "lib", "x265.lib"), x265);
if (!existsSync(de265) || !existsSync(x265)) throw new Error("Native codec archive names differ from the linker contract");
build("heif", source("libheif"), ["-DBUILD_SHARED_LIBS=OFF", `-DCMAKE_PREFIX_PATH=${prefix}`,
  ...(windows ? ["-DCMAKE_CXX_FLAGS=-DLIBDE265_STATIC_BUILD"] : []),
  `-DLIBDE265_INCLUDE_DIR=${join(prefix, "include")}`, `-DLIBDE265_LIBRARY=${de265}`,
  `-DX265_INCLUDE_DIR=${join(prefix, "include")}`, `-DX265_LIBRARY=${x265}`,
  "-DWITH_LIBDE265=ON", "-DWITH_LIBDE265_PLUGIN=OFF", "-DWITH_X265=ON", "-DWITH_X265_PLUGIN=OFF",
  ...["KVAZAAR", "UVG266", "VVDEC", "VVENC", "X264", "OpenH264_DECODER", "DAV1D", "AOM_DECODER", "AOM_ENCODER", "SvtEnc", "RAV1E", "JPEG_DECODER", "JPEG_ENCODER", "OpenJPEG_ENCODER", "OpenJPEG_DECODER", "FFMPEG_DECODER", "OPENJPH_ENCODER"].map((name) => `-DWITH_${name}=OFF`),
  "-DWITH_LIBSHARPYUV=OFF", "-DWITH_HEADER_COMPRESSION=OFF", "-DWITH_UNCOMPRESSED_CODEC=OFF",
  "-DWITH_EXAMPLES=OFF", "-DWITH_GDK_PIXBUF=OFF", "-DBUILD_TESTING=OFF", "-DENABLE_PLUGIN_LOADING=OFF"]);
writeFileSync(join(output, "heif-build-commands.json"), `${JSON.stringify({ platform: process.platform, arch: process.arch, cmake: calls }, null, 2)}\n`);

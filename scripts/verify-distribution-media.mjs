#!/usr/bin/env node
/* global process, console */
import { mkdirSync } from "node:fs";
import { join, resolve } from "node:path";
import { spawnSync } from "node:child_process";

const directory = resolve(process.argv[2]);
const suffix = process.platform === "win32" ? ".exe" : "";
const ffmpeg = join(directory, `ffmpeg/bin/ffmpeg${suffix}`);
const ffprobe = join(directory, `ffmpeg/bin/ffprobe${suffix}`);
function run(program, args) {
  const result = spawnSync(program, args, { encoding: "utf8", windowsHide: true, timeout: 120_000 });
  if (result.status !== 0 || result.error) throw new Error(`${program}: ${result.error?.message ?? result.stderr}`);
  return result.stdout;
}
const fixtures = join(directory, "media-verification");
mkdirSync(fixtures, { recursive: true });
for (const [extension, video, audio] of [["mp4", "libx264", "aac"], ["webm", "libvpx", "libvorbis"]]) {
  const file = join(fixtures, `proxy.${extension}`);
  run(ffmpeg, ["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", "testsrc2=size=64x64:rate=10", "-f", "lavfi", "-i", "sine=frequency=440", "-t", "0.5", "-c:v", video, "-c:a", audio, file]);
  const streams = JSON.parse(run(ffprobe, ["-v", "error", "-show_streams", "-of", "json", file])).streams;
  if (!streams.some((stream) => stream.codec_type === "video") || !streams.some((stream) => stream.codec_type === "audio")) throw new Error(`Missing proxy stream: ${extension}`);
  run(ffmpeg, ["-hide_banner", "-loglevel", "error", "-i", file, "-f", "null", "-"]);
  console.log(`encode/probe/decode passed: ${video}/${audio}`);
}
const decoders = run(ffmpeg, ["-hide_banner", "-decoders"]);
if (!decoders.includes("libdav1d")) throw new Error("AV1 decoder is missing");
const protocols = run(ffmpeg, ["-hide_banner", "-protocols"]);
if (/^\s*https?\s*$/m.test(protocols)) throw new Error("Network protocols must be disabled");
console.log("AV1 decoder present; network protocols disabled");

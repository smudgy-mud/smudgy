// Place this module and a cue.wav file in your server's modules directory.
// Local module paths use import.meta.url; bare relative paths use the process cwd.
// Supported: WAV, MP3, FLAC, Ogg/Vorbis, AAC-LC or ALAC in M4A, and AIFF.
// Mono/stereo, 8–192 kHz, up to 256 MiB per encoded file. Package scripts also
// need file-read permission; importing package code does not grant asset access.
import { echo } from "smudgy:core";
import { Audio } from "smudgy:media";

// Construction preloads a bounded queue; play() starts playback. The player
// uses this session's output and automatically releases resources at the end.
const audio = new Audio(new URL("./cue.wav", import.meta.url));
audio.volume = 0.15;
audio.onerror = () => echo(`Audio playback failed: ${audio.error}`);
audio.onended = () => echo("Audio file finished.");
try {
  await audio.play();
} catch (error) {
  echo(`Could not start audio file: ${error}`);
}

// audio.pause() preserves position; await audio.play() resumes or replays at EOF.
// await audio.close() releases a preloaded/paused player when it is no longer needed.

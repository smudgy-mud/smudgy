// Smudgy's module-scoped media API. This does not extend Web Audio globals.
import { core, domException, primordials, webidl } from "ext:deno_audio/00_webidl.js";
import {
  HostAudioNode, hostContextNative, observeHostSource, callNative, rethrowNativeError,
  hostSourceReady, hostSourceEnded, AudioContext,
} from "ext:deno_audio/02_offline_audio.js";

const {
  SafeWeakMap, WeakMapPrototypeGet, WeakMapPrototypeSet, Symbol, TypeError,
  ReflectApply, Promise, PromiseResolve, PromiseReject, PromisePrototypeThen,
  NumberIsFinite,
} = primordials;
const { defineEventHandler, Event, EventTarget, EventTargetPrototype } = core.loadExtScript("ext:deno_web/02_event.js");
const { markNotSerializable } = core.loadExtScript("ext:deno_web/13_message_port.js");
const records = new SafeWeakMap();
const constructSource = Symbol("[[constructSmudgyFileSource]]");
const dispatchEvent = EventTargetPrototype.dispatchEvent;

function sourceRecord(source) {
  const record = WeakMapPrototypeGet(records, source);
  if (record === undefined) throw new TypeError("Illegal AudioFileSourceNode receiver");
  return record;
}

export class AudioFileSourceNode extends HostAudioNode {
  constructor(key = undefined, context = undefined, native = undefined) {
    if (key !== constructSource) webidl.illegalConstructor();
    super(context, native);
    WeakMapPrototypeSet(records, this, { context, native, ended: false, error: null });
  }

  get error() { return sourceRecord(this).error; }

  start() {
    const record = sourceRecord(this);
    hostContextNative(record.context);
    if (arguments.length !== 0) {
      throw domException("Scheduled file starts are not supported.", "NotSupportedError");
    }
    callNative("Audio file start", () => record.native.startFile());
    observeHostSource(record.context, hostSourceEnded(record.native), (error) => {
      if (record.ended) return;
      record.ended = true;
      if (error !== undefined) {
        try { rethrowNativeError(error, "Audio file playback"); }
        catch (converted) { record.error = converted; }
        ReflectApply(dispatchEvent, this, [new Event("error")]);
      }
      ReflectApply(dispatchEvent, this, [new Event("ended")]);
    });
  }

  stop() {
    const record = sourceRecord(this);
    if (arguments.length !== 0) {
      throw domException("Scheduled file stops are not supported.", "NotSupportedError");
    }
    callNative("Audio file stop", () => record.native.stopFile());
  }
}

export async function createFileSource(context, source) {
  webidl.requiredArguments(arguments.length, 2, "smudgy:media createFileSource");
  source = webidl.converters.DOMString(source, "smudgy:media createFileSource", "Argument 2");
  // String conversion can execute user code, including context.close().
  const host = hostContextNative(context);
  const native = callNative("Audio file source", () => host.createFileSource(source));
  const node = new AudioFileSourceNode(constructSource, context, native);
  try {
    await hostSourceReady(native);
    try { hostContextNative(context); }
    catch { throw domException("Audio file load cancelled by context close.", "AbortError"); }
  } catch (error) {
    native.stopFile();
    rethrowNativeError(error, "Audio file load");
  }
  return node;
}

webidl.configureInterface(AudioFileSourceNode);
defineEventHandler(AudioFileSourceNode.prototype, "ended");
defineEventHandler(AudioFileSourceNode.prototype, "error");
markNotSerializable(AudioFileSourceNode.prototype);

// Each player owns a context: suspending it preserves the PCM queue and decoder
// position without stopping other players. Retirement closes and joins that
// context before a replacement can acquire the player's next worker slot.
const players = new SafeWeakMap();
function playerRecord(player) {
  const record = WeakMapPrototypeGet(players, player);
  if (record === undefined) throw new TypeError("Illegal smudgy:media Audio receiver");
  return record;
}

function emit(player, record, type) {
  const generation = record.generation;
  PromisePrototypeThen(PromiseResolve(), () => {
    if (record.generation === generation) {
      ReflectApply(dispatchEvent, player, [new Event(type)]);
    }
  });
}

function settlePlay(record, error = undefined) {
  const pending = record.pending;
  record.pending = [];
  for (let i = 0; i < pending.length; ++i) {
    if (error === undefined) pending[i].resolve();
    else pending[i].reject(error);
  }
}

function retire(record) {
  const cycle = record.cycle;
  record.cycle = null;
  // Start cancellation immediately, even if preparation or a state change is
  // still awaiting its native operation. A not-yet-created context is guarded
  // by the cycle identity check in prepare().
  const closing = cycle?.context?.close();
  const previous = record.cleanup;
  record.cleanup = (async () => { await previous; await closing; })();
  // Preloading and synchronous setters have no returned promise. Keep cleanup
  // errors observed here; close() and the next preparation still surface them.
  PromisePrototypeThen(record.cleanup, undefined, () => {});
  return record.cleanup;
}

function fail(player, record, cycle, error) {
  if (record.cycle !== cycle) return;
  record.error = error;
  record.paused = true;
  record.ended = false;
  ++record.command;
  settlePlay(record, error);
  retire(record);
  emit(player, record, "error");
}

function applyVolume(record) {
  const gain = record.cycle?.gain;
  if (gain) gain.gain.value = record.muted ? 0 : record.volume;
}

function prepare(player, record) {
  const cycle = { context: null, gain: null, node: null, started: false,
    playing: false, ready: null, control: PromiseResolve() };
  record.cycle = cycle;
  const source = record.src;
  cycle.ready = (async () => {
    try {
      await record.cleanup;
      if (record.cycle !== cycle) return false;
      const context = cycle.context = new AudioContext();
      await context.suspend();
      if (record.cycle !== cycle) return false;
      const node = await createFileSource(context, source);
      if (record.cycle !== cycle) return false;
      cycle.node = node;
      cycle.gain = context.createGain();
      applyVolume(record);
      node.connect(cycle.gain);
      cycle.gain.connect(context.destination);
      node.onended = () => {
        if (record.cycle !== cycle) return;
        if (node.error !== null) { fail(player, record, cycle, node.error); return; }
        record.paused = true;
        record.ended = true;
        const generation = record.generation;
        settlePlay(record);
        const cleanup = retire(record);
        PromisePrototypeThen(cleanup, () => {
          if (record.generation === generation) emit(player, record, "ended");
        }, (error) => {
          if (record.generation !== generation) return;
          record.error = error;
          record.ended = false;
          emit(player, record, "error");
        });
      };
      emit(player, record, "canplay");
      return true;
    } catch (error) {
      fail(player, record, cycle, error);
      return false;
    }
  })();
  return cycle;
}

function changePlayback(player, record, playing) {
  const cycle = record.cycle;
  const command = ++record.command;
  if (cycle === null) return;
  cycle.control = PromisePrototypeThen(cycle.control, async () => {
    if (!await cycle.ready || record.cycle !== cycle || record.command !== command) return;
    try {
      if (!playing) {
        await cycle.context.suspend();
        cycle.playing = false;
        return;
      }
      if (!cycle.started) { cycle.node.start(); cycle.started = true; }
      await cycle.context.resume();
      if (record.cycle !== cycle || record.command !== command) return;
      if (!cycle.playing) emit(player, record, "playing");
      cycle.playing = true;
      settlePlay(record);
    } catch (error) {
      fail(player, record, cycle, error);
    }
  });
}

function reset(player, record, preload) {
  ++record.generation;
  ++record.command;
  settlePlay(record, domException("Audio playback was interrupted by a new load.", "AbortError"));
  record.paused = true;
  record.ended = false;
  record.error = null;
  retire(record);
  if (preload && record.src !== "") {
    emit(player, record, "loadstart");
    prepare(player, record);
  }
}

/** A local streaming player with a documented subset of HTML Audio behavior.
 * This is an EventTarget, not an HTMLAudioElement or a Web Audio node.
 */
export class Audio extends EventTarget {
  constructor(source = undefined) {
    super();
    this[webidl.brand] = webidl.brand;
    const record = { src: "", paused: true, ended: false, error: null,
      volume: 1, muted: false, generation: 0, command: 0, cycle: null,
      cleanup: PromiseResolve(), pending: [] };
    WeakMapPrototypeSet(players, this, record);
    if (source !== undefined) this.src = source;
  }

  get src() { return playerRecord(this).src; }
  set src(value) {
    const record = playerRecord(this);
    value = webidl.converters.DOMString(value, "smudgy:media Audio", "src");
    record.src = value;
    reset(this, record, true);
  }
  get paused() { return playerRecord(this).paused; }
  get ended() { return playerRecord(this).ended; }
  get error() { return playerRecord(this).error; }
  get volume() { return playerRecord(this).volume; }
  set volume(value) {
    const record = playerRecord(this);
    value = webidl.converters.double(value, "smudgy:media Audio", "volume");
    if (!NumberIsFinite(value) || value < 0 || value > 1) {
      throw domException("Audio volume must be between 0 and 1.", "IndexSizeError");
    }
    if (record.volume === value) return;
    const gain = record.cycle?.gain;
    if (gain) gain.gain.value = record.muted ? 0 : value;
    record.volume = value;
    emit(this, record, "volumechange");
  }
  get muted() { return playerRecord(this).muted; }
  set muted(value) {
    const record = playerRecord(this);
    value = webidl.converters.boolean(value);
    if (record.muted === value) return;
    const gain = record.cycle?.gain;
    if (gain) gain.gain.value = value ? 0 : record.volume;
    record.muted = value;
    emit(this, record, "volumechange");
  }

  load() { reset(this, playerRecord(this), true); }

  play() {
    const record = playerRecord(this);
    if (record.src === "") return PromiseReject(domException("Audio has no source.", "NotSupportedError"));
    if (record.error !== null) return PromiseReject(record.error);
    if (record.cycle === null) {
      ++record.generation;
      record.ended = false;
      emit(this, record, "loadstart");
      prepare(this, record);
    }
    const promise = new Promise((resolve, reject) => {
      record.pending[record.pending.length] = { resolve, reject };
    });
    if (record.paused) { record.paused = false; emit(this, record, "play"); }
    changePlayback(this, record, true);
    return promise;
  }

  pause() {
    const record = playerRecord(this);
    if (record.paused) return;
    record.paused = true;
    settlePlay(record, domException("Audio playback was interrupted by pause().", "AbortError"));
    changePlayback(this, record, false);
    emit(this, record, "pause");
  }

  /** Release preloaded or paused resources. The same player can play again. */
  close() {
    const record = playerRecord(this);
    reset(this, record, false);
    return record.cleanup;
  }
}

webidl.configureInterface(Audio);
for (const type of ["loadstart", "canplay", "play", "playing", "pause", "ended", "error", "volumechange"]) {
  defineEventHandler(Audio.prototype, type);
}
markNotSerializable(Audio.prototype);

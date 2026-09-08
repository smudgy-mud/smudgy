// Managed by smudgy. This is the deliberately narrow online Web Audio surface
// supported by Smudgy's hosted session output. It is not the browser DOM lib.

type AudioContextLatencyCategory = "balanced" | "interactive" | "playback";
type AudioContextState = "closed" | "running" | "suspended";
type OscillatorType = "sawtooth" | "sine" | "square" | "triangle";

interface AudioSinkOptions {
  type: "none";
}

interface AudioContextOptions {
  latencyHint?: AudioContextLatencyCategory | number;
  /** Smudgy's physical session output currently requires 48,000 Hz. */
  sampleRate?: number;
  /** Omit or use "" for session output; "none" is hardware-independent. */
  sinkId?: "" | "none" | AudioSinkOptions;
}

interface GainOptions {
  gain?: number;
}

interface OscillatorOptions {
  detune?: number;
  frequency?: number;
  type?: OscillatorType;
}

interface AudioParam {
  /** Scalar mutation is supported; AudioParam automation methods are not. */
  value: number;
}

declare var AudioParam: {
  readonly prototype: AudioParam;
};

interface AudioNode extends EventTarget {
  readonly context: BaseAudioContext;
  readonly numberOfInputs: number;
  readonly numberOfOutputs: number;
  connect<T extends AudioNode>(destinationNode: T, output?: 0, input?: 0): T;
  connect(destinationParam: AudioParam, output?: 0): void;
  disconnect(): void;
  disconnect(output: 0): void;
  disconnect(destinationNode: AudioNode): void;
  disconnect(destinationNode: AudioNode, output: 0): void;
  disconnect(destinationNode: AudioNode, output: 0, input: 0): void;
  disconnect(destinationParam: AudioParam): void;
  disconnect(destinationParam: AudioParam, output: 0): void;
}

declare var AudioNode: {
  readonly prototype: AudioNode;
};

interface AudioDestinationNode extends AudioNode {
  readonly maxChannelCount: number;
}

declare var AudioDestinationNode: {
  readonly prototype: AudioDestinationNode;
};

interface BaseAudioContext extends EventTarget {
  readonly currentTime: number;
  readonly destination: AudioDestinationNode;
  onstatechange: ((this: BaseAudioContext, event: Event) => unknown) | null;
  readonly sampleRate: number;
  readonly state: AudioContextState;
  createGain(): GainNode;
  createOscillator(): OscillatorNode;
}

declare var BaseAudioContext: {
  readonly prototype: BaseAudioContext;
};

interface AudioContext extends BaseAudioContext {
  readonly baseLatency: number;
  readonly outputLatency: number;
  readonly sinkId: "" | "none";
  close(): Promise<void>;
  resume(): Promise<void>;
  suspend(): Promise<void>;
}

declare var AudioContext: {
  readonly prototype: AudioContext;
  new(contextOptions?: AudioContextOptions): AudioContext;
};

/** Smudgy-specific sources live in a module, leaving Web Audio globals standard. */
declare module "smudgy:media" {
  /** Local streaming player; a subset of HTML Audio behavior, not an HTML element.
   * Sources preload a bounded queue without autoplay. No global Audio is installed.
   */
  export class Audio extends EventTarget {
    constructor(source?: string | URL);
    /** Local path or file: URL. Relative paths use the process working directory.
     * Assignment cancels playback and preloads the replacement; empty releases it.
     */
    get src(): string;
    set src(value: string | URL);
    readonly paused: boolean;
    readonly ended: boolean;
    /** Load/playback error, otherwise null. This is an Error, not a MediaError. */
    readonly error: Error | null;
    /** Linear volume from 0 through 1, initially 1. */
    volume: number;
    muted: boolean;
    /** Restart loading the current source; cancels pending play promises. */
    load(): void;
    /** Resolve when playback starts/resumes, not when it ends. Replays after EOF.
     * Rejects with AbortError if interrupted by pause, load, source change, or close.
     */
    play(): Promise<void>;
    /** Preserve the playback position and bounded decoder queue. */
    pause(): void;
    /** Smudgy helper: release resources and await decoder shutdown.
     * Keeps src/volume/muted; a later play() reopens the file from its beginning.
     */
    close(): Promise<void>;
    onloadstart: ((this: Audio, event: Event) => unknown) | null;
    oncanplay: ((this: Audio, event: Event) => unknown) | null;
    onplay: ((this: Audio, event: Event) => unknown) | null;
    onplaying: ((this: Audio, event: Event) => unknown) | null;
    onpause: ((this: Audio, event: Event) => unknown) | null;
    onended: ((this: Audio, event: Event) => unknown) | null;
    onerror: ((this: Audio, event: Event) => unknown) | null;
    onvolumechange: ((this: Audio, event: Event) => unknown) | null;
  }

  /** Stream a local file. Requires this isolate's read permission. */
  export function createFileSource(context: AudioContext, source: string | URL): Promise<AudioFileSourceNode>;

  /** A paused file source returned by createFileSource; cannot be directly constructed. */
  export interface AudioFileSourceNode extends AudioNode {
    /** Decoder failure after loading, otherwise null. Load failures reject createFileSource. */
    readonly error: Error | null;
    onerror: ((this: AudioFileSourceNode, event: Event) => unknown) | null;
    onended: ((this: AudioFileSourceNode, event: Event) => unknown) | null;
    /** Start once, immediately. Scheduling, seeking, and looping are not supported. */
    start(): void;
    /** Silence and cancel permanently. Context.close waits for decoder shutdown. */
    stop(): void;
  }

  export const AudioFileSourceNode: { readonly prototype: AudioFileSourceNode };
}

interface GainNode extends AudioNode {
  readonly gain: AudioParam;
}

declare var GainNode: {
  readonly prototype: GainNode;
  new(context: BaseAudioContext, options?: GainOptions): GainNode;
};

interface AudioScheduledSourceNode extends AudioNode {
  onended: ((this: AudioScheduledSourceNode, event: Event) => unknown) | null;
  start(when?: number): void;
  stop(when?: number): void;
}

declare var AudioScheduledSourceNode: {
  readonly prototype: AudioScheduledSourceNode;
};

interface OscillatorNode extends AudioScheduledSourceNode {
  readonly detune: AudioParam;
  readonly frequency: AudioParam;
  type: OscillatorType;
}

declare var OscillatorNode: {
  readonly prototype: OscillatorNode;
  new(context: BaseAudioContext, options?: OscillatorOptions): OscillatorNode;
};

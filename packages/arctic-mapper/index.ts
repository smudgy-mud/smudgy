/**
 * Auto-mapper for ArcticMUD.
 *
 * Entry point and public API. Other modules should import from this file
 * (or from prompt.ts for prompt/vitals events), not from mapper/ internals.
 *
 * Requires prompt.ts — see mapper/README.md for setup and usage.
 */

import './mapper/room-capturer.ts';
import './mapper/alias.ts';

export { mapEvent } from './mapper/events.ts';
export type { RoomEvent } from './mapper/events.ts';
export { onAreaChanged, onRoomChanged, options, speedwalk, state, State } from './mapper/mapper.ts';
export type { MoveCommand } from './mapper/mapper.ts';
export { Direction, DirectionLetter, MoveCommands, RoomFlags } from './mapper/mapper.ts';

import './mapper/widget.tsx';
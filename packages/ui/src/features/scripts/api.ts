// The script API as the editor reads it (Plan.md P15.25; ADR-0045): everything here comes from
// the generated description (`generated/script-api.ts`, fleet-script's `API` through `just gen`),
// never from a list of the UI's own. Pure.

import type { ScriptApiMember } from "../../generated/ScriptApiMember";
import type { ScriptApiObject } from "../../generated/ScriptApiObject";
import type { ScriptExample } from "../../generated/ScriptExample";
import { SCRIPT_API } from "../../generated/script-api";

/** The description's name for the functions a script defines. */
const HOOKS = "hooks";

/** Every object of the description by its name: the game objects, `input` and `hooks`. */
const OBJECTS: ReadonlyMap<string, ScriptApiObject> = new Map(
  SCRIPT_API.objects.map((object) => [object.name, object]),
);

/**
 * The game objects' names: what a script names to reach the game (`keys`, `bot`, `menu`, …).
 * Never a value: `input`, or a map a script reads (`slot`, `time`), whose name a script may give
 * a variable of its own (Plan.md P16.16).
 */
export const GAME_OBJECTS: ReadonlySet<string> = new Set(
  SCRIPT_API.objects.filter((object) => object.kind === "game").map((object) => object.name),
);

/** The game object or the API object `name`, if the description has it. */
export function objectNamed(name: string): ScriptApiObject | undefined {
  return name === HOOKS ? undefined : OBJECTS.get(name);
}

/** Member `name` of `object`, if it has one. */
export function memberOf(object: ScriptApiObject, name: string): ScriptApiMember | undefined {
  return object.members.find((member) => member.name === name);
}

/** The hooks a script may define, each with its signature and summary. */
export const HOOK_MEMBERS: readonly ScriptApiMember[] = OBJECTS.get(HOOKS)?.members ?? [];

/** The reference's examples, as starter templates. */
export const EXAMPLES: readonly ScriptExample[] = SCRIPT_API.examples;

/** A name in a chain, and whether it's called: `menu.slot(13)` is `menu`, then `slot` called. */
export interface ChainPart {
  readonly name: string;
  readonly called: boolean;
}

/**
 * Where a chain leads: `keys.forward` is an `input`, `menu.slot(13)` a `slot` map,
 * `clock.local(zone)` a `time` map. A chain whose first name is no game object, or that passes
 * through a value the description doesn't name, leads nowhere it knows.
 */
export function resolveCalls(parts: readonly ChainPart[]): ScriptApiObject | undefined {
  const [first, ...rest] = parts;
  if (first === undefined || first.called || !GAME_OBJECTS.has(first.name)) {
    return undefined;
  }
  let object = OBJECTS.get(first.name);
  for (const part of rest) {
    const member = object === undefined ? undefined : memberOf(object, part.name);
    const next = part.called ? member?.answers : member?.object;
    object = next === undefined ? undefined : OBJECTS.get(next);
  }
  return object;
}

/** Where a chain of names, none called, leads: `["keys", "forward"]` is an `input`. */
export function resolveChain(names: readonly string[]): ScriptApiObject | undefined {
  return resolveCalls(names.map((name) => ({ name, called: false })));
}

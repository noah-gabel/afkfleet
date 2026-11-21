// The script API as the editor reads it (Plan.md P15.25; ADR-0045): everything here comes from
// the generated description (`generated/script-api.ts`, fleet-script's `API` through `just gen`),
// never from a list of the UI's own. Pure.

import type { ScriptApiMember } from "../../generated/ScriptApiMember";
import type { ScriptApiObject } from "../../generated/ScriptApiObject";
import type { ScriptExample } from "../../generated/ScriptExample";
import { SCRIPT_API } from "../../generated/script-api";

/** The description's name for what a key or a mouse button is. */
const INPUT = "input";
/** The description's name for the functions a script defines. */
const HOOKS = "hooks";

/** Every object of the description by its name: the game objects, `input` and `hooks`. */
const OBJECTS: ReadonlyMap<string, ScriptApiObject> = new Map(
  SCRIPT_API.objects.map((object) => [object.name, object]),
);

/** The game objects' names: what a script names to reach the game (`keys`, `bot`, …). */
export const GAME_OBJECTS: ReadonlySet<string> = new Set(
  SCRIPT_API.objects
    .map((object) => object.name)
    .filter((name) => name !== INPUT && name !== HOOKS),
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

/**
 * Where a chain of names leads: `["keys", "forward"]` is an `input`; a chain whose first name is
 * no game object, or that passes through a value, leads nowhere the description knows.
 */
export function resolveChain(names: readonly string[]): ScriptApiObject | undefined {
  const [first, ...rest] = names;
  if (first === undefined || !GAME_OBJECTS.has(first)) {
    return undefined;
  }
  let object = OBJECTS.get(first);
  for (const name of rest) {
    const member = object === undefined ? undefined : memberOf(object, name);
    object = member?.object === undefined ? undefined : OBJECTS.get(member.object);
  }
  return object;
}

// What the modes' screens' tests script their FakeApiClient with (Plan.md P11.9): a mode's steps
// as answers show them. Placeholders only (CLAUDE.md).
import { act, screen, within } from "@testing-library/react";

import type { ApiClient } from "../api/ApiClient";
import type { CreateModeRequest } from "../generated/CreateModeRequest";
import type { ModeAction } from "../generated/ModeAction";
import type { ModeResponse } from "../generated/ModeResponse";
import type { ModeSchedule } from "../generated/ModeSchedule";
import type { ModeStep } from "../generated/ModeStep";
import type { UpdateModeRequest } from "../generated/UpdateModeRequest";
import { mode } from "./live";
import type { RenderedApp } from "./renderApp";
import { signedIn } from "./signedIn";
import { refusal } from "./signIn";

/** A step of `action`, on `schedule`, as an answer shows it. */
export function shownStep(
  action: Omit<ModeAction, "arguments_hidden"> & { readonly arguments_hidden?: boolean },
  schedule: ModeSchedule = { type: "at_start" },
  probability = 100,
): ModeStep {
  return { action: { arguments_hidden: false, ...action }, schedule, probability };
}

/** A repeating schedule: `seconds` apart, and up to `jitter` more. */
export function every(seconds: number, jitter = 0): ModeSchedule {
  return { type: "every", interval_ms: seconds * 1000, jitter_ms: jitter * 1000 };
}

/** The on-respawn schedule. */
export const ON_RESPAWN: ModeSchedule = { type: "on_respawn" };

const NEEDS_STEP_UP =
  "This mode holds a command that needs your password: confirm it's you to save it.";
export const SECTION = {
  atStart: "When it joins",
  repeating: "While it's online",
  onRespawn: "After it respawns",
} as const;

/**
 * The App on mode `id`'s screen, or a new mode's (`"new"`), over a fake that keeps `rows`, points
 * a command at its twin, and asks for the password there until `stepUp` ran (unless `stamped`).
 */
export async function atMode(
  id: string,
  options: {
    readonly rows?: ModeResponse[];
    readonly stamped?: boolean;
    readonly handlers?: Partial<ApiClient>;
  } = {},
) {
  const state = {
    rows: [...(options.rows ?? [])],
    stamped: options.stamped ?? false,
    readFails: false,
    /** While set, the modes' reads wait for it. */
    hold: null as Promise<void> | null,
  };
  const holdsCommand = (request: CreateModeRequest) =>
    request.steps.some((step) => step.action.message?.startsWith("/") === true);
  const twin = () => {
    if (!state.stamped) {
      throw refusal(403, "step_up_required");
    }
  };
  const saved = (request: CreateModeRequest): ModeResponse =>
    mode(9, {
      name: request.name.trim(),
      visibility: request.visibility,
      steps: request.steps.map((step) => ({
        ...step,
        action: { ...step.action, arguments_hidden: false },
      })),
      unlisted_command: holdsCommand(request),
    });
  const created = (request: CreateModeRequest) => {
    const made = saved(request);
    state.rows = [made, ...state.rows];
    return made;
  };
  const updated = (target: string, request: UpdateModeRequest) => {
    const row = state.rows.find((one) => one.id === target);
    if (row === undefined) {
      throw refusal(404, "not_found");
    }
    const { version: _read, ...body } = request;
    const edited = { ...saved(body), id: target, version: row.version + 1 };
    state.rows = state.rows.map((one) => (one.id === target ? edited : one));
    return edited;
  };
  const app = await signedIn({
    modes: async () => {
      await state.hold;
      if (state.readFails) {
        throw refusal(500, "internal");
      }
      return { items: state.rows };
    },
    stepUp: async () => {
      state.stamped = true;
    },
    createMode: async (request) => {
      if (holdsCommand(request)) {
        throw refusal(403, "mode_needs_step_up", { message: NEEDS_STEP_UP });
      }
      return created(request);
    },
    createModeWithCommands: async (request) => {
      twin();
      return created(request);
    },
    updateMode: async (target, request) => {
      if (holdsCommand(request)) {
        throw refusal(403, "mode_needs_step_up", { message: NEEDS_STEP_UP });
      }
      return updated(target, request);
    },
    updateModeWithCommands: async (target, request) => {
      twin();
      return updated(target, request);
    },
    ...options.handlers,
  });
  await act(() =>
    id === "new"
      ? app.runtime.router.navigate({ to: "/modes/new" })
      : app.runtime.router.navigate({ to: "/modes/$id", params: { id } }),
  );
  return { app, state };
}

/** A section of the form. */
export function section(name: keyof typeof SECTION): HTMLElement {
  return screen.getByRole("group", { name: SECTION[name] });
}

/** Step `n` of a section, counted from 1. */
export function card(name: keyof typeof SECTION, n: number): HTMLElement {
  return within(section(name)).getByRole("group", { name: `Step ${n}` });
}

/** The arguments of `method`'s calls. */
export function calls(app: RenderedApp, method: keyof ApiClient): unknown[][] {
  return app.fake.calls.filter((call) => call.method === method).map((call) => [...call.args]);
}

/** `field`'s value replaced by `value`. */
export async function fill(app: RenderedApp, field: HTMLElement, value: string): Promise<void> {
  await app.user.clear(field);
  await app.user.click(field);
  await app.user.paste(value);
}

/** Save, by role. */
export function saveButton(): HTMLButtonElement {
  return screen.getByRole("button", { name: "Save" }) as HTMLButtonElement;
}

import { describe, expect, it } from "vitest";

import type { ApiClient } from "./ApiClient";
import { ApiError } from "./ApiError";
import { FakeApiClient } from "./FakeApiClient";
import {
  CA_PEM_MAX_BYTES,
  CHAT_FORMAT_MAX_CHARS,
  CHAT_FORMATS_MAX,
  CHAT_MESSAGE_MAX_UNITS,
  CHAT_PREVIEW_LINE_MAX_CHARS,
  CODE_MAX_CHARS,
  CONFLICT_TEXT_MAX_CHARS,
  CONFLICT_TEXTS_MAX,
  MC_NAME_MAX_CHARS,
  NEW_PASSWORD_MAX_CHARS,
  PAGE_MAX,
  PASSWORD_MAX_CHARS,
  SERVER_ADDRESS_MAX_CHARS,
  SERVER_SETTINGS_PAGE_MAX,
  SERVER_URL_MAX_BYTES,
  TOKEN_MAX_CHARS,
  USERNAME_MAX_CHARS,
} from "./limits";
import { validated } from "./validated";

const ID = "01900000-0000-7000-8000-000000000001";
const USER = "01900000-0000-7000-8000-000000000002";

/** A fake whose every method answers, wrapped in the checks. */
function client() {
  const ok = async () => undefined as never;
  const fake = new FakeApiClient({
    serverProfile: ok,
    setServerProfile: ok,
    checkServer: ok,
    login: ok,
    loginMfa: ok,
    logout: ok,
    register: ok,
    stepUp: ok,
    mfaEnroll: ok,
    mfaConfirm: ok,
    mfaReset: ok,
    me: ok,
    changePassword: ok,
    sessions: ok,
    revokeSession: ok,
    users: ok,
    patchUser: ok,
    invites: ok,
    createInvite: ok,
    revokeInvite: ok,
    startLink: ok,
    linkStatus: ok,
    createOfflineAccount: ok,
    accounts: ok,
    accountsAwaitingApproval: ok,
    account: ok,
    deleteAccount: ok,
    approveAccount: ok,
    refuseAccount: ok,
    relinkAccount: ok,
    accountGrants: ok,
    createGrant: ok,
    changeGrant: ok,
    revokeGrant: ok,
    openVerificationLink: ok,
    watchEvents: ok,
    bots: ok,
    bot: ok,
    modes: ok,
    createMode: ok,
    createModeWithCommands: ok,
    updateMode: ok,
    updateModeWithCommands: ok,
    deleteMode: ok,
    patchBot: ok,
    setBotMode: ok,
    startBot: ok,
    stopBot: ok,
    restartBot: ok,
    resetBot: ok,
    resumeBot: ok,
    botChat: ok,
    sendChat: ok,
    sendCommand: ok,
    serverSettings: ok,
    createServerSettings: ok,
    updateServerSettings: ok,
    deleteServerSettings: ok,
    previewChatFormats: ok,
  });
  return { fake, api: validated(fake) as ApiClient };
}

/** The fields `call` was refused for, each with its message, or a failure if it passed. */
async function refused(call: Promise<unknown>): Promise<{ path: string; message: string }[]> {
  try {
    await call;
  } catch (rejection) {
    expect(rejection).toBeInstanceOf(ApiError);
    const error = (rejection as ApiError).error;
    expect(error.code).toBe("invalid_input");
    return (error.fields ?? []).map((field) => ({ path: field.path, message: field.message }));
  }
  throw new Error("the call wasn't refused");
}

/** The paths of the fields `call` was refused for, or a failure if it passed. */
async function refusedFields(call: Promise<unknown>): Promise<string[]> {
  try {
    await call;
  } catch (rejection) {
    expect(rejection).toBeInstanceOf(ApiError);
    const error = (rejection as ApiError).error;
    expect(error.code).toBe("invalid_input");
    return (error.fields ?? []).map((field) => field.path);
  }
  throw new Error("the call wasn't refused");
}

describe("validated", () => {
  it("passes valid inputs through unchanged", async () => {
    const { fake, api } = client();

    await api.login({ username: "AfkBot1", password: "placeholder password" });
    await api.loginMfa("123456");
    await api.sessions(ID, PAGE_MAX);
    await api.users(undefined, undefined);
    await api.patchUser(ID, { disabled: true });
    await api.setServerProfile({ url: "https://example.com" });
    await api.me();

    expect(fake.calls.map((call) => call.method)).toEqual([
      "login",
      "loginMfa",
      "sessions",
      "users",
      "patchUser",
      "setServerProfile",
      "me",
    ]);
    expect(fake.calls[0]?.args).toEqual([
      { username: "AfkBot1", password: "placeholder password" },
    ]);
  });

  it("refuses a username that breaks the server's rule, before the call", async () => {
    const { fake, api } = client();

    for (const username of ["ab", "a".repeat(USERNAME_MAX_CHARS + 1), "_bot", "bot name", "bøt"]) {
      expect(await refusedFields(api.login({ username, password: "x" }))).toEqual(["username"]);
    }
    expect(fake.calls).toEqual([]);
  });

  it("caps the login and step-up password", async () => {
    const { api } = client();
    const long = "p".repeat(PASSWORD_MAX_CHARS + 1);

    expect(await refusedFields(api.login({ username: "AfkBot1", password: long }))).toEqual([
      "password",
    ]);
    expect(await refusedFields(api.stepUp({ password: long }))).toEqual(["password"]);
  });

  it("checks a new password's length and characters", async () => {
    const { api } = client();

    for (const new_password of [
      "short",
      "n".repeat(NEW_PASSWORD_MAX_CHARS + 1),
      "has a\ttab in it ok",
    ]) {
      expect(await refusedFields(api.changePassword({ new_password }))).toEqual(["new_password"]);
    }
  });

  it("checks a registration's invite, username and password together", async () => {
    const { api } = client();

    const fields = await refusedFields(
      api.register({ invite: "i".repeat(TOKEN_MAX_CHARS + 1), username: "x", password: "short" }),
    );

    expect(fields.sort()).toEqual(["invite", "password", "username"]);
  });

  it("refuses an empty or overlong code under its own name", async () => {
    const { api } = client();

    expect(await refusedFields(api.loginMfa(""))).toEqual(["code"]);
    expect(await refusedFields(api.mfaConfirm("1".repeat(CODE_MAX_CHARS + 1)))).toEqual(["code"]);
  });

  it("refuses an ID that isn't one, and a page size out of range", async () => {
    const { api } = client();

    expect(await refusedFields(api.revokeSession("not-an-id"))).toEqual(["id"]);
    expect(await refusedFields(api.revokeInvite("01900000-0000-4000-8000-000000000001"))).toEqual([
      "id",
    ]);
    expect(await refusedFields(api.sessions("x", 0))).toEqual(["before", "limit"]);
    expect(await refusedFields(api.invites(undefined, PAGE_MAX + 1))).toEqual(["limit"]);
    expect(await refusedFields(api.users(undefined, 1.5))).toEqual(["limit"]);
  });

  it("refuses a patch without exactly one change, on the request as a whole", async () => {
    const { api } = client();

    expect(await refusedFields(api.patchUser(ID, {}))).toEqual([""]);
    expect(await refusedFields(api.patchUser(ID, { role: "admin", disabled: true }))).toEqual([""]);
  });

  it("refuses the owner role for an invite or a patch", async () => {
    const { api } = client();

    expect(await refusedFields(api.createInvite({ role: "owner" }))).toEqual(["role"]);
    expect(await refusedFields(api.patchUser(ID, { role: "owner" }))).toEqual(["role"]);
  });

  it("caps the server profile in bytes", async () => {
    const { api } = client();

    expect(
      await refusedFields(api.setServerProfile({ url: "é".repeat(SERVER_URL_MAX_BYTES / 2 + 1) })),
    ).toEqual(["url"]);
    expect(
      await refusedFields(
        api.setServerProfile({
          url: "https://example.com",
          ca_pem: "c".repeat(CA_PEM_MAX_BYTES + 1),
        }),
      ),
    ).toEqual(["ca_pem"]);
  });

  it("refuses a field the server doesn't know", async () => {
    const { api } = client();
    const request = { url: "https://example.com", insecure: true };

    expect(await refusedFields(api.setServerProfile(request))).toEqual([""]);
  });

  it("passes valid account inputs through unchanged", async () => {
    const { fake, api } = client();

    await api.startLink();
    await api.linkStatus(ID);
    await api.createOfflineAccount({ name: "AfkBot_1" });
    await api.accounts(ID, PAGE_MAX);
    await api.accountsAwaitingApproval(undefined, undefined);
    await api.account(ID);
    await api.deleteAccount(ID);
    await api.approveAccount(ID);
    await api.refuseAccount(ID);
    await api.relinkAccount(ID);
    await api.accountGrants(ID, USER, 1);
    await api.createGrant(ID, { username: "afkbot2", level: "control" });
    await api.changeGrant(ID, USER, { level: "manage" });
    await api.revokeGrant(ID, USER);
    await api.openVerificationLink();
    await api.watchEvents(() => undefined);

    expect(fake.calls).toHaveLength(16);
    expect(fake.calls[11]?.args).toEqual([ID, { username: "afkbot2", level: "control" }]);
  });

  it("refuses an offline name that isn't a Minecraft name, before the call", async () => {
    const { fake, api } = client();

    for (const name of ["ab", "a".repeat(MC_NAME_MAX_CHARS + 1), "Afk Bot", "Afk-Bot", "Bøt1"]) {
      expect(await refusedFields(api.createOfflineAccount({ name }))).toEqual(["name"]);
    }
    expect(fake.calls).toEqual([]);
  });

  it("checks a grant's username and level, and every ID it names", async () => {
    const { fake, api } = client();
    const owner = { username: "ab", level: "owner" } as never;

    expect((await refusedFields(api.createGrant("x", owner))).sort()).toEqual([
      "id",
      "level",
      "username",
    ]);
    expect(
      (await refusedFields(api.changeGrant(ID, "y", { level: "admin" } as never))).sort(),
    ).toEqual(["level", "user"]);
    expect(await refusedFields(api.revokeGrant("x", USER))).toEqual(["id"]);
    expect(
      await refusedFields(
        api.createGrant(ID, { username: "afkbot2", level: "view", note: 1 } as never),
      ),
    ).toEqual([""]);
    expect(fake.calls).toEqual([]);
  });

  it("passes valid bot and mode reads through unchanged", async () => {
    const { fake, api } = client();

    await api.bots(ID, PAGE_MAX);
    await api.bots(undefined, undefined);
    await api.bot(ID);
    await api.modes(ID, 1);

    expect(fake.calls.map((call) => [call.method, call.args])).toEqual([
      ["bots", [ID, PAGE_MAX]],
      ["bots", [undefined, undefined]],
      ["bot", [ID]],
      ["modes", [ID, 1]],
    ]);
  });

  it("refuses a bot or mode ID that isn't one, and a page out of range", async () => {
    const { fake, api } = client();

    expect(await refusedFields(api.bots("x", 0))).toEqual(["before", "limit"]);
    expect(await refusedFields(api.bot("x"))).toEqual(["id"]);
    expect(await refusedFields(api.modes(undefined, PAGE_MAX + 1))).toEqual(["limit"]);
    expect(fake.calls).toEqual([]);
  });

  describe("the mode writes (S82)", () => {
    const step = {
      action: { type: "look", yaw: -12.5, pitch: 0 },
      schedule: { type: "every", interval_ms: 45_000, jitter_ms: 0 },
      probability: 100,
    } as const;
    const create = { name: "Fishing", visibility: "private", steps: [step] } as const;
    const update = { version: 2, ...create };

    /** What the local check says about `request`, by path. */
    async function refusals(request: unknown): Promise<[string, string][]> {
      const { api } = client();
      try {
        await api.createMode(request as Parameters<typeof api.createMode>[0]);
      } catch (rejection) {
        return ((rejection as ApiError).error.fields ?? []).map((field) => [
          field.path,
          field.message,
        ]);
      }
      return [];
    }

    /** `create` with its one step changed by `change`. */
    const stepWith = (change: object) => ({ ...create, steps: [{ ...step, ...change }] });

    it("passes valid writes through unchanged, an action of every kind's fields", async () => {
      const { fake, api } = client();
      const steps = [
        step,
        {
          action: { type: "rotate_random", max_yaw: 30, max_pitch: 10 },
          schedule: { type: "at_start" },
          probability: 1,
        },
        { action: { type: "sneak", on: true }, schedule: { type: "on_respawn" }, probability: 255 },
        {
          action: { type: "select_hotbar_slot", slot: 8 },
          schedule: { type: "at_start" },
          probability: 0,
        },
        {
          action: { type: "send_chat", message: "/spawn" },
          schedule: { type: "at_start" },
          probability: 50,
        },
      ] as const;
      const all = { name: "Fishing", visibility: "shared", steps: [...steps] } as const;

      await api.createMode(all as never);
      await api.createModeWithCommands(create as never);
      await api.updateMode(ID, update as never);
      await api.updateModeWithCommands(ID, update as never);
      await api.deleteMode(ID);

      expect(fake.calls.map((call) => [call.method, call.args])).toEqual([
        ["createMode", [all]],
        ["createModeWithCommands", [create]],
        ["updateMode", [ID, update]],
        ["updateModeWithCommands", [ID, update]],
        ["deleteMode", [ID]],
      ]);
    });

    it("leaves every rule inside the wire type to the server", async () => {
      for (const request of [
        stepWith({ probability: 0 }),
        stepWith({ probability: 101 }),
        stepWith({ probability: 255 }),
        stepWith({ action: { type: "look", yaw: 500, pitch: -500 } }),
        stepWith({ action: { type: "select_hotbar_slot", slot: 255 } }),
        stepWith({ schedule: { type: "every", interval_ms: 0, jitter_ms: 4_294_967_295 } }),
        // A field a type needs but doesn't have: the server's "is required for this type".
        stepWith({ action: { type: "look" }, schedule: { type: "every" } }),
        // A field a type doesn't take: the server's "isn't used by this type".
        stepWith({ action: { type: "jump", yaw: 1 } }),
      ]) {
        expect(await refusals(request)).toEqual([]);
      }
    });

    it.each([
      ["no chance at all", { probability: Number.NaN }, "steps[0].probability", "must be a number"],
      [
        "a part of a percent",
        { probability: 50.5 },
        "steps[0].probability",
        "must be a whole number",
      ],
      ["a chance past a byte", { probability: 256 }, "steps[0].probability", "is out of range"],
      ["a chance below zero", { probability: -1 }, "steps[0].probability", "is out of range"],
      [
        "an infinite chance",
        { probability: Number.POSITIVE_INFINITY },
        "steps[0].probability",
        "must be a number",
      ],
      [
        "an angle that isn't a number",
        { action: { type: "look", yaw: Number.NaN, pitch: 0 } },
        "steps[0].action.yaw",
        "must be a number",
      ],
      [
        "an infinite angle",
        { action: { type: "rotate_random", max_yaw: Number.POSITIVE_INFINITY, max_pitch: 0 } },
        "steps[0].action.max_yaw",
        "must be a number",
      ],
      [
        "a wait past 32 bits",
        { schedule: { type: "every", interval_ms: 4_294_967_296, jitter_ms: 0 } },
        "steps[0].schedule.interval_ms",
        "is out of range",
      ],
      [
        "a wait that isn't whole milliseconds",
        { schedule: { type: "every", interval_ms: 1_000, jitter_ms: 0.5 } },
        "steps[0].schedule.jitter_ms",
        "must be a whole number",
      ],
    ])("refuses %s at its field", async (_name, change, path, message) => {
      expect(await refusals(stepWith(change))).toEqual([[path, message]]);
    });

    it("checks the name, the steps' count and a chat line in the server's words", async () => {
      expect(await refusals({ ...create, name: "   " })).toEqual([["name", "must not be empty"]]);
      expect(await refusals({ ...create, name: "x".repeat(33) })).toEqual([
        ["name", "must be at most 32 characters"],
      ]);
      expect(await refusals({ ...create, name: ` ${"x".repeat(32)} ` })).toEqual([]);
      expect(await refusals({ ...create, steps: Array.from({ length: 33 }, () => step) })).toEqual([
        ["steps", "must hold at most 32 steps"],
      ]);
      expect(await refusals(stepWith({ action: { type: "send_chat", message: "  " } }))).toEqual([
        ["steps[0].action.message", "must not be empty"],
      ]);
    });

    it("refuses an answer's shape: an unknown field or type never reaches the server", async () => {
      const { fake, api } = client();
      const send = (request: unknown) => api.createMode(request as never);

      expect(await refusedFields(send({ ...create, unlisted_command: false }))).toEqual([""]);
      expect(
        await refusedFields(
          send(stepWith({ action: { type: "send_chat", message: "/x", arguments_hidden: true } })),
        ),
      ).toEqual(["steps[0].action"]);
      expect(await refusedFields(send(stepWith({ action: { type: "dance" } })))).toEqual([
        "steps[0].action.type",
      ]);
      expect(await refusedFields(send(stepWith({ schedule: { type: "at_dawn" } })))).toEqual([
        "steps[0].schedule.type",
      ]);
      expect(await refusedFields(send({ ...create, visibility: "secret" }))).toEqual([
        "visibility",
      ]);
      expect(fake.calls).toEqual([]);
    });

    it("refuses an edit's version and ID", async () => {
      const { fake, api } = client();

      expect(await refusedFields(api.updateMode("x", { ...update, version: 0 } as never))).toEqual([
        "id",
        "version",
      ]);
      expect(await refusedFields(api.deleteMode("x"))).toEqual(["id"]);
      expect(fake.calls).toEqual([]);
    });
  });

  it("passes the servers' settings' valid pages and writes through unchanged", async () => {
    const { fake, api } = client();
    // A character outside the BMP counts once, as the server counts it.
    const longest = "😀".repeat(CONFLICT_TEXT_MAX_CHARS);
    const most = Array.from({ length: CONFLICT_TEXTS_MAX }, (_, n) => `Text ${n}`);
    const formats = Array.from({ length: CHAT_FORMATS_MAX }, () =>
      "😀".repeat(CHAT_FORMAT_MAX_CHARS),
    );
    const create = {
      server: "mc.example.com",
      conflict_texts: [longest, "Two\nlines"],
      chat_formats: formats,
    };
    const update = {
      version: 1,
      server: "mc.example.com:25566",
      conflict_texts: most,
      chat_formats: [],
    };

    await api.serverSettings();
    await api.serverSettings(ID, SERVER_SETTINGS_PAGE_MAX);
    await api.createServerSettings(create);
    await api.updateServerSettings(ID, update);
    await api.deleteServerSettings(ID);

    expect(fake.calls.map((call) => [call.method, call.args])).toEqual([
      ["serverSettings", [undefined, undefined]],
      ["serverSettings", [ID, SERVER_SETTINGS_PAGE_MAX]],
      ["createServerSettings", [create]],
      ["updateServerSettings", [ID, update]],
      ["deleteServerSettings", [ID]],
    ]);
  });

  it("refuses a servers' settings page past 10, too long a text, too many, a version below 1 and a bad ID", async () => {
    const { fake, api } = client();
    const tooLong = "x".repeat(CONFLICT_TEXT_MAX_CHARS + 1);
    const tooMany = Array.from({ length: CONFLICT_TEXTS_MAX + 1 }, (_, n) => `Text ${n}`);

    expect(await refusedFields(api.serverSettings("x", SERVER_SETTINGS_PAGE_MAX + 1))).toEqual([
      "before",
      "limit",
    ]);
    expect(
      await refused(
        api.createServerSettings({
          server: "mc.example.com",
          conflict_texts: ["One", tooLong],
          chat_formats: [],
        }),
      ),
    ).toEqual([{ path: "conflict_texts[1]", message: "must be at most 1024 characters" }]);
    expect(
      await refused(
        api.createServerSettings({ server: "", conflict_texts: tooMany, chat_formats: [] }),
      ),
    ).toEqual([
      { path: "server", message: "must not be empty" },
      { path: "conflict_texts", message: "must hold at most 16 texts" },
    ]);
    expect(
      await refused(
        api.createServerSettings({
          server: "mc.example.com",
          conflict_texts: [],
          chat_formats: ["{sender} : ", "x".repeat(CHAT_FORMAT_MAX_CHARS + 1)],
        }),
      ),
    ).toEqual([{ path: "chat_formats[1]", message: "must be at most 64 characters" }]);
    expect(
      await refused(
        api.updateServerSettings(ID, {
          version: 1,
          server: "mc.example.com",
          conflict_texts: ["One"],
          chat_formats: Array.from({ length: CHAT_FORMATS_MAX + 1 }, () => "{sender} : "),
        }),
      ),
    ).toEqual([{ path: "chat_formats", message: "must hold at most 8 formats" }]);
    expect(
      await refusedFields(
        api.updateServerSettings("x", {
          version: 0,
          server: "mc.example.com",
          conflict_texts: ["One"],
          chat_formats: [],
        }),
      ),
    ).toEqual(["id", "version"]);
    expect(await refusedFields(api.deleteServerSettings("x"))).toEqual(["id"]);
    const unknown = {
      server: "mc.example.com",
      conflict_texts: ["One"],
      chat_formats: [],
      highlights: [],
    };
    expect(await refusedFields(api.createServerSettings(unknown))).toEqual([""]);
    expect(fake.calls).toEqual([]);
  });

  it("passes a chat formats' preview at its caps and refuses one past them (R142)", async () => {
    const { fake, api } = client();
    // A character outside the BMP counts once, as the server counts it.
    const longest = {
      chat_formats: Array.from({ length: CHAT_FORMATS_MAX }, () => "{sender} : "),
      line: "😀".repeat(CHAT_PREVIEW_LINE_MAX_CHARS),
    };

    await api.previewChatFormats(longest);

    expect(fake.calls.map((call) => [call.method, call.args])).toEqual([
      ["previewChatFormats", [longest]],
    ]);
    expect(
      await refused(
        api.previewChatFormats({
          chat_formats: ["x".repeat(CHAT_FORMAT_MAX_CHARS + 1)],
          line: "x".repeat(CHAT_PREVIEW_LINE_MAX_CHARS + 1),
        }),
      ),
    ).toEqual([
      { path: "chat_formats[0]", message: "must be at most 64 characters" },
      { path: "line", message: "must be at most 1024 characters" },
    ]);
    expect(
      await refusedFields(
        api.previewChatFormats({ chat_formats: [], line: "", extra: 1 } as never),
      ),
    ).toEqual([""]);
    expect(fake.calls).toHaveLength(1);
  });

  it("passes valid bot writes through unchanged", async () => {
    const { fake, api } = client();
    const address = "a".repeat(SERVER_ADDRESS_MAX_CHARS);

    await api.patchBot(ID, { server_address: "play.example.com:25566" });
    await api.patchBot(ID, { server_address: address });
    await api.setBotMode(ID, { mode_id: ID });
    await api.startBot(ID);
    await api.stopBot(ID);
    await api.restartBot(ID);
    await api.resetBot(ID);
    await api.resumeBot(ID);

    expect(fake.calls.map((call) => [call.method, call.args])).toEqual([
      ["patchBot", [ID, { server_address: "play.example.com:25566" }]],
      ["patchBot", [ID, { server_address: address }]],
      ["setBotMode", [ID, { mode_id: ID }]],
      ["startBot", [ID]],
      ["stopBot", [ID]],
      ["restartBot", [ID]],
      ["resetBot", [ID]],
      ["resumeBot", [ID]],
    ]);
  });

  it("refuses a bot write's bad ID, an empty or too long address and a mode that isn't an ID", async () => {
    const { fake, api } = client();

    expect(await refusedFields(api.patchBot("x", { server_address: "" }))).toEqual([
      "id",
      "server_address",
    ]);
    expect(
      await refusedFields(
        api.patchBot(ID, { server_address: "a".repeat(SERVER_ADDRESS_MAX_CHARS + 1) }),
      ),
    ).toEqual(["server_address"]);
    expect(await refusedFields(api.setBotMode(ID, { mode_id: "x" }))).toEqual(["mode_id"]);
    for (const call of [
      api.startBot("x"),
      api.stopBot("x"),
      api.restartBot("x"),
      api.resetBot("x"),
      api.resumeBot("x"),
    ]) {
      expect(await refusedFields(call)).toEqual(["id"]);
    }
    expect(fake.calls).toEqual([]);
  });

  it("passes the chat's valid pages, lines and commands through unchanged", async () => {
    const { fake, api } = client();
    const longest = "é".repeat(CHAT_MESSAGE_MAX_UNITS);
    const emoji = "😀".repeat(CHAT_MESSAGE_MAX_UNITS / 2);

    await api.botChat(ID);
    await api.botChat(ID, 1, PAGE_MAX);
    await api.sendChat(ID, { message: `  ${longest}  ` });
    await api.sendChat(ID, { message: emoji });
    await api.sendChat(ID, { message: "/spawn" });
    await api.sendCommand(ID, { message: "  /op AfkBot2" });

    expect(fake.calls.map((call) => [call.method, call.args])).toEqual([
      ["botChat", [ID, undefined, undefined]],
      ["botChat", [ID, 1, PAGE_MAX]],
      ["sendChat", [ID, { message: `  ${longest}  ` }]],
      ["sendChat", [ID, { message: emoji }]],
      ["sendChat", [ID, { message: "/spawn" }]],
      ["sendCommand", [ID, { message: "  /op AfkBot2" }]],
    ]);
  });

  it("refuses the chat's bad ID, cursor and limit, an empty or too long line, and a command without its /", async () => {
    const { fake, api } = client();

    expect(await refusedFields(api.botChat("x", 0, 0))).toEqual(["id", "before", "limit"]);
    expect(await refusedFields(api.botChat(ID, 1.5))).toEqual(["before"]);
    expect(await refused(api.sendChat("x", { message: "   " }))).toEqual([
      { path: "id", message: "must be an ID" },
      { path: "message", message: "must not be empty" },
    ]);
    expect(
      await refused(api.sendChat(ID, { message: `${"😀".repeat(CHAT_MESSAGE_MAX_UNITS / 2)}a` })),
    ).toEqual([
      { path: "message", message: "must be at most 256 characters, an emoji counting as 2" },
    ]);
    expect(await refused(api.sendCommand(ID, { message: "hello /spawn" }))).toEqual([
      { path: "message", message: "must be a command, starting with /" },
    ]);
    expect(fake.calls).toEqual([]);
  });

  it("refuses an account ID that isn't one, and a page out of range", async () => {
    const { fake, api } = client();

    for (const call of [
      api.linkStatus("x"),
      api.account("x"),
      api.deleteAccount("x"),
      api.approveAccount("x"),
      api.refuseAccount("x"),
      api.relinkAccount("x"),
    ]) {
      expect(await refusedFields(call)).toEqual(["id"]);
    }
    expect(await refusedFields(api.accounts("x", 0))).toEqual(["before", "limit"]);
    expect(await refusedFields(api.accountsAwaitingApproval(undefined, PAGE_MAX + 1))).toEqual([
      "limit",
    ]);
    expect(await refusedFields(api.accountGrants("x", "y", 0))).toEqual(["id", "before", "limit"]);
    expect(fake.calls).toEqual([]);
  });
});

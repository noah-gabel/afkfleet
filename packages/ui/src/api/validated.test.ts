import { describe, expect, it } from "vitest";

import type { ApiClient } from "./ApiClient";
import { ApiError } from "./ApiError";
import { FakeApiClient } from "./FakeApiClient";
import {
  CA_PEM_MAX_BYTES,
  CODE_MAX_CHARS,
  NEW_PASSWORD_MAX_CHARS,
  PAGE_MAX,
  PASSWORD_MAX_CHARS,
  SERVER_URL_MAX_BYTES,
  TOKEN_MAX_CHARS,
  USERNAME_MAX_CHARS,
} from "./limits";
import { validated } from "./validated";

const ID = "01900000-0000-7000-8000-000000000001";

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
  });
  return { fake, api: validated(fake) as ApiClient };
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
});

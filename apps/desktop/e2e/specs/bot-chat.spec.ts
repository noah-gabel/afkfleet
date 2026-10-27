// A bot's chat as the live events bring it (Plan.md P11.9; ADR-0020, U43, U45, S70): a line
// from the server, and the user's own line, pending until its chat_sent marks it sent.
import { OWNER } from "../fake/backend";
import { expect, fakeBackend, test } from "../fixtures";
import { enterCode, expectHome, logIn, push } from "./steps";

test("a server's line shows, and the user's own line shows pending, then sent", async ({
  page,
  context,
}) => {
  const backend = await fakeBackend(context);
  backend.addAccount(OWNER);
  backend.setBot("AfkBot1", {
    desiredState: "running",
    status: { kind: "reported", state: "online" },
  });
  backend.chatOutcome = "pending";
  await page.goto("/");
  await logIn(page, OWNER);
  await enterCode(page, backend.code(OWNER));
  await expectHome(page, OWNER);
  await page.getByRole("link", { name: "Open AfkBot1" }).click();
  const chat = page.getByRole("main").getByRole("region", { name: "Chat" });
  await expect(chat.getByText("No chat yet.")).toBeVisible();
  const id = backend.bot("AfkBot1").id;

  const said = backend.addChat("AfkBot1", {
    at: new Date().toISOString(),
    direction: "in",
    kind: "system",
    text: "Hello from the server",
    truncated: false,
    arguments_hidden: false,
    mine: false,
  });
  await push(page, { update: "event", event: { type: "chat", bot_id: id, line: said } });
  const fromServer = chat.getByRole("listitem").filter({ hasText: "Hello from the server" });
  await expect(fromServer.getByText("Server", { exact: true })).toBeVisible();

  const field = chat.getByRole("textbox", { name: "Message or /command" });
  await field.fill("hello");
  await chat.getByRole("button", { name: "Send" }).click();
  await expect(field).toHaveValue("");
  const sent = backend.lastSend;
  if (sent === null) {
    throw new Error("the fake kept no send");
  }
  await push(page, {
    update: "event",
    event: { type: "chat", bot_id: id, line: sent.line, request: sent.request },
  });
  const own = chat.getByRole("listitem").filter({ hasText: "hello" });
  await expect(own.getByText("pending", { exact: true })).toBeVisible();
  await push(page, {
    update: "event",
    event: { type: "chat_sent", bot_id: id, request: sent.request },
  });

  await expect(own.getByText("sent", { exact: true })).toBeVisible();
  await expect(own.getByText("pending", { exact: true })).toHaveCount(0);
});

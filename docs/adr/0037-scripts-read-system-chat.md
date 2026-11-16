# 0037. Scripts read chat: system lines, by declared patterns with typed values

- **Status:** Accepted. That scripts read chat, and the six guards under "The user's guards", are the user's decision (2026-11-16); the design below them is mine, under ADR-0025
- **Date:** 2026-11-16
- **Related:** Plan.md Phase 15 (P15.30 to P15.36), §10; [ADR-0036](0036-scripted-modes.md) (scripted modes; its question 2 is answered here); [the script reference](../scripting.md); ADR-0010 (text from a Minecraft server is untrusted); ADR-0020 (S79, S80: chat formats; S3: a bot's chat rate; S11: a line break inside a message); [the threat model](../threat-model.md), "Minecraft servers (B4)"

## Context
Until now one rule held without exception: **chat never triggers an action.** A bot stores and shows what it receives, and nothing a Minecraft server or a player prints moves it. ADR-0036 kept the rule for scripts and asked the user whether to keep it (its question 2).

**The user's answer: scripts do read chat, in Phase 15.** Their use case: every 10 hours, and at every join, a script sends `/balance`, reads the reply, and pays what's above a threshold to their main account with `/pay`. A server confirms a balance only in chat, so nothing else can do it.

This ADR changes the rule to: **chat triggers an action only through a pattern a mode declares, on a system line, as typed values.** Everything else about received text stays as it was: sanitized in core, stored and shown as plain text.

## The user's guards
The six conditions the user set, each with where the design below meets it:

| # | The user's guard | Met by |
|---|---|---|
| 1 | Only system messages, never player chat: the server's chat formats tell player lines apart, private messages included | C2 |
| 2 | Patterns declared in the mode, matched against the whole line, with typed captures only (a number, an amount with separators and k/M suffixes, a word from a closed set); the script gets the pattern's key and its typed values, never raw text | C3, C6 |
| 3 | A reply pattern counts only within a window after the script's own command (`chat.expect(key, ms)`); a pattern that may come at any time is declared as such and flagged as riskier | C4, C5 |
| 4 | A value read from chat can fill only a typed, bounded template parameter; a recipient or any other name is always a literal of the declared template, never from chat | C6 |
| 5 | Every match and every send is a line in the script's log in the app | C7 |
| 6 | A new ADR, threat-model rows for spoofed lines (a broadcast, a nickname, a private message, a renamed item), and live tests that a spoofed line from a player triggers nothing | This ADR; "Threats"; C9 |

## Decisions

### C1. Where it's built: in Phase 15's groups, not in a group of its own
The user left the choice: where it fits Phase 15's groups, or a group of its own after E.

**In the groups.** Each layer gets its part in the pull request that builds that layer:

| Group | Task | What |
|---|---|---|
| A | P15.30 | fleet-core: the pattern, the amount parser, which lines are candidates, the reply windows. This ADR; the threat model's rows |
| B | P15.31 | fleet-script: `chat.expect`, `on_chat(key, values)`, the fuzz target `chat_pattern`, the balance example as a file |
| C | P15.32 | fleet-mc, live: what a player's chat, whisper, `/me`, a renamed item's death line and a `tellraw` line arrive as |
| D | P15.33 | fleet-runtime: windows on tokio's time, opened when the command is sent; every match and send in the log |
| E | P15.34 | The wire and the server: a server's chat formats in a bot's spec; patterns validated at save; a mode with an any-time pattern flagged |
| F | P15.35 | The app: the patterns in the mode editor, the any-time warning, the log's lines |
| G | P15.36 | Live: the balance example on the test server, with a second player's spoofed lines triggering nothing |

**Why:**
1. **Patterns are part of a mode's settings,** in its stored JSON and on the wire. Group A defines that format; a group after E would change a format four groups had already built on.
2. **Each guard is tested with its layer's tools:** the matcher with property tests and a fuzz target, the windows with paused time, the candidates against a real server.
3. **A group after E would reopen five crates in one pull request,** larger than any of the seven and harder to review than seven parts.

**What it costs:** no single pull request shows the whole feature. This ADR does; each group's pull request names the guards it builds, and group G's check runs them together.

### C2. Which lines are candidates
fleet-core decides it, in one function, from a received message and the server's chat formats. A message is a candidate only if **all four** hold:

1. **It's a system message** (`ChatKind::System`). A player's chat, a `/me`, a vanilla whisper (`/msg`, `/tell`) and a `/say` arrive as player messages with their own kinds (Minecraft's chat-type registry), and are never matched.
2. **None of the server's chat formats fits it** (ADR-0020, S80). On servers where a plugin prints player chat as system text, a format such as `[{any}] {sender} : ` names those lines as a player's; a private message a plugin prints (`[{sender} -> me] `) is declared the same way. A line a format fits is a player's line and is never matched.
3. **It's one line.** A message with a line break in it never matches: no part of a longer message can pass as a line of its own (ADR-0020, S11's concern, for patterns).
4. **It arrived whole.** A message cut at the 1,024 characters fleet-core keeps never matches.

The line is then compared with its edge whitespace trimmed.

**The chat formats must reach the agent:** today only the server holds them, for display. Group E sends them in each bot's spec, as the chat rate is (ADR-0020, S3). A standalone agent has none.

### C3. Patterns
A mode declares up to **8** patterns, each with a key. A pattern is literal text with up to **4** captures, written `{name}`; `{{` and `}}` are a literal brace, as in chat formats.

```json
{ "key": "balance", "when": "reply", "text": "Balance: ${amount}",
  "captures": [{ "name": "amount", "type": "amount", "min": 0, "max": 1000000000, "decimal_mark": "." }] }
```

- **The whole line must fit:** matching is anchored at both ends, literal text is compared exactly, case included.
- **Typed captures only.** There is no capture for free text and no wildcard:

  | Type | Reads | The script gets |
  |---|---|---|
  | `integer` | An optional `-` and 1 to 15 digits | A whole number |
  | `amount` | An optional `-`; digits, with or without thousands separators in groups of exactly three; an optional decimal part of 1 to 4 digits; an optional suffix `k`, `M` or `B` (either case) that isn't followed by a letter or a digit. The decimal mark is declared, `.` or `,`; the separator is the other one | A decimal number |
  | `word` | One of up to 32 declared words, the longest that fits | That word |

- **Bounded.** Every number capture declares its range; a value outside it is no match. An amount is at most 10^15 after its suffix.
- **Deterministic and linear.** Each capture reads the longest text its grammar allows and is never tried again shorter. Two captures are never side by side, and a pattern holds at least 4 literal characters that aren't spaces, so a pattern is never "any number" alone.
- **A pattern's own text** is at most 128 characters, one line, and holds no character received chat never has (as a chat format).
- **The first declared pattern that may count now and fits** wins.

**One parser for amounts.** Phase 16's price helpers (P16.7) use the same one, so `B` is in it from the start; the user named `k` and `M` for chat, and one more suffix lets no other kind of line through.

### C4. A reply counts only in its window
A pattern declared `"when": "reply"` counts only while a window is open for it.

- **`chat.expect(key, ms)` opens one,** for 250 to 30,000 ms. It must follow a `chat.send` of a declared **command** in the same hook, or it returns `no_command`: the window belongs to the script's own command, and a script can't keep one open by itself.
- **The window opens when the command is handed to the server,** not when the hook asks: the bot's chat queue may send it later. If the command isn't sent (the queue is full, the session can't sign it), no window opens, and the log says so.
- **One reply an `expect`.** The first match closes the window.
- **Windows close at a disconnect** and when the script stops.
- **A window that runs out** is a log line, and nothing else: a script that wants to react sets a timer.

**What a window is worth:** a spoofed line must arrive within seconds of a command whose time the attacker doesn't choose. The user's case opens 5 seconds every 10 hours.

**Known limit:** a script can send a declared command as often as the bot's chat rate allows and open a window each time. Every send is a log line (C7), and the mode's author is an Admin or the Owner (ADR-0036, D15).

### C5. A pattern that may come at any time
Declared `"when": "any_time"`, a pattern counts whenever the bot is online, with no window. `when` has no default: every pattern says which it is.

**Flagged as riskier, wherever the mode is shown or changed:**
- the API's answer for the mode says it reads chat at any time, and the editor shows a warning at the pattern (group F);
- the audit entry of a mode's create and edit records how many any-time patterns it holds (group E);
- the reference tells authors to prefer a reply.

**At most one match a second for each such pattern;** further ones are dropped and counted, so a flood of lines costs a script neither its operation budget nor its log.

### C6. What reaches the script, and what a value can do
- **`fn on_chat(key, values)`** gets the pattern's key and a map of its captures by name: numbers, and words out of the pattern's own declared list. **No text a server or a player wrote reaches a script.**
- **A value can fill a declared message's typed value** (ADR-0036, D8), which is bounded by its own range, and nothing else in a message. So whatever chat says:
  - the bot sends only messages its mode declares;
  - a recipient, a command's name and every other word are text of the declaration, or one of a declared closed list;
  - a number is inside its declared range, or the send returns `bad_value`.
- **Stated plainly:** a script can branch on a value (`if values.amount > 10000`), a captured word included. What chat can change is *which* declared message is sent, *when*, and *which number* in its range. It can't add a word.

### C7. The log
Every one of these is a line in the script's log, shown on the bot's page (P15.18, P15.26):
- a declared message sent, with its key and values;
- a window opened, and one that ran out;
- a match, with the pattern's key and its values;
- a command that wasn't sent, and a `chat.expect` refused.

They're written by the runtime, not by the script, and don't count against the script's own 20 lines a minute: their number is bounded by the chat rate and by C5's one a second.

### C8. Rates
- **Plain chat keeps 30 s** between a script's sends, as a step mode's chat does.
- **A declared command goes by the bot's own chat rate** (ADR-0020, S3), as a command typed in the app does: the user's answer to ADR-0036's question 4. The limiter passes it, and the bot's chat queue paces it. Declared with values or without, it's the same.
- **Timers reach 24 hours** (250 ms to 86,400,000 ms), the longest interval of a step mode, so "every 10 hours" is one timer.

### C9. Tests
- **Properties** (fleet-core): a line with a player's name before it or text after it never fits; a match's values are inside their declared ranges and lists; no player message and no line a chat format fits is ever a candidate; whatever a script and the runtime do, in any order, a reply matches only inside a window a sent command opened, once for each send.
- **A fuzz target,** `chat_pattern` (group B): any pattern declaration and any line; no panic, bounded work, and the properties above.
- **Live, on the test server** (groups C and G): a second player sends the reply's exact text as chat, as a whisper and with `/me`, dies to an item renamed to it, and joins under a name as close to it as a name can be; the script's `/pay` is never sent. Then the server prints the line itself (`tellraw`), inside the window and outside it: only the first is matched.

## Threats
The rows the threat model gains under "Minecraft servers (B4)", each with the tests that hold it:

| Spoofed by | Why it doesn't match |
|---|---|
| **A player's chat line,** on a vanilla server | It's a player message, never a candidate (C2.1) |
| **A player's chat line, printed by a plugin as system text** | A chat format names it a player's line (C2.2). Without a format for it, the line still starts with the player's name or tag, and the whole line must fit (C3) |
| **A private message** | A vanilla whisper is a player message (C2.1); a plugin's is a line a chat format names (C2.2), and it starts with its sender either way |
| **A broadcast** a player causes (an auction, a death, a join) | The player's text is a part of the server's line, never the whole of it (C3); a second line inside it never counts (C2.3) |
| **A nickname** | It's printed inside a line (`… joined the game`), never as one |
| **A renamed item** | It's printed inside a death or broadcast line, never as one |
| **Any of them, by luck or by a plugin that prints a player's text as a whole system line** | A reply's window is seconds after the script's own command (C4); what a match can cause is one of the mode's declared messages with a number in its range (C6); every match and send is in the log (C7) |

## What stays possible
Recorded in the threat model's accepted risks:
- **A plugin that prints text a player chose as a whole system line** (a `/broadcast` a player may use, a sign-to-chat plugin) lets that player print a matching line. For a reply pattern they must hit its window; for an any-time pattern they needn't. What they gain is what the declared messages can do: in the user's case, the bot pays its own balance's excess to the user's own main account, or `/pay` fails because the balance isn't there.
- **A hostile or compromised Minecraft server** prints whatever it likes, as it can already kick, move and kill a bot. Patterns give it one more lever, bounded the same way.
- **A server without chat formats for its plugin's player lines** relies on C3 alone for those lines.
- **A value is only what the server printed.** A script that pays "the balance above X" trusts the server's number.

## Alternatives considered
- **A group of its own after E.** C1.
- **Inventory and menu state only** (ADR-0036's recommendation). The user's case can't be built on it: a balance is only ever in chat.
- **Regular expressions.** Familiar, and unbounded in what they accept and in what they cost to match; a captured group would be raw text. Rejected for typed captures.
- **A free-text capture, sanitized.** It would put a player's words into a script's hands and, through a template, into a command. The user's guard 2 rules it out.
- **A window opened by `chat.expect` alone,** without a command before it. A timer could then keep one open for ever, which is an any-time pattern that isn't declared as one.
- **A reply bound to its command in the declaration** (`"reply_to": "balance"`), with no `chat.expect`. One concept fewer, but the window's length and whether to listen at all would no longer be the script's to say, and the user named `chat.expect(key, ms)`.
- **Taint tracking,** so a value from chat could never choose a declared word. Rhai offers no way to follow a value through a branch; C6 states what holds instead.

## Consequences
- **The rule "chat never triggers an action" has one declared exception,** and the threat model says so.
- **A mode's JSON, the wire and the editor carry patterns;** a bot's spec carries its server's chat formats.
- **The agent matches lines,** so a hostile server's text now reaches one more parser. It's bounded (a line of at most 1,024 characters against at most 8 patterns of 128), linear, and fuzzed.
- **Phase 16's group G is gone:** its one task, system messages by declared patterns, is this.

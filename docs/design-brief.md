# afkfleet's design brief (Plan.md P11.11)

The starting point of the design pass. The decisions that follow from it are in [ADR-0020](adr/0020-bot-operations-and-phase-11-refinements.md), group L1.

## Who uses it
The project's owner and their friends, on Windows, each signing in to a shared server with invite-only accounts and mandatory 2FA. The window often sits beside Minecraft, so it's often small: today's minimum is 800×600. Most visits are short: is my bot online, why not, start it, read or send a line of chat.

## What each area is for
| Area | Its job |
|---|---|
| **The frame** (nav, header, banner, toasts) | Show where you are, who you are and whether the server answers, and take as little room as it can |
| **Before signing in** (log in, the code step, register, 2FA setup, recovery codes, server settings) | Get a friend in safely, once, without help |
| **Home** | Every bot at a glance: its state, what it's doing, and the one action it needs |
| **A bot's detail** | Why it's in that state, its controls and settings, its timeline, and the chat console |
| **Accounts** | Link a Microsoft account, see each account's state, and who has access |
| **Modes** | Say what a bot does: the list, the step editor, and a mode you can only read |
| **Minecraft servers** | Per-server texts and chat formats, tried on a line before saving |
| **Users, the grants review, Invites** | Admit people, set roles, and see what a role change leaves behind |
| **Agents, the audit log** | Read and act on long technical tables |
| **Profile** | The theme, the password, 2FA, sessions |

## What's wrong today
**The user's words.** After group H3: the app looks poor throughout; button placement, spacing and general styling need rework on every screen.

**The user's notes, area by area (U86).** Button placement, spacing and layout, and styling bother them in all ten areas. Home is the one exception, with styling and placement only. On the code step, in their words:

> "One thing mostly for the starting login screen again: i don't like the 2FA code input field. It would be nicer if it was a bigger like 6box input where you atomatically jump to the newxt one as soon as its filled and it submits as soon as its filled"

**And on Home (U88):** a small Start or Stop button on each card where the viewer may control that bot, with the rest of the card still opening the bot.

**Measured or found in the code:**
- WCAG AA misses, from group K's figures: muted text on muted 4.34 in light, destructive on its tint 3.99 in light, the ring against the background 2.59 in light, and `ring/50` as actually used 1.54 and 1.87.
- A 192 px nav under a full-width header takes a large share of a small window.
- The agents' table has nine columns and the audit's seven, too wide for a small window.
- An error toast looks like any other toast.
- There are no status colours: a bot's state is only a badge's variant, so states are hard to tell apart at a glance.
- A bot tied to a stopped agent shows only "Agent offline" on Start. Nothing says it waits for that agent until it returns or is released.
- Home's loading state is one grey line, and the bot screen's is an empty page.
- The app's name in the header and the icon are placeholders: three white dots on a green square.

## Constraints
These hold for every direction.
- **Bundled only:** fonts and images are served from the app, since the CSP allows only `'self'`.
- **No runtime `<style>`:** the style guard holds.
- **Tokens only:** every colour is a token in both `:root` and `.dark`, and `scripts/theme-guard.mjs` stays green.
- **`--background` stays hex,** kept equal to `window_theme.rs`'s constants by a test.
- **Native selects** only through `NativeSelect`. Their open lists aren't in the DOM, so they stay a hands-on check in both themes.
- **WCAG AA in both themes,** with visible focus rings, as an enforced check.
- **The theme control stays on Profile.**
- **The branding is our own:** nothing from Minecraft or Mojang and nothing that looks like it (no grass block, creeper, Minecraft font or textures), no other brand's marks, nothing generated from a known logo.
- **Decisions that stay:** ADR-0017's frontend decisions, U39's two columns on a bot's screen, and U35's whole card opening the bot, apart from U88's one button.

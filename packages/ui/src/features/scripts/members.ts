// What the compiler can't see (Plan.md P15.25; ADR-0036, D14; ADR-0045): a member of a game
// object that doesn't exist (`keys.forwrd`, `mouse.left.hodl()`), a method written without its
// parentheses or a value with them, and a declared message or reply pattern a script names that
// the mode doesn't declare. Rhai finds each only when the line runs, and the script stops there.
// Read from the generated description and the same tokenizer as the highlighting. Pure.
import type { ScriptApiObject } from "../../generated/ScriptApiObject";
import { memberOf, objectNamed } from "./api";
import { type Token, tokens } from "./rhai";
import {
  notAMethod,
  notCalled,
  undeclaredMessage,
  undeclaredReply,
  undeclaredSign,
  unknownMember,
} from "./texts";

/** What the mode declares, for a script's `chat.send`, `chat.expect` and `sign.write`. */
export interface Declared {
  /** The declared signs' keys; none known when it's left out. */
  readonly signs?: readonly string[];
  /** The declared messages' keys. */
  readonly messages: readonly string[];
  /** The keys of the patterns declared as replies. */
  readonly replies: readonly string[];
}

/** A problem at a place in the text, in UTF-16 offsets. */
export interface MemberProblem {
  readonly from: number;
  readonly to: number;
  readonly message: string;
}

/** A text token's contents, quotes and escapes aside, when it's a plain "…" string. */
function literal(token: Token | undefined): string | undefined {
  if (token?.type !== "string" || !/^"(?:[^"\\]|\\.)*"$/.test(token.text)) {
    return undefined;
  }
  return token.text.slice(1, -1).replace(/\\(.)/g, "$1");
}

/** Whether `token` opens a call. */
function opensCall(token: Token | undefined): boolean {
  return token?.type === "bracket" && token.text === "(";
}

/** A declared key `chat.send` or `chat.expect` names, if the mode doesn't declare it. */
function chatProblem(
  method: string,
  argument: Token | undefined,
  declared: Declared,
): MemberProblem | null {
  const key = literal(argument);
  if (argument === undefined || key === undefined) {
    return null;
  }
  if (method === "send" && !declared.messages.includes(key)) {
    return { from: argument.from, to: argument.to, message: undeclaredMessage(key) };
  }
  if (method === "expect" && !declared.replies.includes(key)) {
    return { from: argument.from, to: argument.to, message: undeclaredReply(key) };
  }
  return null;
}

/** A declared sign's key `sign.write` names, if the mode doesn't declare it. */
function signProblem(argument: Token | undefined, signs: readonly string[]): MemberProblem | null {
  const key = literal(argument);
  if (argument === undefined || key === undefined || signs.includes(key)) {
    return null;
  }
  return { from: argument.from, to: argument.to, message: undeclaredSign(key) };
}

/** The problem of the chain that starts at `all[start]`, a game object, if it has one. */
function chainProblem(
  all: readonly Token[],
  start: number,
  first: ScriptApiObject,
  declared: Declared | undefined,
): MemberProblem | null {
  let object: ScriptApiObject = first;
  let chain = first.name;
  let at = start + 1;
  while (all[at]?.type === "dot" && all[at + 1]?.type === "member") {
    const name = all[at + 1] as Token;
    const member = memberOf(object, name.text);
    if (member === undefined) {
      return { from: name.from, to: name.to, message: unknownMember(chain, name.text) };
    }
    chain = `${chain}.${member.name}`;
    const called = opensCall(all[at + 2]);
    if (member.kind === "method" && !called) {
      return { from: name.from, to: name.to, message: notCalled(chain, member.signature) };
    }
    if (member.kind === "property" && called) {
      return { from: name.from, to: name.to, message: notAMethod(chain) };
    }
    if (object.name === "chat" && declared !== undefined) {
      return chatProblem(member.name, all[at + 3], declared);
    }
    if (object.name === "sign" && member.name === "write" && declared?.signs !== undefined) {
      return signProblem(all[at + 3], declared.signs);
    }
    const next = member.object === undefined ? undefined : objectNamed(member.object);
    if (next === undefined) {
      return null;
    }
    object = next;
    at += 2;
  }
  return null;
}

/**
 * Every problem of a game object's chain in `text`; with `declared`, also every declared key that
 * `chat.send` or `chat.expect` names and the mode doesn't declare.
 */
export function memberProblems(text: string, declared?: Declared): MemberProblem[] {
  const all = tokens(text).filter((token) => token.type !== "comment");
  const problems: MemberProblem[] = [];
  all.forEach((token, index) => {
    // A name after a dot is a member, never the start of a chain (`state.keys`).
    const object = token.type === "object" ? objectNamed(token.text) : undefined;
    if (object !== undefined) {
      const problem = chainProblem(all, index, object, declared);
      if (problem !== null) {
        problems.push(problem);
      }
    }
  });
  return problems;
}

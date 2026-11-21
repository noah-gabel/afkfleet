// The editor's completion (Plan.md P15.25; ADR-0045), from the generated description alone:
// - after a game object's dot, its members, each with its signature and summary; a method's
//   parameters become fields to fill in (`hold_for(ms)`), and after `keys.forward.` an input's;
// - `on_` at a line's start, or after `fn `, offers each hook with its parameters and an empty
//   body (`fn on_join(first) { }`);
// - any other name offers the game objects.
// Nothing inside a comment or a text. Pure but for CodeMirror's types.
import {
  type Completion,
  type CompletionContext,
  type CompletionResult,
  snippetCompletion,
} from "@codemirror/autocomplete";
import { syntaxTree } from "@codemirror/language";

import type { ScriptApiMember } from "../../generated/ScriptApiMember";
import { GAME_OBJECTS, HOOK_MEMBERS, objectNamed, resolveChain } from "./api";

/** The chain of names before a dot at the end of `before`: `keys.forward.` is `["keys", "forward"]`. */
function chainBefore(before: string): string[] | null {
  const match = /((?:[A-Za-z_]\w*\s*\.\s*)+)$/.exec(before);
  return match?.[1] === undefined
    ? null
    : match[1]
        .split(".")
        .map((name) => name.trim())
        .filter((name) => name !== "");
}

/** A snippet's text for `signature`: each parameter a field, `hold_for(${ms})`. */
function callTemplate(signature: string): string {
  return signature.replace(/\(([^)]*)\)/, (_, parameters: string) => {
    const fields = parameters
      .split(",")
      .map((parameter) => parameter.trim())
      .filter((parameter) => parameter !== "")
      .map((parameter) => `\${${parameter}}`);
    return `(${fields.join(", ")})`;
  });
}

/** `member` as an option after a dot: a method with its signature beside it, a value without. */
function memberOption(member: ScriptApiMember): Completion {
  const common = { label: member.name, info: member.summary };
  return member.kind === "method"
    ? snippetCompletion(callTemplate(member.signature), {
        ...common,
        detail: member.signature,
        type: "method",
      })
    : { ...common, type: "property" };
}

/** A hook as an option: its definition with its parameters and an empty body. */
function hookOption(hook: ScriptApiMember, afterFn: boolean): Completion {
  const definition = afterFn ? hook.signature.replace(/^fn /, "") : hook.signature;
  return snippetCompletion(`${definition} {\n\t\${}\n}`, {
    label: hook.name,
    detail: hook.signature,
    info: hook.summary,
    type: "function",
  });
}

/** The editor's completions at `context`; see the module comment. */
export function scriptCompletions(context: CompletionContext): CompletionResult | null {
  const node = syntaxTree(context.state).resolveInner(context.pos, -1);
  if (node.name === "comment" || node.name === "string") {
    return null;
  }
  const word = context.matchBefore(/[A-Za-z_]\w*/);
  const from = word?.from ?? context.pos;
  const line = context.state.doc.lineAt(context.pos);
  const before = line.text.slice(0, from - line.from);

  const chain = chainBefore(before);
  if (chain !== null) {
    const object = resolveChain(chain);
    return object === undefined
      ? null
      : { from, options: object.members.map(memberOption), validFor: /^\w*$/ };
  }
  const typed = word?.text ?? "";
  if (typed.startsWith("on")) {
    const afterFn = /\bfn\s+$/.test(before);
    if (afterFn || /^\s*$/.test(before)) {
      return {
        from,
        options: HOOK_MEMBERS.map((hook) => hookOption(hook, afterFn)),
        validFor: /^\w*$/,
      };
    }
  }
  if (word === null && !context.explicit) {
    return null;
  }
  return {
    from,
    options: [...GAME_OBJECTS].map((name) => ({
      label: name,
      type: "variable",
      info: objectNamed(name)?.summary ?? "",
    })),
    validFor: /^\w*$/,
  };
}

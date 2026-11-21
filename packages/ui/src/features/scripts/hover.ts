// The editor's hover help (Plan.md P15.25; ADR-0045): over a game object its summary, over a
// member its signature as written from its object and its summary, over a hook's name in its
// definition the hook's. From the generated description and the highlighting's tokenizer.
import { hoverTooltip } from "@codemirror/view";

import { HOOK_MEMBERS, memberOf, objectNamed, resolveChain } from "./api";
import { type Token, tokens } from "./rhai";

/** What the help over a name shows. */
export interface Help {
  readonly from: number;
  readonly to: number;
  /** How it's written: `keys.forward.hold_for(ms)`. */
  readonly signature: string;
  readonly summary: string;
}

/** The names of the chain that ends at `all[at]`, a member, from its game object on. */
function chainTo(all: readonly Token[], at: number): string[] | null {
  const names = [(all[at] as Token).text];
  let index = at - 1;
  while (all[index]?.type === "dot") {
    const previous = all[index - 1];
    if (previous?.type === "object") {
      return [previous.text, ...names];
    }
    if (previous?.type !== "member") {
      return null;
    }
    names.unshift(previous.text);
    index -= 2;
  }
  return null;
}

/** The help over `offset` in `text`, if a name the description knows is there. */
export function helpAt(text: string, offset: number): Help | null {
  const all = tokens(text).filter((token) => token.type !== "comment");
  const at = all.findIndex((token) => token.from <= offset && offset < token.to);
  const token = all[at];
  if (token === undefined) {
    return null;
  }
  const { from, to } = token;
  switch (token.type) {
    case "object": {
      const object = objectNamed(token.text);
      return object === undefined
        ? null
        : { from, to, signature: object.name, summary: object.summary };
    }
    case "member": {
      const chain = chainTo(all, at);
      const object = chain === null ? undefined : resolveChain(chain.slice(0, -1));
      const member = object === undefined ? undefined : memberOf(object, token.text);
      if (chain === null || member === undefined) {
        return null;
      }
      const owner = chain.slice(0, -1).join(".");
      return { from, to, signature: `${owner}.${member.signature}`, summary: member.summary };
    }
    case "definition": {
      const hook = HOOK_MEMBERS.find((one) => one.name === token.text);
      return hook === undefined
        ? null
        : { from, to, signature: hook.signature, summary: hook.summary };
    }
    default:
      return null;
  }
}

/** The help as the editor's hover, its text set as text, never as markup. */
export const scriptHover = hoverTooltip((view, pos) => {
  const help = helpAt(view.state.doc.toString(), pos);
  if (help === null) {
    return null;
  }
  return {
    pos: help.from,
    end: help.to,
    above: true,
    create: () => {
      const dom = document.createElement("div");
      dom.className = "cm-script-help";
      const signature = document.createElement("code");
      signature.textContent = help.signature;
      const summary = document.createElement("p");
      summary.textContent = help.summary;
      dom.append(signature, summary);
      return { dom };
    },
  };
});

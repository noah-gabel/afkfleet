import type { Active, Over } from "@dnd-kit/core";
import { describe, expect, it } from "vitest";

import { dragAnnouncements } from "./dragging";

const IDS = ["a", "b", "c"] as const;
const active = (id: string) => ({ id }) as unknown as Active;
const over = (id: string) => ({ id }) as unknown as Over;

describe("dragAnnouncements", () => {
  const said = dragAnnouncements(IDS);

  it("says which step was picked up, counted from 1", () => {
    expect(said.onDragStart({ active: active("b") })).toBe("Picked up step 2.");
  });

  it("says where it goes, over a step or dropped", () => {
    const arguments_ = { active: active("c"), over: over("a") };

    expect(said.onDragOver(arguments_)).toBe("Step 3 moved to position 1 of 3.");
    expect(said.onDragEnd(arguments_)).toBe("Step 3 moved to position 1 of 3.");
  });

  it("says it's back when it's over no step or the drag is cancelled", () => {
    expect(said.onDragOver({ active: active("c"), over: null })).toBeUndefined();
    expect(said.onDragEnd({ active: active("c"), over: null })).toBe("Put back step 3.");
    expect(said.onDragCancel({ active: active("c"), over: over("a") })).toBe("Put back step 3.");
  });
});

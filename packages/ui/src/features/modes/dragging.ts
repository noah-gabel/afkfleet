// What a screen reader hears while a step is dragged (Plan.md P11.9; ADR-0020, U68, R167): the
// steps by position in their section, counted from 1. A pick-up is at once "over" the step itself,
// so that says the pick-up, not a move; a drop there is no move either.
import type { Announcements } from "@dnd-kit/core";

import { pickedUp, putBack, stepMoved } from "./texts";

/** The announcements for a section whose steps have the IDs `ids`, in order. */
export function dragAnnouncements(ids: readonly (string | number)[]): Announcements {
  const position = (id: string | number) => ids.indexOf(id) + 1;
  const moved = (active: string | number, over: string | number) =>
    stepMoved(position(active), position(over), ids.length);
  return {
    onDragStart: ({ active }) => pickedUp(position(active.id)),
    onDragOver: ({ active, over }) => {
      if (over === null) {
        return undefined;
      }
      return over.id === active.id ? pickedUp(position(active.id)) : moved(active.id, over.id);
    },
    onDragEnd: ({ active, over }) =>
      over === null || over.id === active.id
        ? putBack(position(active.id))
        : moved(active.id, over.id),
    onDragCancel: ({ active }) => putBack(position(active.id)),
  };
}

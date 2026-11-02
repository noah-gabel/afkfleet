// One screen back, by the frame's button, the mouse's side-button or Alt+Left (Plan.md P11.11;
// ADR-0020, U111, U112, S126), through the router's own history, so a screen keeps its search.
//
// History starts fresh at sign-in: the frame mounts at sign-in and unmounts at /login, so the index it
// mounted at is the floor of this sign-in, whichever way it began. The router's `canGoBack()` only
// tests for the very first entry, which a second login in the same App is long past. A press below
// the floor does nothing, so nothing here can reach a sign-in screen, and `back()` runs at once, so
// repeated presses read the fresh index.
//
// A screen left because what it showed is gone replaces its entry, which can leave the same screen
// twice in a row, so going back passes over any entry that shows what's already showing.
//
// The side-buttons are taken from the browser even with nothing to go back to: WebView2 has a history
// of its own, and only this one may move.
import { useLocation, useRouter } from "@tanstack/react-router";
import { useCallback, useEffect, useRef, useState } from "react";

import { useAppStores, useBackHold } from "../../../app/context";

/** The mouse's back and forward side-buttons, as `MouseEvent.button` reports them. */
const SIDE_BACK = 3;
const SIDE_FORWARD = 4;

/** Focuses the screen's heading, or the screen, so a screen reader hears where it landed. */
function focusScreen(): void {
  const target = document.querySelector<HTMLElement>("main h1") ?? document.querySelector("main");
  if (target instanceof HTMLElement) {
    target.setAttribute("tabindex", "-1");
    target.classList.add("outline-none");
    target.focus({ preventScroll: true });
  }
}

/** Whether a Back is on offer now, and how to take it. */
export function useBack(): { readonly canGoBack: boolean; readonly goBack: () => void } {
  const router = useRouter();
  const { session, back } = useAppStores();
  const [floor] = useState(() => router.history.location.state.__TSR_index);
  const index = useLocation({ select: (location) => location.state.__TSR_index });
  const held = useBackHold((state) => state.held);
  /** Focus goes to the screen once it has rendered after a Back. */
  const landing = useRef(false);

  const goBack = useCallback(() => {
    const { history } = router;
    if (session.getState().phase !== "signed_in" || back.getState().held) {
      return;
    }
    const from = history.location.href;
    while (history.location.state.__TSR_index > floor) {
      landing.current = true;
      history.back();
      if (history.location.href !== from) {
        return;
      }
    }
  }, [router, session, back, floor]);

  useEffect(
    () =>
      router.subscribe("onRendered", () => {
        if (landing.current) {
          landing.current = false;
          focusScreen();
        }
      }),
    [router],
  );

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (
        event.key === "ArrowLeft" &&
        event.altKey &&
        !event.ctrlKey &&
        !event.shiftKey &&
        !event.metaKey &&
        !event.isComposing &&
        !event.repeat
      ) {
        event.preventDefault();
        goBack();
      }
    };
    const onMouse = (event: MouseEvent) => {
      if (event.button !== SIDE_BACK && event.button !== SIDE_FORWARD) {
        return;
      }
      event.preventDefault();
      if (event.type === "mouseup" && event.button === SIDE_BACK) {
        goBack();
      }
    };
    window.addEventListener("keydown", onKey, true);
    for (const type of ["mousedown", "mouseup", "auxclick"] as const) {
      window.addEventListener(type, onMouse, true);
    }
    return () => {
      window.removeEventListener("keydown", onKey, true);
      for (const type of ["mousedown", "mouseup", "auxclick"] as const) {
        window.removeEventListener(type, onMouse, true);
      }
    };
  }, [goBack]);

  return { canGoBack: index > floor && !held, goBack };
}

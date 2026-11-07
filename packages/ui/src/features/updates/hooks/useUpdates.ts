// The app's updates (Plan.md P12.5; ADR-0022, U27, U28, S27): one check at each start, a check on
// demand, the install, and Try again. Both calls go to the app's Rust side, which asks GitHub,
// never the server: neither is watched, so neither touches the banner or the session (watched.ts).
// What they found lives in the App's `update` store, never the query cache (stores.ts).
import { useEffect } from "react";

import { toApiError } from "../../../api/ApiError";
import { useApiClient } from "../../../api/context";
import { useAppStores } from "../../../app/context";
import type { CommandError } from "../../../generated/CommandError";
import type { UpdateCheck } from "../../../generated/UpdateCheck";

/** `no_update`'s fixed sentence: `CommandErrorCode::NoUpdate.message()`'s. */
const NO_UPDATE: CommandError = {
  code: "no_update",
  message: "There's no update to install. Check again.",
};

/** The updates' actions. */
export interface Updates {
  /** Checks now; `asked` when the user asked, so a failure says why. Its answer, or null. */
  check(asked: boolean): Promise<UpdateCheck | null>;
  /** Installs what the last check found: the app closes, and starts again once it's installed. */
  install(): Promise<void>;
  /** After a failed install: checks again, and installs what it finds. */
  tryAgain(): Promise<void>;
}

/** The actions over this App's client and stores. */
export function useUpdates(): Updates {
  const api = useApiClient();
  const stores = useAppStores();
  const store = stores.update;

  async function check(asked: boolean): Promise<UpdateCheck | null> {
    store.getState().beginCheck();
    try {
      const answer = await api.checkUpdate();
      store.getState().checked(answer);
      return answer;
    } catch (rejection) {
      store.getState().checkFailed(asked ? toApiError(rejection).error : null);
      return null;
    }
  }

  async function install(): Promise<void> {
    store.getState().beginInstall();
    try {
      // The real install ends with the app's exit; it answers only when it failed.
      await api.installUpdate();
    } catch (rejection) {
      const failure = toApiError(rejection).error;
      store.getState().installFailed(failure);
      // The Rust side stops the live events watch only once the download is checked, and answers
      // `update_install` for every failure after that: the watch starts again then alone (S29).
      if (failure.code === "update_install") {
        stores.live.getState().restart();
      }
    }
  }

  async function tryAgain(): Promise<void> {
    // A failed install keeps nothing: the release is checked again first.
    const answer = await check(true);
    if (answer?.state === "available") {
      await install();
      return;
    }
    store.getState().installFailed(store.getState().checkFailure ?? NO_UPDATE);
  }

  return { check, install, tryAgain };
}

/** Runs the start's one check, quietly, the first time the App mounts this (U27). */
export function useStartCheck(): void {
  const store = useAppStores().update;
  const { check } = useUpdates();
  useEffect(() => {
    if (store.getState().start()) {
      void check(false);
    }
  });
}

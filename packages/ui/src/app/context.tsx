// What the router's routes and the app's components share (Plan.md P8.6): the router
// context, and the UI stores (stores.ts) for components, including those outside the
// router (the toaster, the error boundary).
import type { QueryClient } from "@tanstack/react-query";
import { createContext, type ReactNode, useContext } from "react";
import { useStore } from "zustand";

import type { ApiClient } from "../api/ApiClient";
import type { AppStores, ConnectionState, NoticesState, SessionState } from "./stores";

/** The router's context: what every route's `beforeLoad` and `loader` receive. */
export interface RouterContext {
  readonly queryClient: QueryClient;
  /** The app's client: validated and watched. */
  readonly apiClient: ApiClient;
  readonly stores: AppStores;
}

const StoresContext = createContext<AppStores | null>(null);

/** Makes `stores` the UI stores of everything inside. */
export function StoresProvider({ stores, children }: { stores: AppStores; children: ReactNode }) {
  return <StoresContext value={stores}>{children}</StoresContext>;
}

/** The UI stores `StoresProvider` provides; throws outside one. */
export function useAppStores(): AppStores {
  const stores = useContext(StoresContext);
  if (stores === null) {
    throw new Error("useAppStores needs a StoresProvider above it");
  }
  return stores;
}

/** A value of the session store, re-rendering when it changes. */
export function useSession<T>(select: (state: SessionState) => T): T {
  return useStore(useAppStores().session, select);
}

/** A value of the connection store, re-rendering when it changes. */
export function useConnection<T>(select: (state: ConnectionState) => T): T {
  return useStore(useAppStores().connection, select);
}

/** A value of the notices store, re-rendering when it changes. */
export function useNotices<T>(select: (state: NoticesState) => T): T {
  return useStore(useAppStores().notices, select);
}

// The API client for every component below `App` (Plan.md P8.5): components and the
// query hooks (P8.6) take it with `useApiClient()`, never by importing a platform.
import { createContext, type ReactNode, useContext } from "react";

import type { ApiClient } from "./ApiClient";

const ApiClientContext = createContext<ApiClient | null>(null);

/** Makes `client` the API client of everything inside. */
export function ApiClientProvider({
  client,
  children,
}: {
  client: ApiClient;
  children: ReactNode;
}) {
  return <ApiClientContext value={client}>{children}</ApiClientContext>;
}

/** The API client `ApiClientProvider` provides; throws outside one. */
export function useApiClient(): ApiClient {
  const client = useContext(ApiClientContext);
  if (client === null) {
    throw new Error("useApiClient needs an ApiClientProvider above it");
  }
  return client;
}

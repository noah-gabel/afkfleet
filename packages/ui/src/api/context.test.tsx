import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { ApiClientProvider, useApiClient } from "./context";
import { FakeApiClient } from "./FakeApiClient";

function Probe({ expected }: { expected: unknown }) {
  const client = useApiClient();
  return <p>{client === expected ? "the provided client" : "another client"}</p>;
}

describe("useApiClient", () => {
  it("returns the provided client", () => {
    const client = new FakeApiClient();

    render(
      <ApiClientProvider client={client}>
        <Probe expected={client} />
      </ApiClientProvider>,
    );

    expect(screen.getByText("the provided client")).toBeDefined();
  });

  it("throws outside a provider", () => {
    expect(() => render(<Probe expected={null} />)).toThrow(/ApiClientProvider/);
  });
});

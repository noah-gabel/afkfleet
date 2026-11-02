import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { StatusLine, type Tone } from "./StatusLine";

describe("StatusLine", () => {
  it.each<[Tone, string]>([
    ["success", "text-success"],
    ["info", "text-info"],
    ["warning", "text-warning"],
    ["destructive", "text-destructive"],
    ["neutral", "text-muted-foreground"],
  ])("draws %s from its token", (tone, word) => {
    render(<StatusLine tone={tone}>Online</StatusLine>);

    expect(screen.getByText("Online").className).toContain(word);
  });

  it("keeps the word as written and leaves the capitals to the stylesheet", () => {
    render(<StatusLine tone="success">Online</StatusLine>);

    const line = screen.getByText("Online");
    expect(line.textContent).toBe("Online");
    expect(line.className).toContain("uppercase");
    expect(line.className).toContain("font-mono");
  });

  it("hides the square from a screen reader, so only the word is read", () => {
    render(<StatusLine tone="success">Online</StatusLine>);

    const square = screen.getByText("Online").querySelector("[aria-hidden]");
    expect(square).not.toBeNull();
    expect(square?.className).toContain("bg-success");
  });

  it("draws a neutral state's square as an outline", () => {
    render(<StatusLine tone="neutral">Stopped</StatusLine>);

    const square = screen.getByText("Stopped").querySelector("[aria-hidden]");
    expect(square?.className).toContain("border-muted-foreground");
    expect(square?.className).not.toContain("bg-");
  });

  it("announces nothing by itself", () => {
    render(<StatusLine tone="destructive">Failed</StatusLine>);

    expect(screen.queryByRole("status")).toBeNull();
    expect(screen.queryByRole("alert")).toBeNull();
  });
});

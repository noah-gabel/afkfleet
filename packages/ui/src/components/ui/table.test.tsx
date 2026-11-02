import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
  TableSpanRow,
} from "./table";

function sample(span = false) {
  render(
    <Table aria-label="Agents">
      <TableHeader>
        <TableRow>
          <TableHead>Name</TableHead>
          <TableHead drop="under-810">Version</TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        <TableRow>
          <TableCell>agent-main</TableCell>
          <TableCell drop="under-810">1.0</TableCell>
        </TableRow>
        {span ? <TableSpanRow span={2}>Disabling it is final.</TableSpanRow> : null}
      </TableBody>
    </Table>,
  );
}

describe("Table", () => {
  it("is named on the table itself, and its box is the container", () => {
    sample();

    const table = screen.getByRole("table", { name: "Agents" });
    expect(table.parentElement?.className).toContain("@container/table");
    expect(table.parentElement?.getAttribute("aria-label")).toBeNull();
  });

  it("gives a column's head its scope", () => {
    sample();

    expect(screen.getByRole("columnheader", { name: "Name" }).getAttribute("scope")).toBe("col");
  });

  it("drops a marked column's head and cell at the same width, and no other", () => {
    sample();

    const dropped = "@max-[50.625rem]/table:hidden";
    expect(screen.getByRole("columnheader", { name: "Version" }).className).toContain(dropped);
    expect(screen.getByRole("cell", { name: "1.0" }).className).toContain(dropped);
    expect(screen.getByRole("columnheader", { name: "Name" }).className).not.toContain(dropped);
    expect(screen.getByRole("cell", { name: "agent-main" }).className).not.toContain(dropped);
  });

  it("names a row from its cells", () => {
    sample();

    expect(screen.getByRole("row", { name: /agent-main/ })).toBeDefined();
  });

  it("spans a row over its columns, without its text widening them", () => {
    sample(true);

    const cell = screen.getByRole("cell", { name: "Disabling it is final." });
    expect(cell.getAttribute("colspan")).toBe("2");
    expect(cell.firstElementChild?.className).toContain("w-0");
    expect(cell.firstElementChild?.className).toContain("min-w-full");
  });
});

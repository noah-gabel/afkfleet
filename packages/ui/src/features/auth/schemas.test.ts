import { describe, expect, it } from "vitest";

import { classifyCode } from "./schemas";

describe("classifyCode", () => {
  it.each([
    ["", ""],
    ["1", "1"],
    ["12345", "12345"],
    ["123456", "123456"],
    ["123 456", "123456"],
    ["123-456", "123456"],
    [" 123\t456\n", "123456"],
    ["12 ", "12"],
    ["12-", "12"],
  ])("reads %j as the digits %j", (text, digits) => {
    expect(classifyCode(text)).toEqual({ kind: "digits", digits });
  });

  it.each([
    "abcd-efgh-ijkl-mnop-qrst",
    "abcdefghijklmnopqrst",
    "ABCD EFGH IJKL MNOP QRST",
    " 2345-6723-4567-2345-6723 ",
  ])("reads %j as a recovery code, as typed", (text) => {
    expect(classifyCode(text)).toEqual({ kind: "recovery", code: text.trim() });
  });

  // Filtering any of these to digits could send a wrong code, which costs one of three tries.
  it.each([
    "1234567",
    "12345a",
    "a",
    "12 34 56 78",
    "abcd-efgh-ijkl-mnop-qrs",
    "abcd-efgh-ijkl-mnop-qrstu",
    "abcd-efgh-ijkl-mnop-qrs1",
    "12.34.56",
    "١٢٣٤٥٦",
  ])("refuses %j", (text) => {
    expect(classifyCode(text)).toEqual({ kind: "other" });
  });
});

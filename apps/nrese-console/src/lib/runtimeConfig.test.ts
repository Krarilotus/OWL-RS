import { describe, expect, test } from "vitest";

import { normalizeApiBaseUrl, resolveApiBaseUrl } from "./runtimeConfig";

describe("normalizeApiBaseUrl", () => {
  test("keeps empty configuration empty", () => {
    expect(normalizeApiBaseUrl("")).toBe("");
    expect(normalizeApiBaseUrl(undefined)).toBe("");
  });

  test("removes trailing slashes", () => {
    expect(normalizeApiBaseUrl("http://127.0.0.1:8080/")).toBe(
      "http://127.0.0.1:8080",
    );
    expect(normalizeApiBaseUrl("https://api.example.com///")).toBe(
      "https://api.example.com",
    );
  });
});

describe("resolveApiBaseUrl", () => {
  test("the build-time URL holds when the runtime configuration sets none", () => {
    expect(resolveApiBaseUrl({}, "https://api.example.com/")).toBe("https://api.example.com");
  });

  test("a runtime URL overrides the build's, and an empty one asks for the own origin", () => {
    expect(resolveApiBaseUrl({ apiBaseUrl: "https://other.example.com" }, "https://api.example.com")).toBe(
      "https://other.example.com",
    );
    expect(resolveApiBaseUrl({ apiBaseUrl: "" }, "https://api.example.com")).toBe("");
  });

  test("without either, the console's own origin", () => {
    expect(resolveApiBaseUrl({}, undefined)).toBe("");
  });
});

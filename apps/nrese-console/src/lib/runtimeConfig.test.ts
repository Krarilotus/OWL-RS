import { describe, expect, test } from "vitest";

import { normalizeApiBaseUrl } from "./runtimeConfig";

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

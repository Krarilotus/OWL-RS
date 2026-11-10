import { expect, test } from "vitest";

// The storage the console reads (`window.localStorage`, app/useLocale.ts) works in its
// tests, as in a browser. Node 25 and later have their own `localStorage` global; on
// Node 26 (the Windows fleet host, 8 Nov) it hid jsdom's, `window.localStorage` read
// undefined, and every App.test.tsx row failed in useLocale before rendering.
test.each(["localStorage", "sessionStorage"] as const)("window.%s keeps what is stored", (name) => {
  const storage = window[name];
  expect(typeof storage?.getItem, `window.${name}`).toBe("function");
  storage.setItem("probe", "kept");
  expect(storage.getItem("probe")).toBe("kept");
  storage.removeItem("probe");
  expect(storage.getItem("probe")).toBeNull();
});

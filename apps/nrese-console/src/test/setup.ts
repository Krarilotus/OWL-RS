import "@testing-library/jest-dom";

// The test document's storage wins over Node's own: Node 25 and later define global
// `localStorage` and `sessionStorage` (undefined, or Node's in-memory store, without
// `--localstorage-file`), which hide jsdom's on the test window, so `window.localStorage`
// read undefined on Node 26. vitest's jsdom environment exposes its JSDOM instance as
// `jsdom`; its window's storage is what a browser page would see.
const dom = (globalThis as { jsdom?: { window: Window } }).jsdom;
if (dom) {
  for (const name of ["localStorage", "sessionStorage"] as const) {
    const storage = dom.window[name];
    if (globalThis[name] !== storage) {
      Object.defineProperty(globalThis, name, { value: storage, configurable: true, writable: true });
    }
  }
}

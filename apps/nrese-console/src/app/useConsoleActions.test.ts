import { act, renderHook } from "@testing-library/react";
import { describe, expect, test, vi } from "vitest";

import { en } from "../i18n/en";
import * as api from "../lib/api";
import type { ResponseEnvelope } from "../lib/http";
import type { OutputState, QuerySuggestion, QuerySuggestionResponse } from "../lib/types";
import { useConsoleActions } from "./useConsoleActions";

vi.mock("../lib/api", () => ({
  getQuerySuggestions: vi.fn(),
  runQuery: vi.fn(),
  runUpdate: vi.fn(),
  runTell: vi.fn(),
  readGraph: vi.fn(),
  writeGraph: vi.fn(),
  deleteGraph: vi.fn(),
}));

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}

const answer = (body: string): ResponseEnvelope => ({ ok: true, status: 200, body });

function setup() {
  const outputs: OutputState[] = [];
  const setOutput = (value: OutputState) => {
    outputs.push(value);
  };
  const { result } = renderHook(() =>
    useConsoleActions(en, { refetchRuntime: () => {}, refetchCapabilities: () => {} }),
  );
  return { actions: () => result.current, outputs, setOutput, last: () => outputs.at(-1) };
}

// SYN-72 C-1102: the output shows the newest action's answer; an earlier one finishing
// later, or failing, never replaces it, and a failure never leaves an old success showing.
describe("console output ownership", () => {
  test("a slow earlier answer doesn't overwrite a newer one", async () => {
    const { actions, setOutput, last } = setup();
    const slow = deferred<ResponseEnvelope>();
    const fast = deferred<ResponseEnvelope>();
    const a = actions().applyAction("query A", slow.promise, setOutput);
    const b = actions().applyAction("query B", fast.promise, setOutput);
    await act(async () => {
      fast.resolve(answer("B"));
      await b;
      slow.resolve(answer("A"));
      await a;
    });
    expect(last()).toMatchObject({ body: "B", status: "success" });
  });

  test("a failed request shows as an error, not the last success", async () => {
    const { actions, setOutput, last } = setup();
    await act(async () => {
      await actions().applyAction("query", Promise.resolve(answer("old")), setOutput);
    });
    await act(async () => {
      await actions()
        .applyAction("query", Promise.reject(new Error("network down")), setOutput)
        .catch(() => {});
    });
    expect(last()).toMatchObject({ status: "error", body: expect.stringContaining("network down") });
  });

  test("a suggestion finishing after a newer action leaves that action's output", async () => {
    const { actions, setOutput, last } = setup();
    const suggestion = deferred<QuerySuggestionResponse>();
    vi.mocked(api.getQuerySuggestions).mockReturnValueOnce(suggestion.promise);
    const suggest = actions().handleSuggest({
      prompt: "p",
      currentQuery: "",
      locale: "en",
      aiEnabled: true,
      setBusy: () => {},
      setSuggestions: () => {},
      setOutput,
    });
    await act(async () => {
      await actions().applyAction("query", Promise.resolve(answer("B")), setOutput);
      suggestion.resolve({ provider: "test", suggestions: [] } as unknown as QuerySuggestionResponse);
      await suggest;
    });
    expect(last()).toMatchObject({ body: "B" });
  });

  test("an earlier suggestion neither replaces a newer one's list nor clears its busy mark", async () => {
    const { actions, setOutput } = setup();
    const first = deferred<QuerySuggestionResponse>();
    const second = deferred<QuerySuggestionResponse>();
    vi.mocked(api.getQuerySuggestions)
      .mockReturnValueOnce(first.promise)
      .mockReturnValueOnce(second.promise);
    const busy: boolean[] = [];
    const lists: QuerySuggestion[][] = [];
    const state = {
      prompt: "p",
      currentQuery: "",
      locale: "en",
      aiEnabled: true,
      setBusy: (value: boolean) => {
        busy.push(value);
      },
      setSuggestions: (value: QuerySuggestion[]) => {
        lists.push(value);
      },
      setOutput,
    };
    const one = actions().handleSuggest(state);
    const two = actions().handleSuggest(state);
    const older = [{ query: "older" }] as unknown as QuerySuggestion[];
    const newer = [{ query: "newer" }] as unknown as QuerySuggestion[];
    await act(async () => {
      first.resolve({ provider: "test", suggestions: older } as unknown as QuerySuggestionResponse);
      await one;
    });
    // The second is still running: still busy, and the first's list isn't shown.
    expect(busy.at(-1)).toBe(true);
    expect(lists).toEqual([]);
    await act(async () => {
      second.resolve({ provider: "test", suggestions: newer } as unknown as QuerySuggestionResponse);
      await two;
    });
    expect(busy.at(-1)).toBe(false);
    expect(lists).toEqual([newer]);
  });
});

import { de } from "./de";
import { en } from "./en";
import type { AppStrings } from "./types";

export function getStrings(locale: string | undefined): AppStrings {
  if (!locale) {
    return en;
  }

  return locale.toLowerCase().startsWith("de") ? de : en;
}

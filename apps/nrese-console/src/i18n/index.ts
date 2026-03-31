import { de } from "./de";
import { en } from "./en";
import type { AppStrings } from "./types";

export const SUPPORTED_LOCALES = [
  { code: "en", label: "English" },
  { code: "de", label: "Deutsch" },
] as const;

export type SupportedLocale = (typeof SUPPORTED_LOCALES)[number]["code"];

export const DEFAULT_LOCALE: SupportedLocale = "en";

export function isSupportedLocale(locale: string): locale is SupportedLocale {
  return SUPPORTED_LOCALES.some((entry) => entry.code === locale);
}

export function getStrings(locale: string | undefined): AppStrings {
  if (!locale) {
    return en;
  }

  return locale.toLowerCase().startsWith("de") ? de : en;
}

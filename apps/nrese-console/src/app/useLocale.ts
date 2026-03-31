import { useEffect, useMemo, useState } from "react";

import { DEFAULT_LOCALE, getStrings, isSupportedLocale, type SupportedLocale } from "../i18n";

const STORAGE_KEY = "nrese-console.locale";

function initialLocale(): SupportedLocale {
  if (typeof window === "undefined") {
    return DEFAULT_LOCALE;
  }

  const stored = window.localStorage.getItem(STORAGE_KEY);
  if (stored && isSupportedLocale(stored)) {
    return stored;
  }

  return isSupportedLocale(navigator.language.slice(0, 2))
    ? (navigator.language.slice(0, 2) as SupportedLocale)
    : DEFAULT_LOCALE;
}

export function useLocale() {
  const [locale, setLocale] = useState<SupportedLocale>(initialLocale);

  useEffect(() => {
    window.localStorage.setItem(STORAGE_KEY, locale);
  }, [locale]);

  const strings = useMemo(() => getStrings(locale), [locale]);

  return {
    locale,
    setLocale,
    strings,
  };
}

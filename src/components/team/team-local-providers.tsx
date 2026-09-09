"use client"

import { useEffect, useState } from "react"
import { NextIntlClientProvider, type AbstractIntlMessages } from "next-intl"
import { ThemeProvider } from "@/components/theme-provider"
import { getMessagesForLocale } from "@/i18n/messages"
import {
  fromIntlLocale,
  getSystemLocaleCandidates,
  LANGUAGE_SETTINGS_STORAGE_KEY,
  normalizeLanguageSettings,
  resolveAppLocale,
  toIntlLocale,
  type IntlLocale,
} from "@/lib/i18n"
import type { AppLocale } from "@/lib/types"

function readLocalLocale(fallback: AppLocale): AppLocale {
  try {
    const raw = window.localStorage.getItem(LANGUAGE_SETTINGS_STORAGE_KEY)
    const settings = normalizeLanguageSettings(
      raw ? (JSON.parse(raw) as Record<string, unknown>) : null
    )
    if (!raw) return fallback
    return resolveAppLocale(settings, getSystemLocaleCandidates())
  } catch {
    return fallback
  }
}

export function TeamLocalProviders({
  children,
  initialLocale,
  initialMessages,
}: {
  children: React.ReactNode
  initialLocale: IntlLocale
  initialMessages: AbstractIntlMessages
}) {
  const initialAppLocale = fromIntlLocale(initialLocale)
  const [locale, setLocale] = useState<AppLocale>(initialAppLocale)
  const [messages, setMessages] =
    useState<AbstractIntlMessages>(initialMessages)

  useEffect(() => {
    let cancelled = false
    const nextLocale = readLocalLocale(initialAppLocale)
    void getMessagesForLocale(nextLocale).then((nextMessages) => {
      if (cancelled) return
      setLocale(nextLocale)
      setMessages(nextMessages)
      document.documentElement.lang = toIntlLocale(nextLocale)
      document.documentElement.dir = nextLocale === "ar" ? "rtl" : "ltr"
    })
    return () => {
      cancelled = true
    }
  }, [initialAppLocale])

  return (
    <NextIntlClientProvider locale={toIntlLocale(locale)} messages={messages}>
      <ThemeProvider
        attribute="class"
        defaultTheme="dark"
        enableSystem
        disableTransitionOnChange
      >
        {children}
      </ThemeProvider>
    </NextIntlClientProvider>
  )
}

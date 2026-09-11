import { readFileSync } from "node:fs"
import { join } from "node:path"
import { createTranslator } from "next-intl"
import { expect, it } from "vitest"
import english from "@/i18n/messages/en.json"

it.each([
  "ar",
  "de",
  "en",
  "es",
  "fr",
  "ja",
  "ko",
  "pt",
  "zh-CN",
  "zh-TW",
] as const)("renders every native label and interpolation in %s", (locale) => {
  const messages = JSON.parse(
    readFileSync(
      join(process.cwd(), "src/i18n/messages", `${locale}.json`),
      "utf8"
    )
  )
  expect(Object.keys(messages.Native).sort()).toEqual(
    Object.keys(english.Native).sort()
  )
  const errors: unknown[] = []
  const t = createTranslator({
    locale,
    messages,
    namespace: "Native",
    onError: (error) => errors.push(error),
  })
  for (const key of Object.keys(english.Native))
    expect(t(key, { name: "Sample", number: 1 })).toBeTruthy()
  expect(errors).toEqual([])
})

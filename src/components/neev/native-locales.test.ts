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
  const errors: unknown[] = []
  for (const namespace of ["Native", "NativeContent"] as const) {
    expect(Object.keys(messages[namespace]).sort()).toEqual(
      Object.keys(english[namespace]).sort()
    )
    const t = createTranslator({
      locale,
      messages,
      namespace,
      onError: (error) => errors.push(error),
    })
    const placeholders = (message: string) =>
      [
        ...new Set(
          [...message.matchAll(/\{([a-zA-Z]\w*)/g)].map((match) => match[1])
        ),
      ].sort()
    for (const [key, value] of Object.entries(english[namespace])) {
      expect(placeholders(messages[namespace][key])).toEqual(
        placeholders(value)
      )
      expect(
        t(key, {
          name: "Sample",
          number: 1,
          organization: "Test organization",
          audience: "Private",
          revision: "1",
        })
      ).toBeTruthy()
    }
  }
  expect(errors).toEqual([])
})

import type { NextConfig } from "next"
import createNextIntlPlugin from "next-intl/plugin"

const isProd = process.env.NODE_ENV === "production"
const internalHost = process.env.TAURI_DEV_HOST || "localhost"
const withNextIntl = createNextIntlPlugin({
  requestConfig: "./src/i18n/request.ts",
  experimental: {
    messages: {
      path: "./src/i18n/messages",
      format: "json",
      locales: [
        "en",
        "zh-CN",
        "zh-TW",
        "ja",
        "ko",
        "es",
        "de",
        "fr",
        "pt",
        "ar",
      ],
      precompile: true,
    },
  },
})

const nextConfig: NextConfig = {
  output: "export",
  images: {
    unoptimized: true,
  },
  assetPrefix: isProd
    ? undefined
    : (process.env.NEXT_PUBLIC_DEV_ASSET_ORIGIN ??
      `http://${internalHost}:3000`),
  experimental: {
    // Constrained local previews can opt out of the rebuildable disk cache.
    turbopackFileSystemCacheForDev:
      process.env.NEXT_DISABLE_TURBO_CACHE !== "1",
  },
}

export default withNextIntl(nextConfig)

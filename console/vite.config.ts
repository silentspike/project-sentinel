import { defineConfig } from "vite";
import solid from "vite-plugin-solid";

// SolidJS-Konsole (#419). Build-Output -> dist/ (vom #431-Backend per ServeDir ausgeliefert).
// Dev-Proxy leitet /api an das deployte #431-Backend (HTTPS, self-signed) weiter.
export default defineConfig(({ mode }) => ({
  plugins: [solid()],
  build: { target: "es2022", outDir: "dist", sourcemap: true },
  server: {
    proxy: {
      "/api": {
        target: process.env.SENTINEL_BACKEND ?? "https://127.0.0.1:8001",
        changeOrigin: true,
        secure: false,
      },
    },
  },
  // vitest (mode "test"): rendering SolidJS components needs the browser/development export
  // conditions so solid-js resolves to the client build (otherwise: "Client-only API called on
  // the server side"). Scoped to test mode → `vite build` (production) is unaffected.
  ...(mode === "test" ? { resolve: { conditions: ["development", "browser"] } } : {}),
  test: {
    environment: "jsdom",
    globals: true,
    include: ["tests/**/*.test.ts"],
  },
}));

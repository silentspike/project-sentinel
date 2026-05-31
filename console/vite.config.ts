import { defineConfig } from "vite";
import solid from "vite-plugin-solid";

// SolidJS-Konsole (#419). Build-Output -> dist/ (vom #431-Backend per ServeDir ausgeliefert).
// Dev-Proxy leitet /api an das deployte #431-Backend (HTTPS, self-signed) weiter.
export default defineConfig({
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
  test: {
    environment: "jsdom",
    globals: true,
  },
});

import { defineConfig, loadEnv } from "vite";
import react from "@vitejs/plugin-react";

export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, process.cwd(), "");
  const apiProxyTarget = env.VITE_API_PROXY_TARGET || "http://127.0.0.1:8080";
  const consoleBasePath = env.VITE_CONSOLE_BASE_PATH || "/console/";

  return {
    base: consoleBasePath,
    plugins: [react()],
    server: {
      open: consoleBasePath,
      proxy: {
        "/api": {
          target: apiProxyTarget,
          changeOrigin: true,
        },
        "/dataset": {
          target: apiProxyTarget,
          changeOrigin: true,
        },
        "/ops": {
          target: apiProxyTarget,
          changeOrigin: true,
        },
      },
    },
    test: {
      environment: "jsdom",
      globals: true,
      setupFiles: "./src/test/setup.ts",
    },
  };
});

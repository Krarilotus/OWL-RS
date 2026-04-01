import React from "react";
import ReactDOM from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";

import App from "./App";
import "./styles/base.css";
import "./styles/layout.css";

const queryClient = new QueryClient();

async function bootstrap(): Promise<void> {
  await loadRuntimeConfig();

  ReactDOM.createRoot(document.getElementById("root")!).render(
    <React.StrictMode>
      <QueryClientProvider client={queryClient}>
        <App />
      </QueryClientProvider>
    </React.StrictMode>,
  );
}

async function loadRuntimeConfig(): Promise<void> {
  const runtimeConfigModule = `${import.meta.env.BASE_URL}console-config.js`;
  try {
    await import(/* @vite-ignore */ runtimeConfigModule);
  } catch {
    window.__NRESE_CONSOLE_CONFIG__ = window.__NRESE_CONSOLE_CONFIG__ ?? {};
  }
}

void bootstrap();

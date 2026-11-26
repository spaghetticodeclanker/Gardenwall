import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// The dev server only listens on loopback; Tauri loads it in development.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    host: "127.0.0.1",
    port: 1420,
    strictPort: true,
  },
  build: {
    target: "es2022",
    sourcemap: false,
  },
});

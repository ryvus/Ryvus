import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

export default defineConfig({
  plugins: [react(), tailwindcss()],
  server: {
    proxy: {
      "/control": "http://127.0.0.1:8079",
      "/internal": "http://127.0.0.1:8079",
      "/system": "http://127.0.0.1:8080",
    },
  },
});

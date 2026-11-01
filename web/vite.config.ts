import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

export default defineConfig({
  plugins: [react(), tailwindcss()],
  server: {
    // In development, Vite plays the part Caddy plays in production: the
    // browser only talks to one address, so the API needs no CORS.
    proxy: {
      "/api": "http://localhost:3000",
    },
  },
});

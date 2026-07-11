import { defineConfig } from "@rsbuild/core";
import { pluginReact } from "@rsbuild/plugin-react";

export default defineConfig({
  plugins: [pluginReact()],
  html: {
    template: "./public/index.html",
  },
  server: {
    proxy: {
      "/chibitv.v1.ChibitvService": "http://[::1]:3001",
    },
  },
});

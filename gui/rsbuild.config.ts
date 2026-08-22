import { defineConfig } from "@rsbuild/core";
import { pluginReact } from "@rsbuild/plugin-react";

export default defineConfig({
  plugins: [pluginReact()],
  html: {
    template: "./public/index.html",
    // The overlaid UI insets itself with the safe area, so the video below it
    // may cover the whole display of a device with rounded corners: the home
    // indicator in portrait, and the notch or island in landscape, where an
    // installed app has no status bar to leave room for.
    meta: {
      viewport: "width=device-width, initial-scale=1, viewport-fit=cover",
    },
  },
  server: {
    proxy: {
      "/api": "http://[::1]:3001",
    },
  },
});

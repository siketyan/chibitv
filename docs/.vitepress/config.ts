import { defineConfig } from "vitepress";

export default defineConfig({
  title: "chibitv",
  description: "Yet another implementation for ARIB standards",
  lang: "en-US",

  // The site is published to https://siketyan.github.io/chibitv/, so every
  // asset and link has to be prefixed with the repository name.
  base: "/chibitv/",

  // A dead link is a broken page once deployed, so fail the build on one.
  ignoreDeadLinks: false,

  themeConfig: {
    nav: [
      { text: "Configuration", link: "/reference/configuration" },
      { text: "CLI", link: "/reference/cli" },
    ],

    sidebar: [
      {
        text: "Reference",
        items: [
          { text: "Configuration", link: "/reference/configuration" },
          { text: "CLI", link: "/reference/cli" },
        ],
      },
    ],

    socialLinks: [
      { icon: "github", link: "https://github.com/siketyan/chibitv" },
    ],

    editLink: {
      pattern: "https://github.com/siketyan/chibitv/edit/main/docs/:path",
      text: "Edit this page on GitHub",
    },

    search: {
      provider: "local",
    },
  },
});

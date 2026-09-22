import { defineConfig } from "vitepress";

export default defineConfig({
  title: "chibitv",
  description: "Yet another implementation for ARIB standards",
  lang: "en-US",

  head: [
    // Head links are not prefixed with `base`, so it is spelled out here.
    ["link", { rel: "icon", type: "image/svg+xml", href: "/chibitv/favicon.svg" }],
  ],

  // A dead link is a broken page once deployed, so fail the build on one.
  ignoreDeadLinks: false,

  themeConfig: {
    logo: "/favicon.svg",

    nav: [
      { text: "Getting started", link: "/guide/getting-started" },
      { text: "Configuration", link: "/reference/configuration" },
      { text: "CLI", link: "/reference/cli" },
      { text: "Docker", link: "/deployment/docker" },
    ],

    sidebar: [
      {
        text: "Guide",
        items: [{ text: "Getting started", link: "/guide/getting-started" }],
      },
      {
        text: "Reference",
        items: [
          { text: "Configuration", link: "/reference/configuration" },
          { text: "CLI", link: "/reference/cli" },
        ],
      },
      {
        text: "Deployment",
        items: [{ text: "Docker", link: "/deployment/docker" }],
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

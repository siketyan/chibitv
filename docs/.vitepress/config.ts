import { defineConfig } from "vitepress";

export default defineConfig({
  title: "chibitv",
  description: "Yet another implementation for ARIB standards",
  lang: "en-US",

  head: [
    ["link", { rel: "icon", type: "image/svg+xml", href: "/favicon.svg" }],
    // Crawlers need an absolute URL for the image.
    ["meta", { property: "og:image", content: "https://chibitv.p.s6n.jp/og.png" }],
    ["meta", { name: "twitter:card", content: "summary_large_image" }],
  ],

  // A dead link is a broken page once deployed, so fail the build on one.
  ignoreDeadLinks: false,

  themeConfig: {
    logo: { light: "/icon.svg", dark: "/icon-dark.svg", alt: "chibitv" },

    // Two sections, each with its own sidebar: the guide walks through
    // running chibitv, the reference documents every key and option of it.
    nav: [
      { text: "Guide", link: "/guide/getting-started", activeMatch: "/guide/" },
      { text: "Reference", link: "/reference/configuration", activeMatch: "/reference/" },
    ],

    sidebar: {
      "/guide/": [
        {
          text: "Introduction",
          items: [
            { text: "Installation", link: "/guide/installation" },
            { text: "Getting started", link: "/guide/getting-started" },
          ],
        },
        {
          text: "Deployment",
          items: [{ text: "Docker", link: "/guide/docker" }],
        },
      ],

      "/reference/": [
        {
          text: "Reference",
          items: [
            { text: "Configuration", link: "/reference/configuration" },
            { text: "CLI", link: "/reference/cli" },
          ],
        },
      ],
    },

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

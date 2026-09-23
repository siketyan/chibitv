---
layout: home

hero:
  name: chibitv
  tagline: The most lightweight way to watch Japanese TV streams in your home.
  image:
    light: /icon.svg
    dark: /icon-dark.svg
    alt: chibitv
  actions:
    - theme: brand
      text: Get Started
      link: /guide/getting-started
    - theme: alt
      text: View on GitHub
      link: https://github.com/siketyan/chibitv

features:
  - icon: 🪶
    title: Lightweight
    details: The server only tunes, descrambles and remuxes; the decoding happens in the browser, so nothing is transcoded on the way. One binary serves the RPC API, the live stream and the GUI, and the published image is distroless.
  - icon: 📦
    title: Out of the box
    details: A Linux DVB device, a px4_drv one or a BonDriver on Windows, and a CAS card over PC/SC. A scan fills the channels in and the GUI is a browser away, with no external muxer, player or tuner daemon to install beside it.
  - icon: 📘
    title: Written from the standards
    details: One crate per ARIB standard — the SI tables (B10), the character encoding (B24), the conditional access of 2K (B25) and of 4K (B61), and the MMT/TLV container (B60) — rather than a binding to an existing implementation.
  - icon: 🛰️
    title: 4K broadcasting
    details: ISDB-T and ISDB-S carry MPEG-2 TS and are descrambled with B25, while ISDB-S3, the 4K satellite broadcasting, carries MMT/TLV and is descrambled with B61. Both are tuned, descrambled and remuxed by the same pipeline.
---

> [!WARNING]
> This software is intended for experimental purposes to understand the
> standards and is not recommended for any other use.

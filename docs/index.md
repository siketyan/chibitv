---
layout: home

hero:
  name: chibitv
  text: Yet another implementation for ARIB standards
  tagline: Tune, descramble and remux Japanese ISDB-S/ISDB-T broadcasts.
  image:
    src: /favicon.svg
    alt: chibitv
  actions:
    - theme: brand
      text: Getting started
      link: /guide/getting-started
    - theme: alt
      text: Configuration reference
      link: /reference/configuration
    - theme: alt
      text: CLI reference
      link: /reference/cli

features:
  - title: Getting started
    details: Prerequisites, device permissions on Linux, configuring config.toml, and running the server with the GUI.
    link: /guide/getting-started
  - title: Configuration
    details: Every key of config.toml, from the CAS master key to tuners, channels, the server address, the database and where recordings are kept.
    link: /reference/configuration
  - title: CLI
    details: Every subcommand and option of the chibitv binary — live, record, remux, scan, status and serve.
    link: /reference/cli
---

> [!WARNING]
> This software is intended for experimental purposes to understand the
> standards and is not recommended for any other use.

chibitv tunes Japanese ISDB-S/ISDB-T broadcasts, descrambles them, and remuxes
them to MPEG-2 TS / MP4 / fragmented MP4, with an HTTP streaming server and a
React GUI on top.

Start with [getting started](./guide/getting-started) to set up the tuner, the
CAS module and `config.toml`. The reference pages then cover every key of the
[configuration](./reference/configuration) and every subcommand of the
[CLI](./reference/cli).

---
layout: home

hero:
  name: chibitv
  text: Yet another implementation for ARIB standards
  tagline: Tune, descramble and remux Japanese ISDB-S/ISDB-T broadcasts.
  actions:
    - theme: brand
      text: Configuration reference
      link: /reference/configuration
    - theme: alt
      text: CLI reference
      link: /reference/cli
    - theme: alt
      text: View on GitHub
      link: https://github.com/siketyan/chibitv

features:
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

These pages are the reference for the two surfaces the application is driven
through: the `config.toml` file every subcommand reads, and the subcommands
themselves. The [README](https://github.com/siketyan/chibitv#readme) covers the
prerequisites, the device permissions needed on Linux, and running the Docker
image.

---
layout: home

hero:
  name: chibitv
  tagline: The most lightweight way to watch Japanese TV streams in your home.
  image:
    src: /favicon.svg
    alt: chibitv
  actions:
    - theme: brand
      text: Get Started
      link: /guide/getting-started
    - theme: alt
      text: View on GitHub
      link: https://github.com/siketyan/chibitv

features:
  - title: Getting started
    details: Prerequisites, device permissions on Linux, configuring config.toml, and running the server with the GUI.
    link: /guide/getting-started
  - title: Configuration
    details: Every key of config.toml, from the CAS master key to tuners, the server address, the database the channels and the guide are kept in, and where recordings are kept.
    link: /reference/configuration
  - title: CLI
    details: Every subcommand and option of the chibitv binary — live, record, remux, scan, status and serve.
    link: /reference/cli
  - title: Docker
    details: Running the image that serves the RPC API and the GUI from one binary, with the tuner devices and the PC/SC daemon of the host.
    link: /deployment/docker
---

> [!WARNING]
> This software is intended for experimental purposes to understand the
> standards and is not recommended for any other use.

# chibitv - Yet another implementation for ARIB standards

> [!WARNING]
> This software is intended for experimental purposes to understand the standards and is not recommended for any other use.

## Prerequisites

- A DVB compatible tuner to produce raw MMT/TLV stream
- A PC/SC compatible interface to the CAS module
- The value of _Kd_ defined in Section 1.4 of the ARIB STD-B61 standard

## Usage

```shell
cargo run
pnpm -C gui dev
```

Open http://localhost:3000/ in your browser and enjoy!

## References

- ARIB STD-B32: https://www.arib.or.jp/english/html/overview/doc/6-STD-B32v3_11-3p3-E1.pdf
- ARIB STD-B60: https://www.arib.or.jp/english/html/overview/doc/6-STD-B60_v1_14-E1.pdf
- ARIB STD-B61: https://www.arib.or.jp/english/html/overview/doc/6-STD-B61v1_4-E1.pdf

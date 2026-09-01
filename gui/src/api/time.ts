import type { DateTime } from "../gen/chibitv/v1/chibitv_pb";

/** Reads a time the server reports as a date of the browser. */
export function toDate(value: DateTime | undefined): Date | undefined {
  if (!value) {
    return undefined;
  }

  return new Date(Number(value.seconds) * 1000 + value.nanos / 1_000_000);
}

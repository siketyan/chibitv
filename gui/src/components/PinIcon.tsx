import type { JSX } from "react";

/** A thumbtack, rather than a geographic location marker. */
export function PinIcon(): JSX.Element {
  return (
    <svg aria-hidden="true" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5">
      <path strokeLinecap="round" strokeLinejoin="round" d="M8 3h8M9 3v7l-3 4v2h12v-2l-3-4V3M12 16v5" />
    </svg>
  );
}

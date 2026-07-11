export type TranscoderRequest =
  | { type: "start"; bitrate: number }
  | { type: "data"; data: ArrayBuffer }
  | { type: "ack"; chunkId: number }
  | { type: "cancel" };

export type TranscoderResponse =
  | { type: "ready"; mimeType: string }
  | { type: "chunk"; chunkId: number; data: ArrayBuffer }
  | { type: "complete" }
  | { type: "error"; error: string };

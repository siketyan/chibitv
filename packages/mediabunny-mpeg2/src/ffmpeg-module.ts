export type FfmpegModule = {
  HEAPU8: Uint8Array;
  UTF8ToString(pointer: number): string;
  _malloc(size: number): number;
  _free(pointer: number): void;
  _mpeg2_decoder_init(): number;
  _mpeg2_decoder_send(data: number, size: number, pts: number, duration: number): number;
  _mpeg2_decoder_flush(): number;
  _mpeg2_decoder_receive(): number;
  _mpeg2_decoder_close(): void;
  _mpeg2_decoder_error(): number;
  _mpeg2_frame_width(): number;
  _mpeg2_frame_height(): number;
  _mpeg2_frame_plane_pointer(plane: number): number;
  _mpeg2_frame_plane_stride(plane: number): number;
  _mpeg2_frame_pts(): number;
  _mpeg2_frame_duration(): number;
  _mpeg2_frame_sar_num(): number;
  _mpeg2_frame_sar_den(): number;
};

export type FfmpegModuleFactory = () => Promise<FfmpegModule>;

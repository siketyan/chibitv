#![allow(unused)]

use bytes::{Buf, BufMut, Bytes, BytesMut};

const HEVC_NAL_RASL_R: u8 = 9;
const HEVC_NAL_BLA_W_LP: u8 = 16;
const HEVC_NAL_CRA_NUT: u8 = 21;
const HEVC_NAL_VPS: u8 = 32;
const HEVC_NAL_AUD: u8 = 35;
const HEVC_NAL_EOB_NUT: u8 = 37;
const HEVC_NAL_SEI_PREFIX: u8 = 39;
const HEVC_NAL_RSV_NVCL41: u8 = 41;
const HEVC_NAL_RSV_NVCL44: u8 = 44;
const HEVC_NAL_UNSPEC48: u8 = 48;
const HEVC_NAL_UNSPEC55: u8 = 55;

#[derive(Clone, Debug, Default)]
pub struct HevcParser {
    buf: BytesMut,
    frame_start_found: bool,
}

impl HevcParser {
    pub fn push(&mut self, buf: &[u8]) -> Option<Bytes> {
        match self.find_next_frame(buf) {
            Some(idx) => {
                let remaining = self.buf.remaining();
                if remaining == 0 && idx == 0 {
                    // Do not emit an empty buffer.
                    return None;
                }
                self.buf.put_slice(buf);
                Some(self.buf.split_to(remaining + idx).freeze())
            }
            None => {
                self.buf.put_slice(buf);
                None
            }
        }
    }

    /// Find the index of the first octet of the next frame within the buffer.
    fn find_next_frame(&mut self, buf: &[u8]) -> Option<usize> {
        let mut state = [0xFF_u8; 8];

        for (i, b) in buf.iter().copied().enumerate() {
            state = [
                state[1], state[2], state[3], state[4], state[5], state[6], state[7], b,
            ];

            // Find the start code of a NALu.
            if state[2..5] != [0x00, 0x00, 0x01] {
                continue;
            }

            let ty = (state[5] & 0x7E) >> 1;
            let layer_id = (u64::from_be_bytes(state) >> 11) & 0x3F;
            if layer_id > 0 {
                continue;
            }

            if ty == HEVC_NAL_AUD {
                return Some(if state[1] == 0 { i - 6 } else { i - 5 });
            }

            // TODO: Detect the next frame without AUD NAL.
            //       https://github.com/FFmpeg/FFmpeg/blob/3f30ae823e27e7a60c693b52ad44b10ac2ad2823/libavcodec/hevc/parser.c#L257
            // if (HEVC_NAL_VPS..=HEVC_NAL_EOB_NUT).contains(&ty)
            //     || ty == HEVC_NAL_SEI_PREFIX
            //     || (HEVC_NAL_RSV_NVCL41..=HEVC_NAL_RSV_NVCL44).contains(&ty)
            //     || (HEVC_NAL_UNSPEC48..=HEVC_NAL_UNSPEC55).contains(&ty)
            // {
            //     if self.frame_start_found {
            //         return Some(if state[1] == 0 { i - 6 } else { i - 5 });
            //     }
            // } else if (..=HEVC_NAL_RASL_R).contains(&ty)
            //     || (HEVC_NAL_BLA_W_LP..=HEVC_NAL_CRA_NUT).contains(&ty)
            // {
            //     let first_slice_segment_in_pic_flag = b >> 7;
            //     if first_slice_segment_in_pic_flag > 0 {
            //         if self.frame_start_found {
            //             self.frame_start_found = false;
            //
            //             return Some(if state[1] == 0 { i - 6 } else { i - 5 });
            //         } else {
            //             self.frame_start_found = true;
            //         }
            //     }
            // }
        }

        None
    }
}

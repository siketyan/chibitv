use tracing::warn;

use crate::mmtp::FragmentationIndicator;

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum State {
    #[default]
    Init,
    NotStarted,
    InFragment,
    Skip,
}

#[derive(Clone, Debug, Default)]
pub struct Defragmenter {
    state: State,
    last_sequence_number: u32,
    buf: Vec<u8>,
}

impl Defragmenter {
    pub fn state(&self) -> State {
        self.state
    }

    pub fn sync(&mut self, sequence_number: u32) {
        match self.state {
            State::Init => {
                self.state = State::Skip;
                self.last_sequence_number = sequence_number;
            }
            _ if sequence_number == self.last_sequence_number + 1 => {
                self.last_sequence_number = sequence_number;
            }
            _ if sequence_number != self.last_sequence_number => {
                warn!(
                    "Packet sequence number jump: {} != {} + 1",
                    sequence_number, self.last_sequence_number,
                );

                if !self.buf.is_empty() {
                    warn!("Drop {} octets in the buffer.", self.buf.len());

                    self.buf.clear();
                }

                self.state = State::Skip;
                self.last_sequence_number = sequence_number;
            }
            _ => {}
        }
    }

    /// Push a fragment and try assembling fragments into a buffer.
    /// Returns a completed buffer if the current fragment completed the buffer.
    pub fn push(
        &mut self,
        fragmentation_indicator: FragmentationIndicator,
        buf: &[u8],
    ) -> Option<Vec<u8>> {
        match fragmentation_indicator {
            FragmentationIndicator::NotFragmented => {
                // Non-fragment packet can't be accepted while in the middle of a fragment.
                assert_ne!(self.state, State::InFragment);

                self.state = State::NotStarted;

                // Returns the provided buf as-is.
                Some(buf.to_vec())
            }
            FragmentationIndicator::FragmentHead => {
                // Head packet can't be accepted while in the middle of a fragment.
                assert_ne!(self.state, State::InFragment);

                // Copies the buf.
                self.state = State::InFragment;
                self.buf.extend_from_slice(buf);

                // Not yet completed.
                None
            }
            FragmentationIndicator::FragmentBody => {
                if self.state == State::Skip {
                    // We can do nothing on a skipped fragment.
                    warn!("Packet dropped!");
                } else {
                    // It must be in the middle of a fragment.
                    assert_eq!(self.state, State::InFragment);

                    // Copies the buf.
                    self.buf.extend_from_slice(buf);
                }

                // Not yet completed.
                None
            }
            FragmentationIndicator::FragmentTail => {
                if self.state == State::Skip {
                    warn!("Packet dropped!");

                    // Not yet completed.
                    None
                } else {
                    // It must be in the middle of a fragment.
                    assert_eq!(self.state, State::InFragment);

                    // Copies the buf.
                    self.state = State::NotStarted;
                    self.buf.extend_from_slice(buf);

                    // Replace the buf with a new Vec and return the current buf.
                    Some(std::mem::take(&mut self.buf))
                }
            }
        }
    }
}

use std::net::{Ipv4Addr, Ipv6Addr};

use bytes::Bytes;

pub(crate) trait BytesExt {
    fn get_byte_array<const N: usize>(&mut self) -> [u8; N];

    fn get_ipv4_addr(&mut self) -> Ipv4Addr {
        Ipv4Addr::from(self.get_byte_array::<4>())
    }

    fn get_ipv6_addr(&mut self) -> Ipv6Addr {
        Ipv6Addr::from(self.get_byte_array::<16>())
    }
}

impl BytesExt for Bytes {
    fn get_byte_array<const N: usize>(&mut self) -> [u8; N] {
        let buf = self.split_to(N);
        buf.as_ref().try_into().unwrap()
    }
}

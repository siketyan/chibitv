#[derive(Clone, Copy, Debug)]
struct CoreData {
    l: u32,
    r: u32,
}

#[derive(Debug)]
pub(crate) struct Multi2 {
    system_key: Option<[u32; 8]>,
    init_cbc: Option<CoreData>,
    work_keys: Option<[[u32; 8]; 2]>,
    round: usize,
}

impl Multi2 {
    pub(crate) fn new(system_key: [u8; 32], init_cbc: [u8; 8]) -> Self {
        let mut key = [0; 8];
        for (index, chunk) in system_key.chunks_exact(4).enumerate() {
            key[index] = u32::from_be_bytes(chunk.try_into().unwrap());
        }

        Self {
            system_key: Some(key),
            init_cbc: Some(CoreData {
                l: u32::from_be_bytes(init_cbc[0..4].try_into().unwrap()),
                r: u32::from_be_bytes(init_cbc[4..8].try_into().unwrap()),
            }),
            work_keys: None,
            round: 4,
        }
    }

    pub(crate) fn set_scramble_key(&mut self, scramble_key: [u8; 16]) {
        let odd = CoreData {
            l: u32::from_be_bytes(scramble_key[0..4].try_into().unwrap()),
            r: u32::from_be_bytes(scramble_key[4..8].try_into().unwrap()),
        };
        let even = CoreData {
            l: u32::from_be_bytes(scramble_key[8..12].try_into().unwrap()),
            r: u32::from_be_bytes(scramble_key[12..16].try_into().unwrap()),
        };

        let system_key = self.system_key.expect("system key is not set");
        self.work_keys = Some([
            schedule_work_key(system_key, odd),
            schedule_work_key(system_key, even),
        ]);
    }

    pub(crate) fn decrypt(&self, scrambling_control: u8, data: &mut [u8]) -> anyhow::Result<bool> {
        let Some(work_keys) = self.work_keys else {
            return Ok(false);
        };

        let work_key = match scrambling_control {
            0b10 => work_keys[1],
            0b11 => work_keys[0],
            _ => return Ok(true),
        };

        let mut cbc = self.init_cbc.expect("CBC initial value is not set");
        let mut chunks = data.chunks_exact_mut(8);
        for chunk in &mut chunks {
            let src = CoreData {
                l: u32::from_be_bytes(chunk[0..4].try_into().unwrap()),
                r: u32::from_be_bytes(chunk[4..8].try_into().unwrap()),
            };
            let mut dst = core_decrypt(src, work_key, self.round);
            dst.l ^= cbc.l;
            dst.r ^= cbc.r;
            cbc = src;
            chunk[0..4].copy_from_slice(&dst.l.to_be_bytes());
            chunk[4..8].copy_from_slice(&dst.r.to_be_bytes());
        }

        let rest = chunks.into_remainder();
        if !rest.is_empty() {
            let stream = core_encrypt(cbc, work_key, self.round);
            let mut key_stream = [0; 8];
            key_stream[0..4].copy_from_slice(&stream.l.to_be_bytes());
            key_stream[4..8].copy_from_slice(&stream.r.to_be_bytes());
            for (byte, key) in rest.iter_mut().zip(key_stream) {
                *byte ^= key;
            }
        }

        Ok(true)
    }
}

fn schedule_work_key(system_key: [u32; 8], data_key: CoreData) -> [u32; 8] {
    let b1 = pi1(data_key);
    let b2 = pi2(b1, system_key[0]);
    let b3 = pi3(b2, system_key[1], system_key[2]);
    let b4 = pi4(b3, system_key[3]);
    let b5 = pi1(b4);
    let b6 = pi2(b5, system_key[4]);
    let b7 = pi3(b6, system_key[5], system_key[6]);
    let b8 = pi4(b7, system_key[7]);
    let b9 = pi1(b8);

    [b2.l, b3.r, b4.l, b5.r, b6.l, b7.r, b8.l, b9.r]
}

fn core_encrypt(src: CoreData, work_key: [u32; 8], round: usize) -> CoreData {
    let mut dst = src;
    for _ in 0..round {
        dst = pi1(dst);
        dst = pi2(dst, work_key[0]);
        dst = pi3(dst, work_key[1], work_key[2]);
        dst = pi4(dst, work_key[3]);
        dst = pi1(dst);
        dst = pi2(dst, work_key[4]);
        dst = pi3(dst, work_key[5], work_key[6]);
        dst = pi4(dst, work_key[7]);
    }
    dst
}

fn core_decrypt(src: CoreData, work_key: [u32; 8], round: usize) -> CoreData {
    let mut dst = src;
    for _ in 0..round {
        dst = pi4(dst, work_key[7]);
        dst = pi3(dst, work_key[5], work_key[6]);
        dst = pi2(dst, work_key[4]);
        dst = pi1(dst);
        dst = pi4(dst, work_key[3]);
        dst = pi3(dst, work_key[1], work_key[2]);
        dst = pi2(dst, work_key[0]);
        dst = pi1(dst);
    }
    dst
}

fn pi1(src: CoreData) -> CoreData {
    CoreData {
        l: src.l,
        r: src.r ^ src.l,
    }
}

fn pi2(src: CoreData, a: u32) -> CoreData {
    let t0 = src.r.wrapping_add(a);
    let t1 = t0.rotate_left(1).wrapping_add(t0).wrapping_sub(1);
    let t2 = t1.rotate_left(4) ^ t1;

    CoreData {
        l: src.l ^ t2,
        r: src.r,
    }
}

fn pi3(src: CoreData, a: u32, b: u32) -> CoreData {
    let t0 = src.l.wrapping_add(a);
    let t1 = t0.rotate_left(2).wrapping_add(t0).wrapping_add(1);
    let t2 = t1.rotate_left(8) ^ t1;
    let t3 = t2.wrapping_add(b);
    let t4 = t3.rotate_left(1).wrapping_sub(t3);
    let t5 = t4.rotate_left(16) ^ (t4 | src.l);

    CoreData {
        l: src.l,
        r: src.r ^ t5,
    }
}

fn pi4(src: CoreData, a: u32) -> CoreData {
    let t0 = src.r.wrapping_add(a);
    let t1 = t0.rotate_left(2).wrapping_add(t0).wrapping_add(1);

    CoreData {
        l: src.l ^ t1,
        r: src.r,
    }
}

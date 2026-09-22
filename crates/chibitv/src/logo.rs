//! Resolves SDT logo references against CDT images. The ARIB palette omitted
//! from broadcast PNGs is supplied here so browsers can display them.
use std::collections::HashMap;

use crate::registry::{Registry, ServiceKey};

#[derive(Clone, Debug)]
struct Reference {
    network: u16,
    id: u16,
    download: Option<(u16, u16)>, // download_data_id, logo_version
    ranges: Vec<(u8, u8, u8)>,    // MH logo_type, first section, count
}

struct Image {
    download: u16,
    version: u16,
    kind: u8,
    png: Vec<u8>,
}

#[derive(Default)]
pub struct Logos {
    references: HashMap<ServiceKey, Reference>,
    images: HashMap<(u16, u16), Image>,
    modules: HashMap<(u32, u16), Module>,
    carousel_kinds: HashMap<ServiceKey, u8>,
    mh_sections: HashMap<(u16, u16), Fragments>,
}

impl Logos {
    /// B10 0xcf and B60 0x8025 share the reference prefix. The optional MH
    /// section ranges follow it and do not change the logo's identity.
    pub fn reference(&mut self, registry: &Registry, key: ServiceKey, network: u16, bytes: &[u8]) {
        let Some(&kind) = bytes.first() else {
            return;
        };
        if !matches!(kind, 1 | 2) || bytes.len() < if kind == 1 { 7 } else { 3 } {
            return;
        }
        if registry.get_service(key).is_none() {
            return;
        }
        if kind == 1 && !(bytes.len() - 7).is_multiple_of(3) {
            return;
        }
        let reference = Reference {
            ranges: if kind == 1 {
                bytes[7..]
                    .as_chunks::<3>()
                    .0
                    .iter()
                    .map(|range| (range[0], range[1], range[2]))
                    .collect()
            } else {
                Vec::new()
            },
            network,
            id: u16::from_be_bytes([bytes[1], bytes[2]]) & 0x1ff,
            download: (kind == 1).then(|| {
                (
                    u16::from_be_bytes([bytes[5], bytes[6]]),
                    u16::from_be_bytes([bytes[3], bytes[4]]) & 0xfff,
                )
            }),
        };
        self.references.insert(key, reference);
        self.publish_mh(registry);
        self.publish(registry);
    }

    pub fn data(&mut self, registry: &Registry, network: u16, download: u16, data: &[u8]) {
        if data.len() < 7 || data[0] > 7 {
            return;
        }
        let kind = data[0];
        let id = u16::from_be_bytes([data[1], data[2]]) & 0x1ff;
        let version = u16::from_be_bytes([data[3], data[4]]) & 0xfff;
        let size = usize::from(u16::from_be_bytes([data[5], data[6]]));
        let Some(data) = data.get(7..7 + size) else {
            return;
        };
        let key = (network, id);
        if self.images.get(&key).is_some_and(|image| {
            image.download == download && image.version == version && image.kind > kind
        }) {
            return;
        }
        let Some(png) = browser_png(data) else {
            return;
        };
        // A tuned multiplex only needs a small logo catalogue. Bound SI input
        // even when a damaged broadcaster continually invents new identities.
        if self.images.len() >= 512 && !self.images.contains_key(&key) {
            return;
        }
        self.images.insert(
            key,
            Image {
                download,
                version,
                kind,
                png,
            },
        );
        self.publish(registry);
    }

    fn publish(&self, registry: &Registry) {
        for (key, reference) in &self.references {
            if let Some(image) = self.images.get(&(reference.network, reference.id))
                && reference
                    .download
                    .is_none_or(|download| download == (image.download, image.version))
            {
                registry.put_logo(*key, &image.png);
            }
        }
    }
}

struct Fragments {
    version: u8,
    sections: Vec<Option<Vec<u8>>>,
}

impl Logos {
    pub fn mh_data(&mut self, registry: &Registry, table: chibitv_b60::table::MhCdt) {
        let key = (table.original_network_id, table.download_data_id);
        if self.mh_sections.len() >= 32 && !self.mh_sections.contains_key(&key) {
            return;
        }
        let count = usize::from(table.last_section_number) + 1;
        let fragments = self.mh_sections.entry(key).or_insert_with(|| Fragments {
            version: table.version_number,
            sections: vec![None; count],
        });
        if fragments.version != table.version_number || fragments.sections.len() != count {
            *fragments = Fragments {
                version: table.version_number,
                sections: vec![None; count],
            };
        }
        if let Some(section) = fragments
            .sections
            .get_mut(usize::from(table.section_number))
        {
            *section = Some(table.data);
        }
        self.publish_mh(registry);
    }

    fn publish_mh(&mut self, registry: &Registry) {
        let mut modules = Vec::new();
        for reference in self.references.values() {
            let Some((download, version)) = reference.download else {
                continue;
            };
            let Some(fragments) = self.mh_sections.get(&(reference.network, download)) else {
                continue;
            };
            for &(kind, start, count) in &reference.ranges {
                if count == 0 {
                    continue;
                }
                let Some(sections) = fragments
                    .sections
                    .get(usize::from(start)..usize::from(start) + usize::from(count))
                else {
                    continue;
                };
                let Some(sections) = sections
                    .iter()
                    .map(Option::as_ref)
                    .collect::<Option<Vec<_>>>()
                else {
                    continue;
                };
                let data: Vec<u8> = sections.into_iter().flatten().copied().collect();
                if data.len() >= 7
                    && data[0] == kind
                    && u16::from_be_bytes([data[1], data[2]]) & 0x1ff == reference.id
                    && u16::from_be_bytes([data[3], data[4]]) & 0xfff == version
                {
                    modules.push((reference.network, download, data));
                }
            }
        }
        for (network, download, data) in modules {
            self.data(registry, network, download, &data);
        }
    }
}

// DSM-CC data carousel modules are announced by DII and delivered in DDB
// blocks. Only the named common-receiver logo modules are retained.
struct Module {
    version: u8,
    block_size: usize,
    data: Vec<u8>,
    received: Vec<bool>,
}

impl Logos {
    pub fn carousel(&mut self, registry: &Registry, section: &[u8]) {
        let _ = self.read_carousel(registry, section);
    }

    fn read_carousel(&mut self, registry: &Registry, section: &[u8]) -> Option<()> {
        if section.len() < 11 || section[4] & 1 == 0 {
            return None;
        }
        let mut section = section;
        let section_length = usize::from(take_u16(&mut section)? & 0xfff);
        let section = section.get(..section_length)?;
        let mut message = section.get(5..section.len().checked_sub(4)?)?;
        if take(&mut message, 2)? != [0x11, 0x03] {
            return None;
        }
        let kind = take_u16(&mut message)?;
        let transaction = take_u32(&mut message)?;
        take(&mut message, 1)?;
        let adaptation = take_u8(&mut message)? as usize;
        let length = take_u16(&mut message)? as usize;
        let mut body = take(&mut message, length)?;
        take(&mut body, adaptation)?;
        match kind {
            0x1002 => {
                let download = take_u32(&mut body)?;
                let block_size = take_u16(&mut body)? as usize;
                if !(1..=4066).contains(&block_size) {
                    return None;
                }
                take(&mut body, 10)?;
                let compatibility = take_u16(&mut body)? as usize;
                take(&mut body, compatibility)?;
                let count = take_u16(&mut body)?;
                for _ in 0..count {
                    let id = take_u16(&mut body)?;
                    let size = take_u32(&mut body)? as usize;
                    let version = take_u8(&mut body)?;
                    let info_length = take_u8(&mut body)? as usize;
                    let mut info = take(&mut body, info_length)?;
                    let mut is_logo = false;
                    while !info.is_empty() {
                        let tag = take_u8(&mut info)?;
                        let length = take_u8(&mut info)? as usize;
                        let data = take(&mut info, length)?;
                        if tag == 2 {
                            is_logo = data.starts_with(b"LOGO-0") || data.starts_with(b"CS_LOGO-0");
                        }
                    }
                    if !is_logo
                        || size == 0
                        || size > 1024 * 1024
                        || size.div_ceil(block_size) > 65536
                    {
                        continue;
                    }
                    let key = (download, id);
                    if let Some(module) = self.modules.get(&key)
                        && module.version == version
                        && module.data.len() == size
                        && module.block_size == block_size
                    {
                        if module.received.iter().all(|received| *received) {
                            let data = module.data.clone();
                            self.carousel_module(registry, &data);
                        }
                        continue;
                    }
                    if self.modules.len() >= 32 && !self.modules.contains_key(&key) {
                        continue;
                    }
                    self.modules.insert(
                        key,
                        Module {
                            version,
                            block_size,
                            data: vec![0; size],
                            received: vec![false; size.div_ceil(block_size)],
                        },
                    );
                }
            }
            0x1003 => {
                let id = take_u16(&mut body)?;
                let version = take_u8(&mut body)?;
                take(&mut body, 1)?;
                let block = take_u16(&mut body)? as usize;
                let module = self.modules.get_mut(&(transaction, id))?;
                if module.version != version || block >= module.received.len() {
                    return None;
                }
                let start = block * module.block_size;
                let end = (start + module.block_size).min(module.data.len());
                if body.len() != end - start {
                    return None;
                }
                module.data[start..end].copy_from_slice(body);
                module.received[block] = true;
                if module.received.iter().all(|received| *received) {
                    let data = module.data.clone();
                    self.carousel_module(registry, &data);
                }
            }
            _ => {}
        }
        Some(())
    }

    fn carousel_module(&mut self, registry: &Registry, mut data: &[u8]) -> Option<()> {
        let kind = take_u8(&mut data)?;
        if kind > 5 {
            return None;
        }
        let count = take_u16(&mut data)?;
        for _ in 0..count {
            let _logo_id = take_u16(&mut data)? & 0x1ff;
            let count = take_u8(&mut data)? as usize;
            let services = take(&mut data, count * 6)?;
            let size = take_u16(&mut data)? as usize;
            let png = take(&mut data, size)?;
            let Some(png) = browser_png(png) else {
                continue;
            };
            for service in services.as_chunks::<6>().0 {
                let key = ServiceKey {
                    stream_id: u16::from_be_bytes([service[2], service[3]]),
                    service_id: u16::from_be_bytes([service[4], service[5]]),
                };
                if registry.get_service(key).is_some()
                    && self
                        .carousel_kinds
                        .get(&key)
                        .is_none_or(|previous| kind >= *previous)
                {
                    registry.put_logo(key, &png);
                    self.carousel_kinds.insert(key, kind);
                }
            }
        }
        Some(())
    }
}

fn take<'a>(bytes: &mut &'a [u8], length: usize) -> Option<&'a [u8]> {
    let (head, rest) = bytes.split_at_checked(length)?;
    *bytes = rest;
    Some(head)
}
fn take_u8(bytes: &mut &[u8]) -> Option<u8> {
    Some(take(bytes, 1)?[0])
}
fn take_u16(bytes: &mut &[u8]) -> Option<u16> {
    Some(u16::from_be_bytes(take(bytes, 2)?.try_into().ok()?))
}
fn take_u32(bytes: &mut &[u8]) -> Option<u32> {
    Some(u32::from_be_bytes(take(bytes, 4)?.try_into().ok()?))
}

const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
const PNG_CRC: crc::Crc<u32> = crc::Crc::<u32>::new(&crc::CRC_32_ISO_HDLC);

/// Keeps full PNGs unchanged, and fills in the fixed ARIB STD-B24 CLUT for
/// indexed PNGs that omit PLTE/tRNS. Checks framing and CRC before publishing.
fn browser_png(data: &[u8]) -> Option<Vec<u8>> {
    if !data.starts_with(PNG_SIGNATURE) || data.len() > 1024 * 1024 {
        return None;
    }
    let mut offset = 8;
    let mut indexed = false;
    let mut palette = false;
    let mut transparency = false;
    let mut image = false;
    let mut end = false;
    while offset < data.len() {
        let length = u32::from_be_bytes(data.get(offset..offset + 4)?.try_into().ok()?) as usize;
        let chunk = data.get(offset + 4..offset.checked_add(12)?.checked_add(length)?)?;
        let (body, checksum) = chunk.split_at(chunk.len().checked_sub(4)?);
        if PNG_CRC.checksum(body) != u32::from_be_bytes(checksum.try_into().ok()?) {
            return None;
        }
        let tag = body.get(..4)?;
        if offset == 8 {
            if tag != b"IHDR" || length != 13 {
                return None;
            }
            let width = u32::from_be_bytes(body[4..8].try_into().ok()?);
            let height = u32::from_be_bytes(body[8..12].try_into().ok()?);
            if width == 0 || height == 0 || width > 4096 || height > 4096 {
                return None;
            }
            indexed = body[13] == 3;
        }
        match tag {
            b"PLTE" => {
                if image {
                    return None;
                }
                palette = true;
            }
            b"tRNS" => transparency = true,
            b"IDAT" => image = true,
            b"IEND" => {
                if length != 0 {
                    return None;
                }
                end = true;
            }
            _ => {}
        }
        offset += 12 + length;
        if end {
            break;
        }
    }
    if !end || !image || offset != data.len() {
        return None;
    }
    if !indexed || palette {
        return Some(data.to_vec());
    }
    let colors = arib_palette();
    let mut result = data[..33].to_vec();
    append_chunk(
        &mut result,
        b"PLTE",
        &colors
            .iter()
            .flat_map(|color| color[..3].iter().copied())
            .collect::<Vec<_>>(),
    );
    if !transparency {
        append_chunk(
            &mut result,
            b"tRNS",
            &colors.iter().map(|color| color[3]).collect::<Vec<_>>(),
        );
    }
    result.extend_from_slice(&data[33..]);
    Some(result)
}

fn append_chunk(png: &mut Vec<u8>, tag: &[u8; 4], data: &[u8]) {
    png.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = png.len();
    png.extend_from_slice(tag);
    png.extend_from_slice(data);
    let checksum = PNG_CRC.checksum(&png[start..]);
    png.extend_from_slice(&checksum.to_be_bytes());
}

fn arib_palette() -> Vec<[u8; 4]> {
    let mut colors = Vec::with_capacity(128);
    for level in [255, 170] {
        for bits in if level == 255 { 0..8 } else { 1..8 } {
            colors.push([
                if bits & 1 != 0 { level } else { 0 },
                if bits & 2 != 0 { level } else { 0 },
                if bits & 4 != 0 { level } else { 0 },
                255,
            ]);
        }
        if level == 255 {
            colors.push([0, 0, 0, 0]);
        }
    }
    for r in [0, 85, 170, 255] {
        for g in [0, 85, 170, 255] {
            for b in [0, 85, 170, 255] {
                let color = [r, g, b, 255];
                if !colors.contains(&color) {
                    colors.push(color);
                }
            }
        }
    }
    let translucent: Vec<_> = colors
        .iter()
        .filter(|color| color[3] != 0)
        .take(63)
        .map(|color| [color[0], color[1], color[2], 128])
        .collect();
    colors.extend(translucent);
    colors
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn png() -> Vec<u8> {
        let mut png = PNG_SIGNATURE.to_vec();
        append_chunk(&mut png, b"IHDR", &[0, 0, 0, 1, 0, 0, 0, 1, 8, 3, 0, 0, 0]);
        // zlib stream containing the scanline [filter=0, palette index=1].
        append_chunk(
            &mut png,
            b"IDAT",
            &[0x78, 0x9c, 0x63, 0x60, 0x04, 0x00, 0x00, 0x03, 0x00, 0x02],
        );
        append_chunk(&mut png, b"IEND", &[]);
        png
    }

    fn registry() -> (Registry, ServiceKey) {
        let registry = Registry::default();
        let key = ServiceKey {
            stream_id: 1,
            service_id: 101,
        };
        registry.put_cached_service(0, key, "Station".into(), String::new());
        (registry, key)
    }

    fn module(kind: u8, version: u8) -> Vec<u8> {
        let png = png();
        let mut module = vec![kind, 0xfe, 1, 0xf0, version];
        module.extend_from_slice(&(png.len() as u16).to_be_bytes());
        module.extend(png);
        module
    }

    #[test]
    fn resolves_both_arrival_orders_and_reassembles_mh_sections() {
        let (registry, key) = registry();
        let mut logos = Logos::default();
        logos.data(&registry, 4, 9, &module(5, 2));
        assert!(registry.get_logo(key).is_none());
        logos.reference(&registry, key, 4, &[1, 0xfe, 1, 0xf0, 3, 0, 9]);
        assert!(registry.get_logo(key).is_none(), "wrong logo version");
        logos.reference(&registry, key, 4, &[1, 0xfe, 1, 0xf0, 2, 0, 9]);
        assert!(registry.get_logo(key).is_some());

        let (registry, key) = self::registry();
        let mut logos = Logos::default();
        let module = module(7, 2);
        let fragment = |number, data| chibitv_b60::table::MhCdt {
            download_data_id: 9,
            original_network_id: 4,
            version_number: 1,
            current_next_indicator: true,
            section_number: number,
            last_section_number: 2,
            data_type: 1,
            data,
        };
        // The first MH-CDT can precede the SDT, and fragments arrive out of order.
        logos.mh_data(&registry, fragment(2, module[20..].to_vec()));
        logos.reference(&registry, key, 4, &[1, 0xfe, 1, 0xf0, 2, 0, 9, 7, 1, 2]);
        assert!(registry.get_logo(key).is_none());
        logos.mh_data(&registry, fragment(1, module[..20].to_vec()));
        assert_eq!(
            registry.get_logo(key).unwrap().as_slice(),
            browser_png(&png()).unwrap()
        );
    }

    fn carousel_section(kind: u16, transaction: u32, body: &[u8]) -> Vec<u8> {
        let mut message = vec![0x11, 3];
        message.extend(kind.to_be_bytes());
        message.extend(transaction.to_be_bytes());
        message.extend([0xff, 0]);
        message.extend((body.len() as u16).to_be_bytes());
        message.extend(body);
        let mut section = ((9 + message.len()) as u16 | 0xb000).to_be_bytes().to_vec();
        section.extend([0, 0, 0xc1, 0, 0]);
        section.extend(message);
        section.extend([0; 4]);
        section
    }

    #[test]
    fn assembles_named_carousel_modules_and_rejects_truncated_sections() {
        let (registry, key) = registry();
        let mut logos = Logos::default();
        let png = png();
        let mut module = vec![5, 0, 1, 0xfe, 1, 1, 0, 4, 0, 1, 0, 101];
        module.extend((png.len() as u16).to_be_bytes());
        module.extend(png);
        let mut dii = vec![0, 0, 0, 9, 0, 64];
        dii.extend([0; 12]);
        dii.extend([0, 1, 0, 7]);
        dii.extend((module.len() as u32).to_be_bytes());
        dii.extend([1, 9, 2, 7]);
        dii.extend(b"LOGO-05");
        let dii = carousel_section(0x1002, 1, &dii);
        for length in 0..dii.len() {
            logos.carousel(&registry, &dii[..length]);
        }
        assert!(logos.modules.is_empty());
        logos.carousel(&registry, &dii);
        for (index, chunk) in module.chunks(64).enumerate().rev() {
            let mut block = vec![0, 7, 1, 0xff];
            block.extend((index as u16).to_be_bytes());
            block.extend(chunk);
            logos.carousel(&registry, &carousel_section(0x1003, 9, &block));
        }
        assert_eq!(
            registry.get_logo(key).unwrap().as_slice(),
            browser_png(&self::png()).unwrap()
        );
    }

    #[test]
    fn restores_palette_and_rejects_damaged_pngs() {
        let broadcast = png();
        let png = browser_png(&broadcast).unwrap();
        assert_eq!(&png[37..41], b"PLTE");
        assert_eq!(browser_png(&png), Some(png.clone()));
        let colors = arib_palette();
        assert_eq!(colors.len(), 128);
        assert_eq!(colors[4], [0, 0, 255, 255]);
        assert_eq!(colors[8][3], 0);
        assert_eq!(colors[65], [0, 0, 0, 128]);
        assert_eq!(colors[127], [255, 255, 85, 128]);
        for length in 0..broadcast.len() {
            assert!(browser_png(&broadcast[..length]).is_none());
        }
        let mut damaged = broadcast;
        damaged[40] ^= 1;
        assert!(browser_png(&damaged).is_none());
    }
}

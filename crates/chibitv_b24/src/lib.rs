use std::collections::BTreeMap;

use kradical_jis::jis213_to_utf8;

mod additional_symbols;

use additional_symbols::ADDITIONAL_SYMBOLS;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum CodeWidth {
    One,
    Two,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum GraphicSet {
    Kanji,
    Alphanumeric,
    Hiragana,
    Katakana,
    Mosaic,
    ProportionalAlphanumeric,
    ProportionalHiragana,
    ProportionalKatakana,
    JisX0201Katakana,
    JisX0213Plane1,
    JisX0213Plane2,
    AdditionalSymbols,
    Macro,
    DrCs { set: u8, width: CodeWidth },
    Unknown(CodeWidth),
}

impl GraphicSet {
    fn drcs(set: u8) -> Self {
        Self::DrCs {
            set,
            width: if set == 0 {
                CodeWidth::Two
            } else {
                CodeWidth::One
            },
        }
    }

    fn is_two_byte(self) -> bool {
        matches!(
            self,
            Self::Kanji
                | Self::JisX0213Plane1
                | Self::JisX0213Plane2
                | Self::AdditionalSymbols
                | Self::DrCs {
                    width: CodeWidth::Two,
                    ..
                }
                | Self::Unknown(CodeWidth::Two)
        )
    }
}

#[derive(Clone, Debug)]
pub enum DrcsMapping {
    /// Assign encountered DRCS glyphs to the BMP private-use area from U+EC00.
    PrivateUse,
    /// Replace DRCS glyphs with U+FFFD for clients without a matching glyph registry.
    Replacement,
}

#[derive(Clone, Debug)]
pub struct Decoder {
    g: [GraphicSet; 4],
    gl: usize,
    gr: usize,
    drcs_mapping: DrcsMapping,
    drcs_characters: BTreeMap<(u8, u16), char>,
    next_drcs_code_point: u32,
    pending_nonspacing: String,
}

impl Default for Decoder {
    fn default() -> Self {
        Self {
            g: [
                GraphicSet::Kanji,
                GraphicSet::Alphanumeric,
                GraphicSet::Hiragana,
                GraphicSet::Macro,
            ],
            gl: 0,
            gr: 2,
            drcs_mapping: DrcsMapping::PrivateUse,
            drcs_characters: BTreeMap::new(),
            next_drcs_code_point: 0xEC00,
            pending_nonspacing: String::new(),
        }
    }
}

impl Decoder {
    pub fn with_drcs_mapping(drcs_mapping: DrcsMapping) -> Self {
        Self {
            drcs_mapping,
            ..Self::default()
        }
    }

    pub fn decode(&mut self, bytes: &[u8]) -> String {
        let mut output = String::new();
        let mut index = 0;

        while index < bytes.len() {
            let byte = bytes[index];
            index += 1;

            match byte {
                0x0E => self.gl = 1,
                0x0F => self.gl = 0,
                0x19 => self.decode_single_shift(bytes, &mut index, 2, &mut output),
                0x16 => Self::skip_bytes(bytes, &mut index, 1),
                0x1C => Self::skip_bytes(bytes, &mut index, 2),
                0x1B => self.decode_escape(bytes, &mut index),
                0x1D => self.decode_single_shift(bytes, &mut index, 3, &mut output),
                0x20 => self.push_graphic(&mut output, " "),
                0x0A | 0x0D => output.push('\n'),
                0x21..=0x7E => self.decode_graphic(bytes, &mut index, self.gl, byte, &mut output),
                0xA1..=0xFE => {
                    self.decode_graphic(bytes, &mut index, self.gr, byte & 0x7F, &mut output)
                }
                0x80..=0x9F => self.skip_c1(bytes, &mut index, byte),
                _ => {}
            }
        }

        output.push_str(&self.pending_nonspacing);
        self.pending_nonspacing.clear();

        output
    }

    fn decode_single_shift(
        &mut self,
        bytes: &[u8],
        index: &mut usize,
        g: usize,
        output: &mut String,
    ) {
        let Some(byte) = bytes.get(*index).copied() else {
            return;
        };
        *index += 1;

        match byte {
            0x21..=0x7E => self.decode_graphic(bytes, index, g, byte, output),
            0xA1..=0xFE => self.decode_graphic(bytes, index, g, byte & 0x7F, output),
            _ => {}
        }
    }

    fn decode_graphic(
        &mut self,
        bytes: &[u8],
        index: &mut usize,
        g: usize,
        byte: u8,
        output: &mut String,
    ) {
        let set = self.g[g];

        if set == GraphicSet::Macro {
            self.apply_default_macro(byte);
            return;
        }

        if let GraphicSet::DrCs { set, width } = set {
            let code = if width == CodeWidth::Two {
                let Some(trail) = bytes.get(*index).copied() else {
                    return;
                };
                *index += 1;
                (u16::from(byte) << 8) | u16::from(trail & 0x7F)
            } else {
                u16::from(byte)
            };
            let decoded = self.decode_drcs(set, code);
            self.push_graphic(output, &decoded);
            return;
        }

        if set.is_two_byte() {
            let Some(trail) = bytes.get(*index).copied() else {
                return;
            };
            *index += 1;

            let trail = trail & 0x7F;
            self.push_graphic(output, &decode_two_byte(set, byte, trail));
        } else {
            self.push_graphic(output, &decode_one_byte(set, byte));
        }
    }

    fn decode_drcs(&mut self, set: u8, code: u16) -> String {
        if matches!(self.drcs_mapping, DrcsMapping::Replacement) {
            return replacement();
        }
        if let Some(character) = self.drcs_characters.get(&(set, code)) {
            return character.to_string();
        }
        if self.next_drcs_code_point > 0xF8FF {
            return replacement();
        }

        let character = char::from_u32(self.next_drcs_code_point).unwrap();
        self.next_drcs_code_point += 1;
        self.drcs_characters.insert((set, code), character);
        character.to_string()
    }

    fn push_graphic(&mut self, output: &mut String, decoded: &str) {
        if decoded.is_empty() {
            return;
        }
        if decoded.chars().count() == 1 && decoded.chars().all(is_arib_nonspacing) {
            self.pending_nonspacing.push_str(decoded);
            return;
        }

        output.push_str(decoded);
        output.push_str(&self.pending_nonspacing);
        self.pending_nonspacing.clear();
    }

    fn apply_default_macro(&mut self, byte: u8) {
        let graphic_sets = match byte {
            0x60 => [
                GraphicSet::Kanji,
                GraphicSet::Alphanumeric,
                GraphicSet::Hiragana,
                GraphicSet::Macro,
            ],
            0x61 => [
                GraphicSet::Kanji,
                GraphicSet::Katakana,
                GraphicSet::Hiragana,
                GraphicSet::Macro,
            ],
            0x62 => [
                GraphicSet::Kanji,
                GraphicSet::drcs(1),
                GraphicSet::Hiragana,
                GraphicSet::Macro,
            ],
            0x63 => [
                GraphicSet::Mosaic,
                GraphicSet::Mosaic,
                GraphicSet::Mosaic,
                GraphicSet::Macro,
            ],
            0x64 => [
                GraphicSet::Mosaic,
                GraphicSet::Mosaic,
                GraphicSet::Mosaic,
                GraphicSet::Macro,
            ],
            0x65 => [
                GraphicSet::Mosaic,
                GraphicSet::drcs(1),
                GraphicSet::Mosaic,
                GraphicSet::Macro,
            ],
            0x66..=0x6A => {
                let first = (byte - 0x66) * 3 + 1;
                [
                    GraphicSet::drcs(first),
                    GraphicSet::drcs(first + 1),
                    GraphicSet::drcs(first + 2),
                    GraphicSet::Macro,
                ]
            }
            0x6B..=0x6D => [
                GraphicSet::Kanji,
                GraphicSet::drcs(byte - 0x69),
                GraphicSet::Hiragana,
                GraphicSet::Macro,
            ],
            0x6E => [
                GraphicSet::Katakana,
                GraphicSet::Hiragana,
                GraphicSet::Alphanumeric,
                GraphicSet::Macro,
            ],
            0x6F => [
                GraphicSet::Alphanumeric,
                GraphicSet::Mosaic,
                GraphicSet::drcs(1),
                GraphicSet::Macro,
            ],
            _ => return,
        };

        self.g = graphic_sets;
        self.gl = 0;
        self.gr = 2;
    }

    fn decode_escape(&mut self, bytes: &[u8], index: &mut usize) {
        let Some(first) = bytes.get(*index).copied() else {
            return;
        };
        *index += 1;

        match first {
            0x6E => self.gl = 2,
            0x6F => self.gl = 3,
            0x7C => self.gr = 3,
            0x7D => self.gr = 2,
            0x7E => self.gr = 1,
            0x24 => self.decode_multibyte_designation(bytes, index),
            0x28..=0x2B => {
                let g = (first - 0x28) as usize;
                self.decode_singlebyte_designation(bytes, index, g);
            }
            _ => {}
        }
    }

    fn decode_multibyte_designation(&mut self, bytes: &[u8], index: &mut usize) {
        let Some(next) = bytes.get(*index).copied() else {
            return;
        };
        *index += 1;

        if (0x28..=0x2B).contains(&next) {
            let g = (next - 0x28) as usize;
            let Some(next) = bytes.get(*index).copied() else {
                return;
            };
            *index += 1;
            self.g[g] = if next == 0x20 {
                let Some(final_byte) = bytes.get(*index).copied() else {
                    return;
                };
                *index += 1;
                drcs_from_final(final_byte, CodeWidth::Two)
            } else {
                graphic_set_from_final(next, true)
            };
        } else {
            self.g[0] = graphic_set_from_final(next, true);
        }
    }

    fn decode_singlebyte_designation(&mut self, bytes: &[u8], index: &mut usize, g: usize) {
        let Some(next) = bytes.get(*index).copied() else {
            return;
        };
        *index += 1;
        self.g[g] = if next == 0x20 {
            let Some(final_byte) = bytes.get(*index).copied() else {
                return;
            };
            *index += 1;
            drcs_from_final(final_byte, CodeWidth::One)
        } else {
            graphic_set_from_final(next, false)
        };
    }

    fn skip_c1(&self, bytes: &[u8], index: &mut usize, byte: u8) {
        match byte {
            0x8B | 0x91 | 0x93 | 0x94 | 0x97 | 0x98 => {
                Self::skip_bytes(bytes, index, 1);
            }
            0x90 | 0x92 => {
                let parameter_bytes = usize::from(bytes.get(*index) == Some(&0x20)) + 1;
                Self::skip_bytes(bytes, index, parameter_bytes);
            }
            0x95 => Self::skip_macro(bytes, index),
            0x9B => Self::skip_control_sequence(bytes, index),
            0x9D => Self::skip_time_control(bytes, index),
            _ => {}
        }
    }

    fn skip_bytes(bytes: &[u8], index: &mut usize, count: usize) {
        *index = index.saturating_add(count).min(bytes.len());
    }

    fn skip_control_sequence(bytes: &[u8], index: &mut usize) {
        while let Some(byte) = bytes.get(*index).copied() {
            *index += 1;
            if (0x40..=0x7E).contains(&byte) {
                break;
            }
        }
    }

    fn skip_time_control(bytes: &[u8], index: &mut usize) {
        match bytes.get(*index).copied() {
            Some(0x20 | 0x28) => Self::skip_bytes(bytes, index, 2),
            Some(0x29) => Self::skip_control_sequence(bytes, index),
            Some(_) => Self::skip_bytes(bytes, index, 1),
            None => {}
        }
    }

    fn skip_macro(bytes: &[u8], index: &mut usize) {
        let Some(command) = bytes.get(*index).copied() else {
            return;
        };
        *index += 1;

        if !matches!(command, 0x40 | 0x41) {
            return;
        }

        Self::skip_bytes(bytes, index, 1);
        while *index + 1 < bytes.len() {
            if bytes[*index] == 0x95 {
                match bytes[*index + 1] {
                    0x4F => {
                        *index += 2;
                        return;
                    }
                    0x40 | 0x41 => {
                        *index += 2;
                        Self::skip_bytes(bytes, index, 1);
                        continue;
                    }
                    _ => {}
                }
            }
            *index += 1;
        }

        *index = bytes.len();
    }
}

pub fn decode(bytes: &[u8]) -> String {
    Decoder::default().decode(bytes)
}

fn graphic_set_from_final(final_byte: u8, multibyte: bool) -> GraphicSet {
    let width = if multibyte {
        CodeWidth::Two
    } else {
        CodeWidth::One
    };

    match (multibyte, final_byte) {
        (true, 0x42) => GraphicSet::Kanji,
        (true, 0x39) => GraphicSet::JisX0213Plane1,
        (true, 0x3A) => GraphicSet::JisX0213Plane2,
        (true, 0x3B) => GraphicSet::AdditionalSymbols,
        (false, 0x4A) => GraphicSet::Alphanumeric,
        (false, 0x30) => GraphicSet::Hiragana,
        (false, 0x31) => GraphicSet::Katakana,
        (false, 0x32..=0x35) => GraphicSet::Mosaic,
        (false, 0x36) => GraphicSet::ProportionalAlphanumeric,
        (false, 0x37) => GraphicSet::ProportionalHiragana,
        (false, 0x38) => GraphicSet::ProportionalKatakana,
        (false, 0x49) => GraphicSet::JisX0201Katakana,
        (false, 0x70) => GraphicSet::Macro,
        _ => GraphicSet::Unknown(width),
    }
}

fn drcs_from_final(final_byte: u8, width: CodeWidth) -> GraphicSet {
    match (width, final_byte) {
        (CodeWidth::Two, 0x40) => GraphicSet::drcs(0),
        (CodeWidth::One, 0x41..=0x4F) => GraphicSet::drcs(final_byte - 0x40),
        _ => GraphicSet::Unknown(width),
    }
}

fn decode_one_byte(set: GraphicSet, byte: u8) -> String {
    match set {
        GraphicSet::Mosaic => String::new(),
        GraphicSet::Alphanumeric | GraphicSet::ProportionalAlphanumeric => {
            decode_alphanumeric(byte)
                .map(|c| c.to_string())
                .unwrap_or_else(replacement)
        }
        GraphicSet::Hiragana | GraphicSet::ProportionalHiragana => decode_hiragana(byte)
            .map(|c| c.to_string())
            .unwrap_or_else(replacement),
        GraphicSet::Katakana | GraphicSet::ProportionalKatakana => decode_katakana(byte)
            .map(|c| c.to_string())
            .unwrap_or_else(replacement),
        GraphicSet::JisX0201Katakana => decode_jis_x0201_katakana(byte)
            .map(|c| c.to_string())
            .unwrap_or_else(replacement),
        _ => replacement(),
    }
}

fn decode_two_byte(set: GraphicSet, lead: u8, trail: u8) -> String {
    match set {
        GraphicSet::Kanji => decode_kanji(lead, trail).unwrap_or_else(replacement),
        GraphicSet::JisX0213Plane1 => {
            decode_jis_x0213(false, lead, trail).unwrap_or_else(replacement)
        }
        GraphicSet::JisX0213Plane2 => {
            decode_jis_x0213(true, lead, trail).unwrap_or_else(replacement)
        }
        GraphicSet::AdditionalSymbols => decode_additional_symbol(lead, trail)
            .map(|c| c.to_string())
            .unwrap_or_else(replacement),
        _ => replacement(),
    }
}

fn decode_alphanumeric(byte: u8) -> Option<char> {
    match byte {
        0x5C => Some('¥'),
        0x21..=0x7E => Some(byte as char),
        _ => None,
    }
}

fn decode_hiragana(byte: u8) -> Option<char> {
    const TABLE: &str = "ぁあぃいぅうぇえぉおかがきぎくぐけげこござざしじすずせぜそぞただちぢっつづてでとどなにぬねのはばぱひびぴふぶぷへべぺほぼぽまみむめもゃやゅゆょよらりるれろゎわゐゑをん";

    decode_arib_kana(TABLE, byte, 'ゝ', 'ゞ')
}

fn decode_katakana(byte: u8) -> Option<char> {
    const TABLE: &str = "ァアィイゥウェエォオカガキギクグケゲコゴサザシジスズセゼソゾタダチヂッツヅテデトドナニヌネノハバパヒビピフブプヘベペホボポマミムメモャヤュユョヨラリルレロヮワヰヱヲン";

    decode_arib_kana(TABLE, byte, 'ヽ', 'ヾ')
}

fn decode_arib_kana(
    table: &str,
    byte: u8,
    iteration: char,
    voiced_iteration: char,
) -> Option<char> {
    match byte {
        0x21..=0x73 => table.chars().nth((byte - 0x21) as usize),
        0x77 => Some(iteration),
        0x78 => Some(voiced_iteration),
        0x79 => Some('ー'),
        0x7A => Some('。'),
        0x7B => Some('「'),
        0x7C => Some('」'),
        0x7D => Some('、'),
        0x7E => Some('・'),
        _ => None,
    }
}

fn decode_kanji(lead: u8, trail: u8) -> Option<String> {
    let nonspacing = match (lead, trail) {
        (0x21, 0x2D) => Some('\u{0301}'),
        (0x21, 0x2E) => Some('\u{0300}'),
        (0x21, 0x2F) => Some('\u{0308}'),
        (0x21, 0x30) => Some('\u{0302}'),
        (0x21, 0x31) => Some('\u{0305}'),
        (0x21, 0x32) => Some('\u{0332}'),
        (0x22, 0x7E) => Some('\u{20DD}'),
        _ => None,
    };
    if let Some(nonspacing) = nonspacing {
        return Some(nonspacing.to_string());
    }
    if (0x7A..=0x7E).contains(&lead) {
        return decode_additional_symbol(lead, trail).map(|c| c.to_string());
    }

    decode_jis_x0213(false, lead, trail)
}

fn is_arib_nonspacing(character: char) -> bool {
    matches!(
        character,
        '\u{0301}' | '\u{0300}' | '\u{0308}' | '\u{0302}' | '\u{0305}' | '\u{0332}' | '\u{20DD}'
    )
}

fn decode_jis_x0213(plane_two: bool, lead: u8, trail: u8) -> Option<String> {
    if !(0x21..=0x7E).contains(&lead) || !(0x21..=0x7E).contains(&trail) {
        return None;
    }

    let mut code = (u32::from(lead | 0x80) << 8) | u32::from(trail | 0x80);
    if plane_two {
        code |= 0x8F_00_00;
    }
    jis213_to_utf8(code).map(str::to_owned)
}

fn decode_jis_x0201_katakana(byte: u8) -> Option<char> {
    const TABLE: [char; 63] = [
        '｡', '｢', '｣', '､', '･', 'ｦ', 'ｧ', 'ｨ', 'ｩ', 'ｪ', 'ｫ', 'ｬ', 'ｭ', 'ｮ', 'ｯ', 'ｰ', 'ｱ', 'ｲ',
        'ｳ', 'ｴ', 'ｵ', 'ｶ', 'ｷ', 'ｸ', 'ｹ', 'ｺ', 'ｻ', 'ｼ', 'ｽ', 'ｾ', 'ｿ', 'ﾀ', 'ﾁ', 'ﾂ', 'ﾃ', 'ﾄ',
        'ﾅ', 'ﾆ', 'ﾇ', 'ﾈ', 'ﾉ', 'ﾊ', 'ﾋ', 'ﾌ', 'ﾍ', 'ﾎ', 'ﾏ', 'ﾐ', 'ﾑ', 'ﾒ', 'ﾓ', 'ﾔ', 'ﾕ', 'ﾖ',
        'ﾗ', 'ﾘ', 'ﾙ', 'ﾚ', 'ﾛ', 'ﾜ', 'ﾝ', 'ﾞ', 'ﾟ',
    ];

    (0x21..=0x5F)
        .contains(&byte)
        .then(|| TABLE[(byte - 0x21) as usize])
}

fn decode_additional_symbol(lead: u8, trail: u8) -> Option<char> {
    if !(0x75..=0x7E).contains(&lead) || !(0x21..=0x7E).contains(&trail) {
        return None;
    }

    let row = usize::from(lead - 0x75);
    let cell = usize::from(trail - 0x21);
    let code_point = ADDITIONAL_SYMBOLS[row * 94 + cell];
    (code_point != 0xFFFD)
        .then(|| char::from_u32(code_point))
        .flatten()
}

fn replacement() -> String {
    "\u{FFFD}".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_locking_shift_alphanumeric() {
        assert_eq!(decode(b"\x0ETOKYO MX1"), "TOKYO MX1");
    }

    #[test]
    fn decodes_default_kanji_set() {
        assert_eq!(decode(&[0x24, 0x22]), "あ");
    }

    #[test]
    fn decodes_single_shift_hiragana() {
        assert_eq!(decode(&[0x19, 0x22]), "あ");
    }

    #[test]
    fn decodes_gr_hiragana() {
        assert_eq!(decode(&[0xA2]), "あ");
    }
    #[test]
    fn decodes_additional_kanji_and_symbols() {
        assert_eq!(decode(&[0x1B, 0x24, 0x3B, 0x75, 0x21]), "㐂");
        assert_eq!(decode(&[0x1B, 0x24, 0x3B, 0x75, 0x22]), "𠅘");
        assert_eq!(decode(&[0x1B, 0x24, 0x3B, 0x7A, 0x23]), "❗");
        assert_eq!(decode(&[0x1B, 0x24, 0x3B, 0x7E, 0x21]), "Ⅰ");
    }

    #[test]
    fn replaces_undefined_additional_symbol_cells() {
        assert_eq!(decode(&[0x1B, 0x24, 0x3B, 0x77, 0x21]), "�");
    }

    #[test]
    fn decodes_arib_alphanumeric_yen_sign() {
        assert_eq!(decode(b"\x0E\\"), "¥");
    }

    #[test]
    fn decodes_arib_kana_tail() {
        assert_eq!(decode(&[0x19, 0x77, 0x19, 0x79, 0x19, 0x7A]), "ゝー。");
        assert_eq!(
            decode(&[0x1B, 0x29, 0x31, 0x0E, 0x77, 0x79, 0x7A]),
            "ヽー。"
        );
    }

    #[test]
    fn rejects_undefined_arib_kana_cells() {
        assert_eq!(decode(&[0x19, 0x74]), "�");
        assert_eq!(decode(&[0x1B, 0x29, 0x31, 0x0E, 0x74]), "�");
    }

    #[test]
    fn initializes_g3_as_macro() {
        assert_eq!(decode(&[0x1D, 0x61, 0x0E, 0x22]), "ア");
    }

    #[test]
    fn applies_default_macro_to_restore_designations() {
        assert_eq!(
            decode(&[
                0x1B, 0x29, 0x31, // Designate Katakana to G1.
                0x1D, 0x60, // Restore the default sets through G3.
                0x0E, 0x41,
            ]),
            "A"
        );
    }

    #[test]
    fn consumes_parameterized_c0_controls() {
        assert_eq!(
            decode(&[0x0E, b'A', 0x16, 0x42, b'B', 0x1C, 0x41, 0x42, b'C',]),
            "ABC"
        );
    }

    #[test]
    fn consumes_parameterized_c1_controls() {
        assert_eq!(
            decode(&[
                0x0E, b'A', 0x8B, 0x45, 0x90, 0x20, 0x42, 0x91, 0x40, 0x92, 0x20, 0x42, 0x93, 0x40,
                0x94, 0x40, 0x97, 0x40, 0x98, 0x42, 0x9D, 0x20, 0x41, b'B',
            ]),
            "AB"
        );
    }

    #[test]
    fn consumes_csi_and_macro_definitions() {
        assert_eq!(
            decode(&[
                0x0E, b'A', 0x9B, 0x31, 0x3B, 0x32, 0x20, 0x53, b'B', 0x95, 0x40, 0x21, b'X', b'Y',
                0x95, 0x4F, b'C',
            ]),
            "ABC"
        );
    }
    #[test]
    fn decodes_jis_x0213_planes() {
        assert_eq!(decode(&[0x1B, 0x24, 0x39, 0x21, 0x21]), "　");
        assert_eq!(decode(&[0x1B, 0x24, 0x3A, 0x21, 0x21]), "𠂉");
    }

    #[test]
    fn preserves_unknown_multibyte_set_width() {
        assert_eq!(
            decode(&[
                0x1B, 0x24, 0x29, 0x7F, 0x0E, 0x21, 0x22, 0x1B, 0x29, 0x4A, 0x41,
            ]),
            "�A"
        );
    }

    #[test]
    fn treats_default_macro_drcs_as_one_byte() {
        assert_eq!(
            decode(&[0x1D, 0x62, 0x0E, 0x21, 0x1D, 0x60, 0x0E, 0x41]),
            "\u{EC00}A"
        );
    }

    #[test]
    fn parses_one_and_two_byte_drcs_designations() {
        assert_eq!(
            decode(&[
                0x1B, 0x29, 0x20, 0x41, 0x0E, 0x21, // DRCS-1 in G1.
                0x1B, 0x24, 0x29, 0x20, 0x40, 0x0E, 0x21, 0x22, // DRCS-0 in G1.
                0x1B, 0x29, 0x4A, 0x0E, 0x41,
            ]),
            "\u{EC00}\u{EC01}A"
        );
    }

    #[test]
    fn can_replace_drcs_instead_of_using_private_use_characters() {
        let mut decoder = Decoder::with_drcs_mapping(DrcsMapping::Replacement);
        assert_eq!(decoder.decode(&[0x1D, 0x62, 0x0E, 0x21]), "�");
    }

    #[test]
    fn reuses_private_use_character_for_the_same_drcs_code() {
        assert_eq!(
            decode(&[0x1D, 0x62, 0x0E, 0x21, 0x21, 0x22]),
            "\u{EC00}\u{EC00}\u{EC01}"
        );
    }

    #[test]
    fn ignores_mosaic_characters_when_converting_to_unicode() {
        assert_eq!(
            decode(&[0x1B, 0x29, 0x32, 0x0E, 0x21, 0x1B, 0x29, 0x4A, 0x41]),
            "A"
        );
    }

    #[test]
    fn converts_and_reorders_nonspacing_characters() {
        assert_eq!(decode(&[0x21, 0x2D, 0x24, 0x22]), "あ\u{0301}");
        assert_eq!(decode(&[0x22, 0x7E, 0x24, 0x22]), "あ\u{20DD}");
    }

    #[test]
    fn maps_kanji_rows_90_to_94_as_additional_symbols() {
        assert_eq!(decode(&[0x7A, 0x23]), "❗");
    }
}

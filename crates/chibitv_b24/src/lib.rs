use encoding_rs::EUC_JP;

mod additional_symbols;

use additional_symbols::ADDITIONAL_SYMBOLS;

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
    AdditionalSymbols,
    Macro,
    DrCs,
    Unknown,
}

impl GraphicSet {
    fn is_two_byte(self) -> bool {
        matches!(self, Self::Kanji | Self::AdditionalSymbols | Self::DrCs)
    }
}

#[derive(Clone, Debug)]
pub struct Decoder {
    g: [GraphicSet; 4],
    gl: usize,
    gr: usize,
}

impl Default for Decoder {
    fn default() -> Self {
        Self {
            g: [
                GraphicSet::Kanji,
                GraphicSet::Alphanumeric,
                GraphicSet::Hiragana,
                GraphicSet::Katakana,
            ],
            gl: 0,
            gr: 2,
        }
    }
}

impl Decoder {
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
                0x1B => self.decode_escape(bytes, &mut index),
                0x1D => self.decode_single_shift(bytes, &mut index, 3, &mut output),
                0x20 => output.push(' '),
                0x0A | 0x0D => output.push('\n'),
                0x21..=0x7E => self.decode_graphic(bytes, &mut index, self.gl, byte, &mut output),
                0xA1..=0xFE => {
                    self.decode_graphic(bytes, &mut index, self.gr, byte & 0x7F, &mut output)
                }
                0x80..=0x9F => self.skip_c1(bytes, &mut index, byte),
                _ => {}
            }
        }

        output
    }

    fn decode_single_shift(&self, bytes: &[u8], index: &mut usize, g: usize, output: &mut String) {
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
        &self,
        bytes: &[u8],
        index: &mut usize,
        g: usize,
        byte: u8,
        output: &mut String,
    ) {
        let set = self.g[g];

        if set.is_two_byte() {
            let Some(trail) = bytes.get(*index).copied() else {
                return;
            };
            *index += 1;

            let trail = trail & 0x7F;
            output.push_str(&decode_two_byte(set, byte, trail));
        } else {
            output.push_str(&decode_one_byte(set, byte));
        }
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
            let Some(final_byte) = bytes.get(*index).copied() else {
                return;
            };
            *index += 1;
            self.g[g] = graphic_set_from_final(final_byte, true);
        } else {
            self.g[0] = graphic_set_from_final(next, true);
        }
    }

    fn decode_singlebyte_designation(&mut self, bytes: &[u8], index: &mut usize, g: usize) {
        let Some(final_byte) = bytes.get(*index).copied() else {
            return;
        };
        *index += 1;
        self.g[g] = graphic_set_from_final(final_byte, false);
    }

    fn skip_c1(&self, bytes: &[u8], index: &mut usize, byte: u8) {
        if byte != 0x9B {
            return;
        }

        while let Some(byte) = bytes.get(*index).copied() {
            *index += 1;
            if (0x40..=0x7E).contains(&byte) {
                break;
            }
        }
    }
}

pub fn decode(bytes: &[u8]) -> String {
    Decoder::default().decode(bytes)
}

fn graphic_set_from_final(final_byte: u8, multibyte: bool) -> GraphicSet {
    match (multibyte, final_byte) {
        (true, 0x42) => GraphicSet::Kanji,
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
        (_, 0x40..=0x4F) => GraphicSet::DrCs,
        _ => GraphicSet::Unknown,
    }
}

fn decode_one_byte(set: GraphicSet, byte: u8) -> String {
    match set {
        GraphicSet::Alphanumeric | GraphicSet::ProportionalAlphanumeric => {
            decode_alphanumeric(byte).to_string()
        }
        GraphicSet::Hiragana | GraphicSet::ProportionalHiragana => {
            decode_jis_row(0x24, byte).unwrap_or_else(replacement)
        }
        GraphicSet::Katakana | GraphicSet::ProportionalKatakana => {
            decode_jis_row(0x25, byte).unwrap_or_else(replacement)
        }
        GraphicSet::JisX0201Katakana => decode_jis_x0201_katakana(byte)
            .map(|c| c.to_string())
            .unwrap_or_else(replacement),
        _ => replacement(),
    }
}

fn decode_two_byte(set: GraphicSet, lead: u8, trail: u8) -> String {
    match set {
        GraphicSet::Kanji => decode_jis(lead, trail).unwrap_or_else(replacement),
        GraphicSet::AdditionalSymbols => decode_additional_symbol(lead, trail)
            .map(|c| c.to_string())
            .unwrap_or_else(replacement),
        _ => replacement(),
    }
}

fn decode_alphanumeric(byte: u8) -> char {
    match byte {
        0x21..=0x7E => byte as char,
        _ => '\u{FFFD}',
    }
}

fn decode_jis_row(row: u8, cell: u8) -> Option<String> {
    decode_jis(row, cell)
}

fn decode_jis(lead: u8, trail: u8) -> Option<String> {
    if !(0x21..=0x7E).contains(&lead) || !(0x21..=0x7E).contains(&trail) {
        return None;
    }

    let euc = [lead | 0x80, trail | 0x80];
    let (decoded, _, had_errors) = EUC_JP.decode(&euc);
    (!had_errors).then(|| decoded.into_owned())
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
}

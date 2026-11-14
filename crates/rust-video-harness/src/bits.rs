/// Test bit writer with emulation prevention.
#[derive(Default)]
pub struct Bits {
    /// Finished bytes.
    bytes: Vec<u8>,
    /// Open byte under fill.
    current: u8,
    /// Bits filled in the open byte.
    filled: u8,
}

impl Bits {
    /// Empty writer.
    #[must_use]
    pub fn new() -> Self {
        Self {
            bytes: Vec::new(),
            current: 0,
            filled: 0,
        }
    }

    /// Single bit.
    pub fn bit(&mut self, set: bool) {
        if set {
            self.current |= 1 << (7 - self.filled);
        }
        self.filled += 1;
        if self.filled == 8 {
            self.bytes.push(self.current);
            self.current = 0;
            self.filled = 0;
        }
    }

    /// Low `count` bits of `value`, most significant first.
    pub fn bits(&mut self, value: u32, count: u32) {
        for index in (0..count).rev() {
            self.bit(value >> index & 1 != 0);
        }
    }

    /// Unsigned Exp-Golomb code.
    pub fn ue(&mut self, value: u32) {
        let code = value + 1;
        let width = 32 - code.leading_zeros();
        for _ in 1..width {
            self.bit(false);
        }
        self.bits(code, width);
    }

    /// Signed Exp-Golomb code.
    pub fn se(&mut self, value: i32) {
        let code = if value > 0 {
            u32::try_from(value).unwrap_or(u32::MAX) * 2 - 1
        } else {
            u32::try_from(-value).unwrap_or(u32::MAX) * 2
        };
        self.ue(code);
    }

    /// Finished EBSP with emulation prevention inserted.
    #[must_use]
    pub fn finish(mut self) -> Vec<u8> {
        self.bit(true);
        while self.filled != 0 {
            self.bit(false);
        }
        let mut out = Vec::new();
        let mut zeros = 0;
        for byte in self.bytes {
            if zeros == 2 && byte <= 3 {
                out.push(3);
                zeros = 0;
            }
            out.push(byte);
            zeros = if byte == 0 { zeros + 1 } else { 0 };
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::Bits;

    #[test]
    fn golomb_codes_match_bits() {
        let mut writer = Bits::new();
        writer.ue(0);
        assert_eq!(writer.finish(), vec![0xC0]);
        let mut writer = Bits::new();
        writer.se(-1);
        assert_eq!(writer.finish(), vec![0x70]);
    }

    #[test]
    fn finish_inserts_emulation_prevention() {
        let mut writer = Bits::new();
        writer.bits(0, 24);
        assert_eq!(writer.finish(), vec![0, 0, 3, 0, 0x80]);
    }
}

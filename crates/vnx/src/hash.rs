//! VNX type, service and topic identities use CRC-64/XZ.
#[derive(Clone, Debug)]
pub struct Crc64(u64);
impl Default for Crc64 {
    fn default() -> Self {
        Self::new()
    }
}
impl Crc64 {
    pub fn new() -> Self {
        Self(!0)
    }
    pub fn update(&mut self, bytes: impl AsRef<[u8]>) {
        for &byte in bytes.as_ref() {
            self.0 ^= u64::from(byte);
            for _ in 0..8 {
                self.0 = (self.0 >> 1) ^ (0xc96c5795d7870f42 & 0u64.wrapping_sub(self.0 & 1));
            }
        }
    }
    pub fn as_u64(&self) -> u64 {
        !self.0
    }
}
pub fn hash64(bytes: impl AsRef<[u8]>) -> u64 {
    let mut hash = Crc64::new();
    hash.update(bytes);
    hash.as_u64()
}
pub type Hash64 = u64;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn crc_xz_vectors() {
        assert_eq!(hash64(""), 0);
        assert_eq!(hash64("123456789"), 0x995dc9bbdf1939fa);
        assert_eq!(hash64("mmx.KeyFile"), 0xdf868931a939cba1);
    }
}

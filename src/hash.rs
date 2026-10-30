pub(crate) fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        encoded.push(char::from(DIGITS[usize::from(byte >> 4)]));
        encoded.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::hex;

    #[test]
    fn preserves_lowercase_and_leading_zero_bytes() {
        for (bytes, expected) in [
            (&[][..], ""),
            (&[0, 1, 15, 16, 127, 128, 255][..], "00010f107f80ff"),
        ] {
            assert_eq!(hex(bytes), expected);
        }
    }
}

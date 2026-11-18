//! Byte encodings: hex and base64 (RFC 4648, standard and URL-safe
//! alphabets). Small enough not to need a crate.

pub fn hex_encode(b: &[u8]) -> String {
    const D: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(b.len() * 2);
    for &x in b {
        s.push(D[(x >> 4) as usize] as char);
        s.push(D[(x & 15) as usize] as char);
    }
    s
}

pub fn hex_decode(s: &str) -> Result<Vec<u8>, String> {
    let digit = |c: u8| -> Result<u8, String> {
        match c {
            b'0'..=b'9' => Ok(c - b'0'),
            b'a'..=b'f' => Ok(c - b'a' + 10),
            b'A'..=b'F' => Ok(c - b'A' + 10),
            _ => Err(format!("Invalid hex digit: {:?}", c as char)),
        }
    };
    let s = s.as_bytes();
    if !s.len().is_multiple_of(2) {
        return Err("Hex string has an odd number of digits".into());
    }
    s.as_chunks::<2>().0.iter().map(|p| Ok(digit(p[0])? << 4 | digit(p[1])?)).collect()
}

const STD: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// Standard alphabet with `=` padding, or the URL-safe alphabet without.
pub fn base64_encode(b: &[u8], url: bool) -> String {
    let a = if url { URL } else { STD };
    let mut s = String::with_capacity(b.len().div_ceil(3) * 4);
    for c in b.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                s.push(a[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else if !url {
                s.push('=');
            }
        }
    }
    s
}

/// Accepts either alphabet, with or without padding; whitespace is ignored.
pub fn base64_decode(s: &str) -> Result<Vec<u8>, String> {
    let val = |c: u8| -> Result<u32, String> {
        Ok(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return Err(format!("Invalid base64 character: {:?}", c as char)),
        } as u32)
    };
    let chars: Vec<u8> = s.bytes().filter(|c| !c.is_ascii_whitespace()).collect();
    let body: &[u8] = match chars.iter().position(|&c| c == b'=') {
        Some(i) if chars[i..].iter().all(|&c| c == b'=') && chars[i..].len() <= 2 => &chars[..i],
        Some(_) => return Err("Misplaced base64 padding".into()),
        None => &chars,
    };
    if body.len() % 4 == 1 {
        return Err("Truncated base64".into());
    }
    let mut out = Vec::with_capacity(body.len() * 3 / 4);
    for c in body.chunks(4) {
        let mut n = 0u32;
        for (i, &x) in c.iter().enumerate() {
            n |= val(x)? << (18 - 6 * i);
        }
        out.push((n >> 16) as u8);
        if c.len() > 2 {
            out.push((n >> 8) as u8);
        }
        if c.len() > 3 {
            out.push(n as u8);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        for n in 0..40usize {
            let b: Vec<u8> = (0..n).map(|i| (i * 37 + 11) as u8).collect();
            assert_eq!(hex_decode(&hex_encode(&b)).unwrap(), b);
            assert_eq!(base64_decode(&base64_encode(&b, false)).unwrap(), b);
            assert_eq!(base64_decode(&base64_encode(&b, true)).unwrap(), b);
        }
    }

    #[test]
    fn rfc4648_vectors() {
        for (plain, enc) in
            [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"), ("foob", "Zm9vYg=="), ("fooba", "Zm9vYmE="), ("foobar", "Zm9vYmFy")]
        {
            assert_eq!(base64_encode(plain.as_bytes(), false), enc);
        }
        assert!(base64_decode("Zm9v=Yg").is_err());
        assert!(hex_decode("abc").is_err());
    }
}

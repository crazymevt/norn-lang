//! Compression and cryptography (M9 part 9.12): the primitives behind
//! norn.compress, norn.zip and norn.crypto. Compression is flate2 (its
//! pure-Rust miniz_oxide); digests, HMAC, PBKDF2 and secure random are
//! ring, which rustls already links.

use crate::vm::{Error, Result};
use norn_rt::string::bytes_new;
use norn_rt::{Kind, Mutator, Value};
use std::io::{Read, Write};

/// Bytes, or a string as its UTF-8.
fn data(v: Value, op: &str) -> Result<Vec<u8>> {
    if v.is(Kind::Bytes) {
        Ok(v.as_bytes().to_vec())
    } else if v.is(Kind::String) {
        Ok(v.as_str().as_bytes().to_vec())
    } else {
        Err(Error::of("TypeError", format!("{op} expects bytes or a string, got {}", v.type_name())))
    }
}

fn kw(v: Value) -> String {
    if v.is_keyword() { v.sym_name().to_owned() } else { String::new() }
}

fn bad_data(what: &str, e: impl std::fmt::Display) -> Error {
    Error::of("ParseError", format!("Not {what} data: {e}")).with("source", crate::vm::ErrData::Str(what.into()))
}

/// `(%deflate data level format)`: format :raw, :zlib or :gzip.
pub(crate) fn deflate(m: &mut Mutator, a: &[Value]) -> Result<Value> {
    let input = data(a[0], "compress")?;
    let level = if a[1].is_fixnum() { a[1].as_fixnum().clamp(0, 9) as u32 } else { 6 };
    let c = flate2::Compression::new(level);
    let out = match kw(a[2]).as_str() {
        "raw" => {
            let mut e = flate2::write::DeflateEncoder::new(Vec::new(), c);
            e.write_all(&input).and_then(|_| e.finish())
        }
        "zlib" => {
            let mut e = flate2::write::ZlibEncoder::new(Vec::new(), c);
            e.write_all(&input).and_then(|_| e.finish())
        }
        "gzip" => {
            let mut e = flate2::write::GzEncoder::new(Vec::new(), c);
            e.write_all(&input).and_then(|_| e.finish())
        }
        f => return Err(Error::new(format!("compress: no format :{f} (:raw, :zlib or :gzip)"))),
    }
    .map_err(|e| Error::new(format!("compress failed: {e}")))?;
    Ok(bytes_new(m, &out))
}

/// `(%inflate data format max)`: at most max bytes out (a bigger result is
/// an error: compressed input can claim far more than it takes).
pub(crate) fn inflate(m: &mut Mutator, a: &[Value]) -> Result<Value> {
    let input = data(a[0], "decompress")?;
    let max = if a[2].is_fixnum() { a[2].as_fixnum().max(0) as u64 } else { u64::MAX };
    let fmt = kw(a[1]);
    let mut out = Vec::new();
    let reader: Box<dyn Read> = match fmt.as_str() {
        "raw" => Box::new(flate2::read::DeflateDecoder::new(&input[..])),
        "zlib" => Box::new(flate2::read::ZlibDecoder::new(&input[..])),
        "gzip" => Box::new(flate2::read::MultiGzDecoder::new(&input[..])),
        f => return Err(Error::new(format!("decompress: no format :{f} (:raw, :zlib or :gzip)"))),
    };
    let n = reader.take(max.saturating_add(1)).read_to_end(&mut out).map_err(|e| bad_data(&fmt, e))?;
    if n as u64 > max {
        return Err(Error::new(format!("decompress: the result is larger than :max-size ({max} bytes)")));
    }
    Ok(bytes_new(m, &out))
}

/// `(%crc32 data)`: the CRC-32 (IEEE, as zip and gzip use).
pub(crate) fn crc32(a: &[Value]) -> Result<Value> {
    let mut h = flate2::Crc::new();
    h.update(&data(a[0], "crc32")?);
    Ok(Value::fix(h.sum() as i64))
}

fn digest_alg(v: Value) -> Result<&'static ring::digest::Algorithm> {
    Ok(match kw(v).as_str() {
        "sha1" => &ring::digest::SHA1_FOR_LEGACY_USE_ONLY,
        "sha256" => &ring::digest::SHA256,
        "sha384" => &ring::digest::SHA384,
        "sha512" => &ring::digest::SHA512,
        a => return Err(Error::new(format!("No digest :{a} (:sha256, :sha384, :sha512, or :sha1 for old protocols)"))),
    })
}

fn hmac_alg(v: Value) -> Result<ring::hmac::Algorithm> {
    Ok(match kw(v).as_str() {
        "sha1" => ring::hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY,
        "sha256" => ring::hmac::HMAC_SHA256,
        "sha384" => ring::hmac::HMAC_SHA384,
        "sha512" => ring::hmac::HMAC_SHA512,
        a => return Err(Error::new(format!("No HMAC :{a} (:sha256, :sha384, :sha512 or :sha1)"))),
    })
}

/// `(%digest alg data)`.
pub(crate) fn digest(m: &mut Mutator, a: &[Value]) -> Result<Value> {
    let d = ring::digest::digest(digest_alg(a[0])?, &data(a[1], "digest")?);
    Ok(bytes_new(m, d.as_ref()))
}

/// `(%hmac alg key data)`.
pub(crate) fn hmac(m: &mut Mutator, a: &[Value]) -> Result<Value> {
    let key = ring::hmac::Key::new(hmac_alg(a[0])?, &data(a[1], "hmac")?);
    let tag = ring::hmac::sign(&key, &data(a[2], "hmac")?);
    Ok(bytes_new(m, tag.as_ref()))
}

/// `(%hmac-verify alg key data tag)`: in constant time.
pub(crate) fn hmac_verify(a: &[Value]) -> Result<Value> {
    let key = ring::hmac::Key::new(hmac_alg(a[0])?, &data(a[1], "hmac-verify")?);
    Ok(Value::bool(ring::hmac::verify(&key, &data(a[2], "hmac-verify")?, &data(a[3], "hmac-verify")?).is_ok()))
}

/// `(%pbkdf2 alg iterations salt password n)`: an n-byte key.
pub(crate) fn pbkdf2(m: &mut Mutator, a: &[Value]) -> Result<Value> {
    let alg = match kw(a[0]).as_str() {
        "sha1" => ring::pbkdf2::PBKDF2_HMAC_SHA1,
        "sha256" => ring::pbkdf2::PBKDF2_HMAC_SHA256,
        "sha384" => ring::pbkdf2::PBKDF2_HMAC_SHA384,
        "sha512" => ring::pbkdf2::PBKDF2_HMAC_SHA512,
        x => return Err(Error::new(format!("No PBKDF2 :{x} (:sha256, :sha384, :sha512 or :sha1)"))),
    };
    let iters = if a[1].is_fixnum() && a[1].as_fixnum() > 0 { a[1].as_fixnum() as u32 } else { return Err(Error::new("pbkdf2: iterations must be a positive integer")) };
    let n = if a[4].is_fixnum() && (1..=1024).contains(&a[4].as_fixnum()) { a[4].as_fixnum() as usize } else { return Err(Error::new("pbkdf2: a key of 1 to 1024 bytes")) };
    let mut out = vec![0u8; n];
    ring::pbkdf2::derive(alg, std::num::NonZeroU32::new(iters).unwrap(), &data(a[2], "pbkdf2")?, &data(a[3], "pbkdf2")?, &mut out);
    Ok(bytes_new(m, &out))
}

/// `(%secure-random n)`: n bytes from the system's secure generator.
pub(crate) fn secure_random(m: &mut Mutator, a: &[Value]) -> Result<Value> {
    use ring::rand::SecureRandom;
    let n = if a[0].is_fixnum() && (0..=1 << 20).contains(&a[0].as_fixnum()) { a[0].as_fixnum() as usize } else { return Err(Error::new("secure-random-bytes: 0 to 1048576 bytes")) };
    let mut out = vec![0u8; n];
    ring::rand::SystemRandom::new().fill(&mut out).map_err(|_| Error::new("The system's secure random generator failed"))?;
    Ok(bytes_new(m, &out))
}

/// `(%constant-time= a b)`: equal, taking as long whatever bytes differ.
pub(crate) fn constant_time_eq(a: &[Value]) -> Result<Value> {
    let (x, y) = (data(a[0], "constant-time=")?, data(a[1], "constant-time=")?);
    if x.len() != y.len() {
        return Ok(Value::FALSE);
    }
    let diff = x.iter().zip(&y).fold(0u8, |d, (p, q)| d | (p ^ q));
    Ok(Value::bool(std::hint::black_box(diff) == 0))
}

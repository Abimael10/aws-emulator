// SPDX-License-Identifier: CC0-1.0

//! The COMPRESSION the `zlib` core module is served by: one deflate stream, under each of the
//! three wrappers a Node program asks for it by.
//!
//! None of it is written here. The compressor is the one its own crate states, and this module
//! only says which wrapper goes around it and bounds how many bytes one call may make. It is a
//! host function rather than guest source because that is where the implementation is — not
//! because the guest is given any new reach: a wrapper name and bytes go in, bytes come out,
//! and nothing here opens a file, a socket or an environment.
//!
//! Both directions are BOUNDED in host memory, which is outside the engine's own allocation
//! ceiling: a call is refused past [`MAX_COMPRESSED_BYTES`] on the way in and on the way out,
//! so a small input that expands without end — the shape every archive bomb has — ends as a
//! refused call rather than as this process's memory.

use std::io::{Read, Write as _};

/// What `zlib.unzip` asks for: whichever of the two wrappers the bytes themselves carry, which
/// is the one thing that call decides that the others do not.
pub(super) const AUTO_FORMAT: &str = "auto";

/// How many bytes one call may read or write.
///
/// A HOST bound on the work one `zlib` call makes this process do, in both directions: the
/// bytes are materialised in host memory, outside the engine's own allocation ceiling, so the
/// input is refused past this before anything is read and the output is refused past it rather
/// than grown — which is what bounds a stream whose decompressed size its own header never
/// states.
pub(super) const MAX_COMPRESSED_BYTES: usize = 8 * 1024 * 1024;

/// The first two bytes of a gzip member, which is how `zlib.unzip` tells the gzip wrapper from
/// the zlib one: "ID1 … ID2 … fixed values" (RFC 1952 §2.3.1).
const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];

/// The window and the buffer one brotli stream is computed with: the window the format's own
/// encoder defaults to, and a buffer well inside the ceiling above.
const BROTLI_WINDOW: u32 = 22;
const BROTLI_BUFFER_BYTES: usize = 4096;

/// The quality a brotli stream is computed at for a call that named no level: the quality the
/// format's own encoder defaults to.
const BROTLI_DEFAULT_QUALITY: u32 = 11;

/// The compression level `level` names, as the compressor's own setting.
///
/// Node states the levels zlib itself does — `Z_NO_COMPRESSION` (0) through
/// `Z_BEST_COMPRESSION` (9), with `Z_DEFAULT_COMPRESSION` (-1) asking for the library's own
/// default — so a level outside that range is clamped onto it rather than refused, exactly as
/// the library clamps it.
fn compression(level: f64) -> ::flate2::Compression {
    if !level.is_finite() || level < 0.0 {
        return ::flate2::Compression::default();
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    ::flate2::Compression::new(level.min(9.0) as u32)
}

/// The bytes `data` deflates to under `format` at `level` — hex in, hex out — or `None` for a
/// wrapper this module does not state or a call past [`MAX_COMPRESSED_BYTES`].
#[must_use]
pub(super) fn deflate(format: &str, level: f64, data: &str) -> Option<String> {
    let bytes = ::hex::decode(data).ok()?;
    if bytes.len() > MAX_COMPRESSED_BYTES {
        return None;
    }
    let setting = compression(level);
    // One arm per wrapper, because the wrapper is the encoder's own type.
    macro_rules! wrapped {
        ($encoder:path) => {{
            let mut encoder = $encoder(Vec::new(), setting);
            encoder.write_all(&bytes).ok()?;
            encoder.finish().ok()?
        }};
    }
    let out: Vec<u8> = match format {
        "gzip" => wrapped!(::flate2::write::GzEncoder::new),
        "deflate" => wrapped!(::flate2::write::ZlibEncoder::new),
        "deflate-raw" => wrapped!(::flate2::write::DeflateEncoder::new),
        "brotli" => {
            let quality = if level.is_finite() && level >= 0.0 {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                {
                    (level as u32).min(BROTLI_DEFAULT_QUALITY)
                }
            } else {
                BROTLI_DEFAULT_QUALITY
            };
            let mut held = Vec::new();
            {
                let mut encoder = ::brotli::CompressorWriter::new(
                    &mut held,
                    BROTLI_BUFFER_BYTES,
                    quality,
                    BROTLI_WINDOW,
                );
                encoder.write_all(&bytes).ok()?;
            }
            held
        }
        _ => return None,
    };
    if out.len() > MAX_COMPRESSED_BYTES {
        return None;
    }
    Some(::hex::encode(out))
}

/// The bytes `data` inflates to under `format` — hex in, hex out — or `None` for a wrapper this
/// module does not state, bytes that are not that stream, or an answer past
/// [`MAX_COMPRESSED_BYTES`].
///
/// [`AUTO_FORMAT`] reads the wrapper off the bytes themselves, which is what `zlib.unzip` does.
#[must_use]
pub(super) fn inflate(format: &str, data: &str) -> Option<String> {
    let bytes = ::hex::decode(data).ok()?;
    if bytes.len() > MAX_COMPRESSED_BYTES {
        return None;
    }
    let named = if format == AUTO_FORMAT {
        if bytes.starts_with(&GZIP_MAGIC) {
            "gzip"
        } else {
            "deflate"
        }
    } else {
        format
    };
    let held = bytes.as_slice();
    let mut reader: Box<dyn Read> = match named {
        "gzip" => Box::new(::flate2::read::GzDecoder::new(held)),
        "deflate" => Box::new(::flate2::read::ZlibDecoder::new(held)),
        "deflate-raw" => Box::new(::flate2::read::DeflateDecoder::new(held)),
        "brotli" => Box::new(::brotli::Decompressor::new(held, BROTLI_BUFFER_BYTES)),
        _ => return None,
    };
    // Read through a reader capped at one byte PAST the ceiling, so a stream that expands
    // without end is stopped by the cap rather than by this process's memory, and the call that
    // asked for it is refused.
    let mut out = Vec::new();
    reader
        .by_ref()
        .take(MAX_COMPRESSED_BYTES as u64 + 1)
        .read_to_end(&mut out)
        .ok()?;
    if out.len() > MAX_COMPRESSED_BYTES {
        return None;
    }
    Some(::hex::encode(out))
}

/// The CRC-32 of `data` — hex in, the checksum as a number out — continued from the state
/// `seed` holds, which is what a program checksumming a stream a chunk at a time hands back in.
///
/// The polynomial is not written here: it is the one its own crate states, which is the
/// checksum a gzip member carries (RFC 1952 §8).
#[must_use]
pub(super) fn crc32(data: &str, seed: u32) -> Option<u32> {
    let bytes = ::hex::decode(data).ok()?;
    if bytes.len() > MAX_COMPRESSED_BYTES {
        return None;
    }
    let mut digest = ::crc32fast::Hasher::new_with_initial(seed);
    digest.update(&bytes);
    Some(digest.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every stream this module computes, by the name the guest's `zlib` asks for it under: the
    /// three wrappers of one deflate stream, and the other compressor beside them.
    ///
    /// A name that is not one of these is answered with nothing ([`deflate`], [`inflate`]), so
    /// the guest reports the method as unsupported rather than being handed a stream of another
    /// kind; [`AUTO_FORMAT`] stands beside these on the way IN only.
    const FORMATS: &[&str] = &["gzip", "deflate", "deflate-raw", "brotli"];

    /// RULE: a wrapper's compressor and its decompressor are the two halves of one stream, so
    /// bytes handed to the first come back out of the second whatever they are.
    ///
    /// The rows differ along what the rule generalises over: WHICH wrapper, the content of the
    /// bytes (empty, text that compresses well, bytes that do not compress at all) and the
    /// level asked for (the library's default, no compression, the best).
    #[test]
    fn every_wrapper_round_trips_the_bytes_it_was_given() {
        let payloads: &[(&str, Vec<u8>)] = &[
            ("no bytes at all", Vec::new()),
            ("text that compresses", b"lambda ".repeat(512)),
            (
                "bytes that do not compress",
                (0_u16..4096).map(|byte| (byte % 251) as u8).collect(),
            ),
        ];
        for format in FORMATS {
            for (case, bytes) in payloads {
                for level in [-1.0_f64, 0.0, 9.0] {
                    let hex = ::hex::encode(bytes);
                    let compressed = deflate(format, level, &hex).unwrap_or_else(|| {
                        panic!("case {format} at {level} over {case:?}: the wrapper is stated")
                    });
                    let back = inflate(format, &compressed);
                    assert_eq!(
                        back.as_deref(),
                        Some(hex.as_str()),
                        "case {format} at {level} over {case:?}: expected the bytes back, got \
                         {back:?}"
                    );
                }
            }
        }
    }

    /// The two wrappers a stream's own bytes state are told apart by them, which is what
    /// `zlib.unzip` answers from; the raw stream states none, so it is not one of them.
    #[test]
    fn the_automatic_wrapper_is_read_off_the_stream() {
        let hex = ::hex::encode(b"lambda");
        for format in ["gzip", "deflate"] {
            let compressed =
                deflate(format, -1.0, &hex).expect("the wrapper is stated");
            let back = inflate(AUTO_FORMAT, &compressed);
            assert_eq!(
                back.as_deref(),
                Some(hex.as_str()),
                "case {format}: expected the stream's own wrapper to be read off it, got {back:?}"
            );
        }
    }

    /// A wrapper this module does not state compresses and inflates nothing, and neither does a
    /// stream the named wrapper cannot read.
    #[test]
    fn a_stream_this_module_does_not_state_is_refused() {
        for absent in ["zstd", "", "GZIP", "lz4"] {
            assert!(
                deflate(absent, -1.0, "").is_none() && inflate(absent, "").is_none(),
                "case {absent:?}: this runtime computes no such stream, so none must be answered"
            );
        }
        let gzipped = deflate("gzip", -1.0, &::hex::encode(b"lambda")).expect("gzip is stated");
        assert!(
            inflate("deflate-raw", &gzipped).is_none(),
            "a gzip member is not a raw deflate stream, so reading it as one answers nothing"
        );
    }

    /// RULE: the checksum is the one its polynomial states over the bytes it was given, so a
    /// checksum computed here is the one every other implementation computes.
    ///
    /// The rows are the values the standards publish: the checksum of the empty input, of the
    /// one-character input a CRC catalogue states, and of the catalogue's own check string.
    #[test]
    fn the_checksum_is_its_polynomials_own() {
        // Each row: the case, the bytes, and the checksum published for them.
        let cases: &[(&str, &[u8], u32)] = &[
            ("no bytes at all", b"", 0x0000_0000),
            ("one byte", b"a", 0xe8b7_be43),
            // The CRC catalogue's own check string for CRC-32/ISO-HDLC.
            ("the catalogue's check string", b"123456789", 0xcbf4_3926),
        ];
        for (case, bytes, expected) in cases {
            let actual = crc32(&::hex::encode(bytes), 0);
            assert_eq!(
                actual,
                Some(*expected),
                "case {case:?}: expected {expected:#010x}, got {actual:?}"
            );
        }
        // A checksum continued from the state a first chunk left is the checksum of the whole:
        // that is what the seed is for.
        let whole = crc32(&::hex::encode(b"123456789"), 0);
        let first = crc32(&::hex::encode(b"1234"), 0).expect("the first chunk");
        let continued = crc32(&::hex::encode(b"56789"), first);
        assert_eq!(
            continued, whole,
            "a checksum continued from the first chunk's state is the whole input's: expected \
             {whole:?}, got {continued:?}"
        );
    }

    /// Both directions are bounded: an input past the host's ceiling is refused before anything
    /// is read, and so is an answer that would grow past it.
    #[test]
    fn a_call_past_the_hosts_ceiling_is_refused() {
        let oversized = ::hex::encode(vec![0_u8; MAX_COMPRESSED_BYTES + 1]);
        assert!(
            deflate("gzip", -1.0, &oversized).is_none(),
            "an input past the ceiling is refused rather than compressed"
        );
        assert!(
            inflate("gzip", &oversized).is_none(),
            "an input past the ceiling is refused rather than read"
        );
        // A stream whose own content is larger than the ceiling: compressed on a reader that
        // has not been given the bound, so what refuses it is the bound on the way out.
        let mut encoder =
            ::flate2::write::GzEncoder::new(Vec::new(), ::flate2::Compression::default());
        for _ in 0..=(MAX_COMPRESSED_BYTES / 1024) {
            encoder
                .write_all(&[0_u8; 1024])
                .expect("the encoder takes the bytes");
        }
        let bomb = ::hex::encode(encoder.finish().expect("a finished stream"));
        assert!(
            bomb.len() / 2 <= MAX_COMPRESSED_BYTES,
            "the stream itself is inside the ceiling, so it is the ANSWER that breaches it"
        );
        assert!(
            inflate("gzip", &bomb).is_none(),
            "an answer past the ceiling is refused rather than grown"
        );
    }
}

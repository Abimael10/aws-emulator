// SPDX-License-Identifier: CC0-1.0

//! What this service derives from the BYTES of a read set it ingested, and where it
//! reports that read set's files.
//!
//! A read set formed by a direct multipart upload is reported with the metadata the
//! service read out of the content it ingested, not merely with the members the upload
//! request carried: the per-file digest, the ETag of the sequence data under the store's
//! algorithm family, the read and base counts of the records the file holds, the
//! alignment the source format implies, and the S3 location of the ingested file under
//! the store's access point. Every value below is computed from the stored content and
//! the ids the request and the store already carry — nothing here is read back from a
//! request or invented per call, so two reads of one read set report the same values.
//!
//! The derivation happens ONCE, where the read set is formed
//! (`CompleteMultipartReadSetUpload`), and the results are stored on the read set, which
//! is what every later read reports.

use super::*;

/// The part size a direct multipart read set upload is conducted with, in bytes (100
/// MiB).
///
/// It is the upload's OWN part size, not the length of any part a caller happened to
/// send: HealthOmics conducts a direct read set upload in 100 MiB parts (only the last
/// part of a source may be shorter), so every file of an uploaded read set reports this
/// as its `partSize`, whatever the parts weighed. The modelled `partSize` range
/// (1..5368709120) admits it.
pub const READ_SET_UPLOAD_PART_SIZE: i64 = 100 * 1024 * 1024;

/// Every value the modelled `ETagAlgorithm` enum admits.
/// Source: `smithy_query(query="shape",shape="ETagAlgorithm")`.
pub const E_TAG_ALGORITHMS: &[&str] = &[
    "FASTQ_MD5up",
    "BAM_MD5up",
    "CRAM_MD5up",
    "FASTQ_SHA256up",
    "BAM_SHA256up",
    "CRAM_SHA256up",
    "FASTQ_SHA512up",
    "BAM_SHA512up",
    "CRAM_SHA512up",
];

/// The modelled `FileType` of an unaligned FASTQ source.
/// Source: `smithy_query(query="shape",shape="FileType")` -> FASTQ | BAM | CRAM | UBAM.
pub const FILE_TYPE_FASTQ: &str = "FASTQ";

/// The alignment setting of a read set whose source format carries no alignment.
///
/// `CreateSequenceStore`'s own documentation names which formats those are: "For aligned
/// formats (BAM and CRAM), a sequence store can only use one reference genome. For
/// unaligned formats (FASTQ and uBAM), a reference genome is not required."
pub const ALIGNMENT_UNALIGNED: &str = "UNALIGNED";

/// The alignment setting of a read set whose source format IS an alignment of the reads
/// against a reference, from the same sentence.
pub const ALIGNMENT_ALIGNED: &str = "ALIGNED";

/// The suffix every S3 access point alias carries.
pub const S3_ACCESS_POINT_ALIAS_SUFFIX: &str = "s3alias";

/// How many characters of the digest below stand for the unique part of a store's access
/// point alias, which keeps the whole bucket name inside the modelled `S3Uri` bucket
/// pattern (`[a-z0-9][a-z0-9-.]{1,61}[a-z0-9]`).
const ALIAS_DISCRIMINATOR_WIDTH: usize = 12;

/// The MD5 digest of one ingested file's content, as the recorded `md5` of a read set
/// file reports it: the 32 lowercase hexadecimal characters a client gets by digesting
/// the bytes it uploaded.
pub fn content_md5(content: &[u8]) -> String {
    use md5::Digest;
    hex::encode(md5::Md5::digest(content))
}

/// The ETag of one ingested file's SEQUENCE DATA under the store's algorithm family, or
/// `None` when the family is not one this service computes.
///
/// The published families (`MD5up`, `SHA256up`, `SHA512up`) are computed over the
/// uncompressed sequence data rather than over the stored object, so the digest here is
/// taken from [`uncompressed_content`] and a client that hashes the records it uploaded
/// gets the same value back whether or not it compressed them. Rendered as lowercase
/// hexadecimal, the form an ETag travels in.
pub fn e_tag_digest(family: &str, sequence_data: &[u8]) -> Option<String> {
    use sha2::Digest as _;
    match family {
        "MD5up" => Some(content_md5(sequence_data)),
        "SHA256up" => Some(hex::encode(sha2::Sha256::digest(sequence_data))),
        "SHA512up" => Some(hex::encode(sha2::Sha512::digest(sequence_data))),
        _ => None,
    }
}

/// The modelled `ETagAlgorithm` a read set of this file type ingested into a store of
/// this algorithm family is reported under, or `None` when the pair names no modelled
/// value.
///
/// The algorithm is `<format>_<family>`: the format of the ingested data joined to the
/// family the store ingests under. A uBAM read set's data IS a BAM file (an unaligned
/// one), and the modelled enum names only the FASTQ, BAM and CRAM formats, so the BAM
/// token is the one that names a uBAM read set's bytes.
pub fn e_tag_algorithm(file_type: &str, family: &str) -> Option<String> {
    let algorithm = format!("{}_{family}", e_tag_format(file_type)?);
    E_TAG_ALGORITHMS
        .contains(&algorithm.as_str())
        .then_some(algorithm)
}

/// The format token of one modelled `FileType`, which is both the `ETagAlgorithm` prefix
/// above and (lowercased) the extension of the ingested file.
fn e_tag_format(file_type: &str) -> Option<&'static str> {
    match file_type {
        "FASTQ" => Some("FASTQ"),
        "BAM" | "UBAM" => Some("BAM"),
        "CRAM" => Some("CRAM"),
        _ => None,
    }
}

/// The alignment setting a read set of this file type carries, or `None` for a file type
/// this service knows no format for.
pub fn alignment(file_type: &str) -> Option<&'static str> {
    match file_type {
        "FASTQ" | "UBAM" => Some(ALIGNMENT_UNALIGNED),
        "BAM" | "CRAM" => Some(ALIGNMENT_ALIGNED),
        _ => None,
    }
}

/// Whether `content` is a gzip stream, from the two bytes every gzip member starts with
/// (RFC 1952's ID1/ID2).
pub fn is_gzip(content: &[u8]) -> bool {
    content.starts_with(&[0x1f, 0x8b])
}

/// The uncompressed sequence data of one ingested file: the content itself, or the gzip
/// stream it holds unframed.
///
/// The decoder reads at most [`MAX_READ_SET_BYTES`], the same ceiling that bounds the
/// content a store holds, so a small stream that expands without bound cannot be made to
/// fill this process. A stream that fails to decode yields the stored bytes, which is
/// what a digest of "the data as ingested" means when nothing could be unframed from it.
pub fn uncompressed_content(content: &[u8]) -> std::borrow::Cow<'_, [u8]> {
    if !is_gzip(content) {
        return std::borrow::Cow::Borrowed(content);
    }
    use std::io::Read;
    let mut data = Vec::new();
    let decoded = flate2::read::MultiGzDecoder::new(content)
        .take(MAX_READ_SET_BYTES as u64)
        .read_to_end(&mut data);
    match decoded {
        Ok(_) => std::borrow::Cow::Owned(data),
        Err(_) => std::borrow::Cow::Borrowed(content),
    }
}

/// The read and base counts of the FASTQ records `sequence_data` holds — how many reads
/// it contains and how many bases those reads contain together — or `None` when the
/// bytes are not a FASTQ file.
///
/// The records are read with the `noodles-fastq` reader, so the format's own framing
/// decides what a record is rather than anything hand-written here. Nothing is counted
/// from a file that fails to parse: the counts are the ones the ingested records hold, so
/// a partial count would be a number nothing counted.
pub fn fastq_counts(sequence_data: &[u8]) -> Option<(i64, i64)> {
    let mut reader = noodles_fastq::io::Reader::new(sequence_data);
    let mut reads: i64 = 0;
    let mut bases: i64 = 0;
    for record in reader.records() {
        let record = record.ok()?;
        reads += 1;
        bases += record.sequence().len() as i64;
    }
    (reads > 0).then_some((reads, bases))
}

/// The alias of the S3 bucket a sequence store's read set files are reachable at.
///
/// HealthOmics exposes a store's files through an S3 access point, whose alias is what a
/// client addresses as a bucket. The alias is `<store id>-<discriminator>-s3alias`, the
/// form S3 gives every access point alias; the discriminator stands for the unique part S3
/// assigns, and is taken from a digest of the store id and the account that holds it, so
/// one store always reports the same alias and no two stores share one. It is
/// environment-specific — a client reads it off the store rather than constructing it —
/// and the whole name stays inside the modelled `S3Uri` bucket pattern.
pub fn sequence_store_bucket_alias(sequence_store_id: &str) -> String {
    use sha2::Digest as _;
    let digest = hex::encode(sha2::Sha256::digest(format!(
        "{}/{sequence_store_id}",
        ::engine::emulator_account_id()
    )));
    let discriminator: String = digest.chars().take(ALIAS_DISCRIMINATOR_WIDTH).collect();
    format!("{sequence_store_id}-{discriminator}-{S3_ACCESS_POINT_ALIAS_SUFFIX}")
}

/// The `s3Uri` of one ingested read set file: the object it was ingested into, under the
/// bucket alias of the store that holds it.
///
/// Every segment but the bucket alias comes from the request and the state the read set
/// was formed from — the account that holds the store, the store's id, the read set's id,
/// the name the upload carried, which source file it is and the format's extension — so
/// the address is the one place that read set's file lives and does not move between
/// reads.
pub fn read_set_file_s3_uri(
    sequence_store_id: &str,
    read_set_id: &str,
    read_set_name: &str,
    part_source: &str,
    file_type: &str,
    content: &[u8],
) -> String {
    let alias = sequence_store_bucket_alias(sequence_store_id);
    let account_id = ::engine::emulator_account_id();
    let source = part_source.to_lowercase();
    let extension = source_file_extension(file_type, content);
    format!(
        "s3://{alias}/{account_id}/{SEQUENCE_STORE_ARN_RESOURCE_PREFIX}{sequence_store_id}\
         {READ_SET_ARN_INFIX}{read_set_id}/{read_set_name}_{source}.{extension}"
    )
}

/// The extension of the object one ingested source file was stored as: the lowercased
/// format of its data, with the gzip suffix when a FASTQ was ingested gzip-compressed.
///
/// BAM and CRAM are themselves compressed container formats, so only a FASTQ carries the
/// suffix, and it carries it only when the bytes ingested really are a gzip stream.
pub fn source_file_extension(file_type: &str, content: &[u8]) -> String {
    let format = e_tag_format(file_type)
        .map(str::to_lowercase)
        .unwrap_or_else(|| file_type.to_lowercase());
    if file_type == FILE_TYPE_FASTQ && is_gzip(content) {
        return format!("{format}.gz");
    }
    format
}

// SPDX-License-Identifier: CC0-1.0

//! What the BUILD of a revision reads: the deployment package the revision's
//! `buildConfig.codeConfig.s3Object` names, and whether that object is an archive the
//! build can open at all.
//!
//! `CodeConfig$s3Object` is "The Amazon S3 location of the deployment artifact" and
//! `RevisionState` documents the build the create starts as asynchronous — `Pending` is
//! "the revision is being built", `Active` "ready to serve traffic", `Failed` "the
//! revision build failed". So the build has an OUTCOME, and the outcome is decided by the
//! object: a deployment package is a .zip archive, and an object no zip reader can open
//! builds nothing.
//!
//! The object is read by ASKING AMAZON S3 for it, over the in-process callback into this
//! same emulator ([`smithy_server_runtime::OperationContext::callback`]) through the one
//! shared client construction ([`::service_common::s3_client`]): the read is a `GetObject`
//! signed with this emulator's own credentials and dispatched in process, so the bytes the
//! build judges are the ones `PutObject` wrote, and nothing here opens a socket or
//! resolves a host. It is made with no store guard held, because it is a call out of the
//! handler.
//!
//! Three outcomes, kept apart because they are different claims:
//!
//! * the object is an archive the build could open — the build succeeds, and the revision
//!   settles `Active` when the clock makes that transition due. The archive's own modules
//!   are read out of it here ([`DeploymentPackage`]), because they are what the endpoint's
//!   data plane RUNS once that revision is serving traffic
//!   ([`super::web_runtime`]): the package is read ONCE, where the build reads it, rather
//!   than fetched again per request;
//! * Amazon S3 ANSWERED that it will not serve the object, or it served bytes that are no
//!   zip archive — a fact about the object the revision names, so the build FAILS and the
//!   revision settles `Failed` carrying the reason;
//! * nothing answered the read at all, or nothing could be read from the answer (this
//!   emulator was assembled with no object store to ask, the body stopped part-way, or the
//!   object is longer than this emulator reads) — NOTHING about the package is established,
//!   so no failure is reported: a build is refuted by an answer about its object, never by
//!   this emulator failing to ask.

use ::std::collections::BTreeMap;
use ::std::io::Read as _;
use ::std::sync::Arc;

use ::aws_sdk_s3::error::ProvideErrorMetadata as _;

use crate::ops::types::S3Object;

/// The `stateReason` a build settles with when the object it fetched is not a zip archive.
///
/// The whole fixed sentence AWS words this failure class as — it has no variable part, and
/// it is the same sentence whatever the object held and whatever reader read it, so the
/// words are a constant here rather than the Rust reader's own diagnosis.
pub const NOT_A_ZIP_ARCHIVE_REASON: &str = "Failed to read zip file: zip: not a valid zip file";

/// How many bytes of a deployment package this emulator holds in memory to judge it.
///
/// CHOSEN, not cited: the model publishes no package ceiling for a web function. The read
/// is bounded all the same, because the object is a caller's and the judgement needs only
/// the archive's own structure. The bound is applied to the READ: an object Amazon S3
/// reports as longer is not fetched, and a body that turns out to be longer than it claimed
/// stops one byte past the bound — in both cases nothing about the package is established,
/// rather than a judgement being made on part of it.
pub const MAX_PACKAGE_BYTES: usize = 64 * 1024 * 1024;

/// [`MAX_PACKAGE_BYTES`] as the type Amazon S3 reports an object's length in.
const MAX_PACKAGE_BYTES_I64: i64 = MAX_PACKAGE_BYTES as i64;

/// How many entries of one archive are read.
///
/// CHOSEN, not cited, for the same reason as [`MAX_PACKAGE_BYTES`]: the archive is a
/// caller's, so what one package costs this process is bounded here rather than by what it
/// declares. An archive with more entries than this is still an archive the build can open
/// — the entries past the bound are simply not held, and a module that is missing surfaces
/// where it is imported.
pub const MAX_PACKAGE_ENTRIES: usize = 4_096;

/// How many bytes of entry CONTENTS one package holds inflated, across every entry.
///
/// CHOSEN for the same reason, and applied to the READ rather than to the sizes the archive
/// declares: each entry is read through a reader capped by what is left of this budget, so
/// an archive that claims a small entry and inflates a large one is stopped at the bound.
pub const MAX_UNZIPPED_BYTES: usize = 32 * 1024 * 1024;

/// The modules a deployment package holds: the entries of its archive this emulator can
/// RUN, keyed by their path inside the archive.
///
/// Only text entries are held, because what the endpoint's data plane loads from a package
/// is JavaScript and JSON ([`super::web_runtime`]): an entry whose bytes are not UTF-8 text
/// is not a module, so it is skipped rather than held. Nothing is written to a host path at
/// any point — the archive is read in memory.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DeploymentPackage {
    /// Each held entry's path inside the archive, with its source text.
    pub files: BTreeMap<String, String>,
}

impl DeploymentPackage {
    /// Whether the package holds no module at all, which is a package nothing can be
    /// started from.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

/// What following a revision's `s3Object` established about its deployment package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodePackage {
    /// The object store served the object and it is an archive the build could open,
    /// carrying the modules read out of it.
    Built(Arc<DeploymentPackage>),
    /// The build cannot read a deployment package there, and this is the reason it fails
    /// with: either Amazon S3's own answer about the object, or that the bytes it served
    /// are no zip archive.
    Unbuildable(String),
    /// Nothing established whether there is a package there at all: no object store
    /// answered, or its answer could not be read.
    Unestablished,
}

impl CodePackage {
    /// The `stateReason` the revision this verdict was taken for settles `Failed` with,
    /// and `None` for a revision whose build settles successfully.
    ///
    /// An [`Self::Unestablished`] resolution reports no failure: see the module header.
    #[must_use]
    pub fn failure_reason(&self) -> Option<String> {
        match self {
            Self::Unbuildable(reason) => Some(reason.clone()),
            Self::Built(_) | Self::Unestablished => None,
        }
    }

    /// The modules the build read out of the package, and `None` for a build that read no
    /// package at all — a revision that holds none runs nothing, and the endpoint serving
    /// it answers for that rather than inventing a response.
    #[must_use]
    pub fn package(&self) -> Option<Arc<DeploymentPackage>> {
        match self {
            Self::Built(package) => Some(Arc::clone(package)),
            Self::Unbuildable(_) | Self::Unestablished => None,
        }
    }
}

/// The sentence a build that was refused the object fails with: the call it made, and the
/// two parts AMAZON S3 itself worded — the code it classified the refusal under and the
/// message it answered.
///
/// This is Lambda's own wording for a deployment package it could not read out of Amazon
/// S3 (the sentence `services/service-com-amazonaws-lambda-awsgirapiservice` states for
/// the same read), so the two Lambda surfaces report one refusal alike.
#[must_use]
fn refusal_reason(code: &str, message: &str) -> String {
    format!("Error occurred while GetObject. S3 Error Code: {code}. S3 Error Message: {message}")
}

/// What the build of a revision whose code object is `object` can establish about its
/// deployment package, read through the emulator's own Amazon S3 in `region`.
///
/// A READ: it fetches and judges, and changes nothing in the object store.
pub async fn resolve(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    object: &S3Object,
) -> CodePackage {
    let read = ::service_common::s3_client::s3_client(ctx.callback(), region)
        .get_object()
        .bucket(&object.bucket)
        .key(&object.key)
        .set_version_id(object.version_id.clone())
        .send()
        .await;
    let read = match read {
        Ok(read) => read,
        Err(error) => {
            // What AMAZON S3 answered, when it answered at all: a hop that produced no
            // coded refusal of its own establishes nothing about the object, and is worth
            // a warning, because a deployment with no Amazon S3 mounted to ask is one
            // where no build's package is ever established. The caller's own values are
            // rendered in their escaped `Debug` form, so a bucket or key carrying a
            // newline cannot forge a log line.
            let Some((code, message)) = error.as_service_error().and_then(|service| {
                let code = service.code()?;
                Some((
                    code.to_string(),
                    service.message().unwrap_or(code).to_string(),
                ))
            }) else {
                ::tracing::warn!(
                    bucket = ?object.bucket,
                    key = ?object.key,
                    error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                    "a web function revision's code object could not be resolved against the \
                     emulator's Amazon S3"
                );
                return CodePackage::Unestablished;
            };
            return CodePackage::Unbuildable(refusal_reason(&code, &message));
        }
    };
    // The object's length as AMAZON S3 reported it, judged BEFORE a byte of it is held:
    // an object longer than this emulator reads is not fetched at all, so nothing about it
    // is established and no failure is claimed for it.
    if read
        .content_length()
        .is_some_and(|length| length > MAX_PACKAGE_BYTES_I64)
    {
        ::tracing::warn!(
            bucket = ?object.bucket,
            key = ?object.key,
            content_length = ?read.content_length(),
            "a web function revision's code object is longer than this emulator reads, so its \
             build outcome is not established"
        );
        return CodePackage::Unestablished;
    }
    // The object's bytes, read out of the in-process answer through a reader that stops ONE
    // byte past the bound — reading more than the bound is what says the object is over it,
    // whatever its answer claimed — so what this process holds for one request is bounded by
    // [`MAX_PACKAGE_BYTES`] and not by the object a caller staged. A body that stopped
    // part-way through is an answer this emulator could not read, which shows nothing about
    // the object.
    let mut bytes = Vec::new();
    let allowed = MAX_PACKAGE_BYTES as u64 + 1;
    let mut reader = ::tokio::io::AsyncReadExt::take(read.body.into_async_read(), allowed);
    if ::tokio::io::AsyncReadExt::read_to_end(&mut reader, &mut bytes)
        .await
        .is_err()
    {
        return CodePackage::Unestablished;
    }
    if bytes.len() > MAX_PACKAGE_BYTES {
        ::tracing::warn!(
            bucket = ?object.bucket,
            key = ?object.key,
            "a web function revision's code object is longer than this emulator reads, so its \
             build outcome is not established"
        );
        return CodePackage::Unestablished;
    }
    // Judging the archive INFLATES its entries, which is CPU work bounded by this module's
    // own budget rather than by anything the caller declared: it runs on a blocking thread,
    // so a pathological archive cannot hold a runtime worker while it is read. A pool that
    // could not run it establishes nothing about the package.
    match ::tokio::task::spawn_blocking(move || judge_package(&bytes)).await {
        Ok(judged) => judged,
        Err(_) => CodePackage::Unestablished,
    }
}

/// Whether `bytes` are a deployment package the build can open: an archive a zip reader
/// accepts, which is what a .zip deployment package is.
///
/// The ARCHIVE decides the build's outcome — an archive no reader can open is the one
/// failure class there is — and the modules of an archive that opens are read out of it
/// here, under the bounds above, because they are what the endpoint's data plane runs. An
/// entry that cannot be read (a corrupt stream, a compression method this reader was not
/// built with) is skipped: it says nothing about whether there is an archive, which is what
/// this build's outcome is.
#[must_use]
pub fn judge_package(bytes: &[u8]) -> CodePackage {
    judge_package_within(bytes, MAX_PACKAGE_ENTRIES, MAX_UNZIPPED_BYTES)
}

/// [`judge_package`] under the bounds `entries` and `budget` — the ONE path a package is
/// read through, so bounds small enough to spend in a test are spent exactly where a
/// package's are.
fn judge_package_within(bytes: &[u8], entries: usize, budget: usize) -> CodePackage {
    let Ok(mut archive) = ::zip::ZipArchive::new(::std::io::Cursor::new(bytes)) else {
        return CodePackage::Unbuildable(NOT_A_ZIP_ARCHIVE_REASON.to_string());
    };
    let mut package = DeploymentPackage::default();
    let mut left = budget;
    for index in 0..archive.len().min(entries) {
        // The budget bounds the DECOMPRESSION one package costs, so a package that has spent
        // it is read no further, whatever entries remain.
        if left == 0 {
            break;
        }
        let Ok(entry) = archive.by_index(index) else {
            continue;
        };
        if !entry.is_file() {
            continue;
        }
        // The path the archive itself states for the entry, taken through the zip reader's
        // own sanitising reading of it, so no `..` segment and no absolute path reaches the
        // registry the modules are resolved in.
        let Some(path) = entry
            .enclosed_name()
            .map(|path| path.to_string_lossy().replace('\\', "/"))
        else {
            continue;
        };
        // Read through a reader capped by what is LEFT of the budget, plus one byte: an
        // entry whose contents reach that cap is longer than the budget still allows, so it
        // is not held at all — a PREFIX of a module is not that module, and reading more than
        // the bound is what says the entry is over it, whatever size the archive declared.
        let mut held = Vec::new();
        let room = left;
        let allowed = u64::try_from(room).unwrap_or(u64::MAX).saturating_add(1);
        let read = entry.take(allowed).read_to_end(&mut held);
        // Every byte INFLATED is charged, held or not: what the budget bounds is the work of
        // reading the archive, so an entry that is read and then skipped — unreadable, over
        // the budget, or not text — has spent it too.
        left = left.saturating_sub(held.len());
        if read.is_err() || held.len() > room {
            continue;
        }
        let Ok(text) = String::from_utf8(held) else {
            continue;
        };
        package.files.insert(path, text);
    }
    CodePackage::Built(Arc::new(package))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::std::io::Write as _;

    /// A zip archive carrying one entry, for the rows below.
    fn archive(name: &str, contents: &[u8]) -> Vec<u8> {
        let mut writer = ::zip::ZipWriter::new(::std::io::Cursor::new(Vec::new()));
        writer
            .start_file(name, ::zip::write::SimpleFileOptions::default())
            .expect("the entry starts");
        writer.write_all(contents).expect("the entry is written");
        writer
            .finish()
            .expect("the archive is finished")
            .into_inner()
    }

    /// RULE: a build succeeds exactly when the object it fetched is an archive a zip
    /// reader can open; anything else is a build that failed, with the one fixed sentence
    /// AWS words that class as.
    #[test]
    fn only_a_readable_archive_builds() {
        let empty = archive("index.mjs", b"");
        let rows: Vec<(&str, Vec<u8>, Option<&str>)> = vec![
            ("an archive with one entry", archive("index.mjs", b"export const handler = 1;"), None),
            ("an archive with an empty entry", empty, None),
            ("a nested entry path", archive("src/app/index.mjs", b"x"), None),
            ("plain text", b"not a zip archive".to_vec(), Some(NOT_A_ZIP_ARCHIVE_REASON)),
            ("no bytes at all", Vec::new(), Some(NOT_A_ZIP_ARCHIVE_REASON)),
            (
                "a truncated archive: the local header alone",
                b"PK\x03\x04".to_vec(),
                Some(NOT_A_ZIP_ARCHIVE_REASON),
            ),
            (
                "an archive with its central directory cut off",
                {
                    let mut built = archive("index.mjs", b"x");
                    built.truncate(built.len() / 2);
                    built
                },
                Some(NOT_A_ZIP_ARCHIVE_REASON),
            ),
        ];
        for (case, bytes, expected) in rows {
            let judged = judge_package(&bytes);
            let reason = judged.failure_reason();
            assert_eq!(
                reason.as_deref(),
                expected,
                "case {case:?}: expected the failure reason {expected:?}, got {reason:?}"
            );
            assert_eq!(
                judged.package().is_some(),
                expected.is_none(),
                "case {case:?}: expected built {}, got {judged:?}",
                expected.is_none()
            );
        }
    }

    /// One row of the entry table: the case, the archive's entries, and the modules the
    /// build must hold out of it.
    type EntryRow<'a> = (&'a str, Vec<(&'a str, &'a [u8])>, Vec<(&'a str, &'a str)>);

    /// RULE: the modules of an archive the build opened are read out of it — every text
    /// entry, under its own path in the archive, nested paths included — because they are
    /// what the endpoint's data plane runs; an entry that is not text is not a module and
    /// is not held.
    ///
    /// The rows span what an archive holds: one entry, nested entries, an empty entry and
    /// an entry of bytes that are not text.
    #[test]
    fn the_modules_of_an_archive_are_read_out_of_it() {
        let rows: Vec<EntryRow<'_>> = vec![
            (
                "one entry",
                vec![("index.mjs", b"export const handler = 1;")],
                vec![("index.mjs", "export const handler = 1;")],
            ),
            (
                "a nested entry beside a root one",
                vec![
                    ("index.js", b"require('./src/app.js');"),
                    ("src/app.js", b"module.exports = 1;"),
                ],
                vec![
                    ("index.js", "require('./src/app.js');"),
                    ("src/app.js", "module.exports = 1;"),
                ],
            ),
            (
                "an empty entry",
                vec![("index.js", b"")],
                vec![("index.js", "")],
            ),
            (
                "an entry whose bytes are not text",
                vec![
                    ("index.js", b"module.exports = 1;"),
                    ("logo.png", &[0x89, b'P', b'N', b'G', 0x80, 0xfe]),
                ],
                vec![("index.js", "module.exports = 1;")],
            ),
        ];
        for (case, entries, expected) in rows {
            let judged = judge_package(&archive_of(&entries));
            let package = judged
                .package()
                .unwrap_or_else(|| panic!("case {case:?}: an archive the build opened"));
            let held: Vec<(&str, &str)> = package
                .files
                .iter()
                .map(|(path, source)| (path.as_str(), source.as_str()))
                .collect();
            assert_eq!(
                held, expected,
                "case {case:?}: expected the modules {expected:?}, got {held:?}"
            );
        }
    }

    /// An archive carrying `entries`, for the rows above.
    fn archive_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = ::zip::ZipWriter::new(::std::io::Cursor::new(Vec::new()));
        for (name, contents) in entries {
            writer
                .start_file(*name, ::zip::write::SimpleFileOptions::default())
                .expect("the entry starts");
            writer.write_all(contents).expect("the entry is written");
        }
        writer
            .finish()
            .expect("the archive is finished")
            .into_inner()
    }

    /// One row of the budget table: the case, the archive's entries, the budget, and the
    /// modules the read must hold.
    type BudgetRow<'a> = (&'a str, Vec<(&'a str, &'a [u8])>, usize, Vec<&'a str>);

    /// RULE: the inflated-bytes budget bounds the whole read of one package, and EVERY entry
    /// read spends it — whether its bytes are held as a module or skipped — so an archive
    /// cannot make this emulator inflate more than the budget however many entries it
    /// declares, and the entries past a spent budget are not read at all.
    ///
    /// Driven at a budget small enough to spend in a test, through exactly the path a
    /// package's own bounds are spent on. The rows differ in what spends the budget: a held
    /// text entry, an entry skipped because its bytes are not text, and an entry longer than
    /// the whole budget.
    #[test]
    fn every_entry_read_spends_the_packages_inflation_budget() {
        // (case, the archive's entries, the budget, the modules the read must hold)
        let rows: Vec<BudgetRow<'_>> = vec![
            (
                "entries that fit the budget",
                vec![("a.js", b"12345678"), ("b.js", b"1234")],
                16,
                vec!["a.js", "b.js"],
            ),
            (
                "an entry that spends what is left",
                vec![("a.js", b"12345678"), ("b.js", b"12345678"), ("c.js", b"1")],
                16,
                vec!["a.js", "b.js"],
            ),
            (
                "a skipped entry spends the budget too",
                vec![
                    ("logo.png", &[0x80, 0xfe, 0x80, 0xfe, 0x80, 0xfe, 0x80, 0xfe]),
                    ("a.js", b"12345678"),
                    ("b.js", b"1"),
                ],
                16,
                vec!["a.js"],
            ),
            (
                "an entry longer than the whole budget",
                vec![("big.js", b"123456789012345678901234567890"), ("a.js", b"1")],
                16,
                vec![],
            ),
            (
                "an entry longer than what is left after a held one",
                vec![("a.js", b"12345678"), ("b.js", b"123456789012345678901234567890")],
                16,
                vec!["a.js"],
            ),
            (
                "an entry exactly as long as what is left",
                vec![("a.js", b"12345678"), ("b.js", b"12345678")],
                16,
                vec!["a.js", "b.js"],
            ),
            (
                "an entry one byte longer than what is left",
                vec![("a.js", b"12345678"), ("b.js", b"123456789")],
                16,
                vec!["a.js"],
            ),
            (
                "a budget of nothing reads nothing",
                vec![("a.js", b"1")],
                0,
                vec![],
            ),
        ];
        for (case, entries, budget, expected) in rows {
            let judged = judge_package_within(&archive_of(&entries), MAX_PACKAGE_ENTRIES, budget);
            let package = judged
                .package()
                .unwrap_or_else(|| panic!("case {case:?}: an archive the build opened"));
            let held: Vec<&str> = package.files.keys().map(String::as_str).collect();
            assert_eq!(
                held, expected,
                "case {case:?}: expected the modules {expected:?} within a budget of \
                 {budget}, got {held:?}"
            );
            let inflated: usize = package.files.values().map(String::len).sum();
            assert!(
                inflated <= budget,
                "case {case:?}: expected at most {budget} bytes held, got {inflated}"
            );
            // A PREFIX of a module is not that module: every module held is the whole entry
            // the archive carried, never the part of it that fitted.
            for (path, source) in &package.files {
                let staged = entries
                    .iter()
                    .find(|(name, _)| name == path)
                    .map(|(_, contents)| *contents)
                    .unwrap_or_default();
                assert_eq!(
                    source.as_bytes(),
                    staged,
                    "case {case:?}: the module {path:?} must be held whole, got {source:?}"
                );
            }
        }
    }

    /// RULE: how many entries of one archive are read is bounded as well, so an archive of
    /// many tiny entries is read no further than the count.
    #[test]
    fn an_archive_is_read_no_further_than_the_entry_count() {
        let entries: Vec<String> = (0..8).map(|index| format!("m{index}.js")).collect();
        let staged: Vec<(&str, &[u8])> = entries
            .iter()
            .map(|name| (name.as_str(), b"1" as &[u8]))
            .collect();
        for (case, count) in [
            ("no entry at all", 0usize),
            ("one entry", 1),
            ("some of them", 3),
            ("every entry", 8),
            ("more than the archive holds", 32),
        ] {
            let judged =
                judge_package_within(&archive_of(&staged), count, MAX_UNZIPPED_BYTES);
            let package = judged
                .package()
                .unwrap_or_else(|| panic!("case {case:?}: an archive the build opened"));
            assert_eq!(
                package.files.len(),
                count.min(staged.len()),
                "case {case:?}: expected {} modules within a count of {count}, got {:?}",
                count.min(staged.len()),
                package.files.keys().collect::<Vec<_>>()
            );
        }
    }

    /// A refusal is reported as the call that was made and the two parts Amazon S3
    /// worded; a resolution that establishes nothing reports no reason at all.
    #[test]
    fn a_refused_read_reports_what_amazon_s3_answered() {
        let rows: [(&str, CodePackage, Option<&str>); 4] = [
            (
                "a key the bucket holds nothing under",
                CodePackage::Unbuildable(refusal_reason(
                    "NoSuchKey",
                    "The specified key does not exist.",
                )),
                Some(
                    "Error occurred while GetObject. S3 Error Code: NoSuchKey. S3 Error \
                     Message: The specified key does not exist.",
                ),
            ),
            (
                "a read the bucket will not serve",
                CodePackage::Unbuildable(refusal_reason("AccessDenied", "Access Denied")),
                Some(
                    "Error occurred while GetObject. S3 Error Code: AccessDenied. S3 Error \
                     Message: Access Denied",
                ),
            ),
            ("an object that is an archive", judge_package(&archive("index.js", b"")), None),
            ("nothing answered the read", CodePackage::Unestablished, None),
        ];
        for (case, package, expected) in rows {
            let reason = package.failure_reason();
            assert_eq!(
                reason.as_deref(),
                expected,
                "case {case:?}: expected {expected:?}, got {reason:?}"
            );
        }
    }
}

// SPDX-License-Identifier: CC0-1.0

//! The BUNDLE an EC2/on-premises revision is: the archive's entries, as the
//! CodeDeploy agent on an instance unpacks them.
//!
//! # What such a revision carries
//!
//! A revision of the `Server` compute platform is not one document: it is "the
//! bundled artifacts for the application revision"
//! (`smithy_query(query="doc",shape="S3Location",member="eTag")`), whose
//! `bundleType` is "the file type of the application revision … tar: A tar archive
//! file. tgz: A compressed tar archive file. zip: A zip archive file. YAML: A YAML
//! formatted file. JSON: A JSON formatted file"
//! (`smithy_query(query="doc",shape="S3Location",member="bundleType")`). Inside it
//! are the AppSpec file — which the agent reads to learn what to install and which
//! script to run for each lifecycle event ([`super::appspec`]) — and the files and
//! scripts that AppSpec names. So a deployment of that platform can only do what
//! the archive holds, and this module is the ONE place an archive is unpacked.
//!
//! # How it is unpacked
//!
//! Each archive format is read with a maintained implementation of it — `zip` for a
//! zip archive, `tar` for a tar one and `flate2` for the compression a `tgz`
//! carries — rather than with a reader written here, exactly as this crate reads
//! caller-supplied YAML with `yaml-rust2`. A `YAML` or `JSON` bundle is not an
//! archive at all: the object IS the AppSpec file, so it unpacks to that one entry.
//! An object whose `bundleType` the revision left unstated is read by the archive's
//! own leading bytes, which is what identifies the format of a file that does not
//! say.
//!
//! # Bounds
//!
//! The object is a client's, so every bound on what one revision costs this process
//! is stated here and checked as the archive is walked: how many bytes of object are
//! read at all ([`MAX_BUNDLE_BYTES`]), how many entries one bundle holds
//! ([`MAX_BUNDLE_ENTRIES`]) and how many bytes one entry carries
//! ([`MAX_ENTRY_BYTES`]). An entry past a bound is left out rather than truncated,
//! and so is an entry that is not text: the storage an instance's guest holds is
//! text ([`::service_common::posix_shell_volume`]), so a binary artifact is not
//! something this emulator can install on one.
//!
//! Nothing here opens a socket or reads a file of the host: the bytes are the ones
//! Amazon S3's own object store holds ([`super::s3_revision`] fetches them).

use std::collections::BTreeMap;
use std::io::Read;

/// The longest object this emulator unpacks a revision bundle out of.
///
/// EMULATOR-CHOSEN, like every other bound here: well above the few kilobytes an
/// AppSpec file, a page and a handful of hook scripts come to, and far below what
/// would cost this process anything noticeable. An object longer than this is not
/// read at all, so the size of one caller's object does not decide how much this
/// service allocates.
pub const MAX_BUNDLE_BYTES: usize = 1024 * 1024;

/// How many entries one unpacked bundle holds.
pub const MAX_BUNDLE_ENTRIES: usize = 64;

/// How many bytes ONE entry of it carries: the same bound the AppSpec document
/// itself is read within ([`super::appspec::max_app_spec_bytes`]), since every
/// entry is a document of the same kind — the AppSpec, a script, a page the
/// revision installs. What every entry comes to TOGETHER is held to
/// [`MAX_BUNDLE_BYTES`] as the archive is walked, so an archive that expands to
/// more than that is read up to it and no further.
pub const MAX_ENTRY_BYTES: usize = 64 * 1024;

/// The names the AppSpec file carries inside an EC2/on-premises bundle: "the
/// AppSpec file must be placed in the root directory of your revision" and it is
/// named `appspec.yml` (the AWS CodeDeploy User Guide's "Add an application
/// specification file to a revision"); the `.yaml` spelling is read as well, so a
/// revision is read for the file it HAS.
pub const APP_SPEC_NAMES: &[&str] = &["appspec.yml", "appspec.yaml"];

/// The `S3Location$bundleType` values that name an ARCHIVE
/// (`smithy_query(query="shape",shape="BundleType")`: tar | tgz | zip | YAML |
/// JSON); the other two name a bundle that IS the AppSpec document.
const BUNDLE_TYPE_ZIP: &str = "zip";
/// See [`BUNDLE_TYPE_ZIP`].
const BUNDLE_TYPE_TAR: &str = "tar";
/// See [`BUNDLE_TYPE_ZIP`].
const BUNDLE_TYPE_TGZ: &str = "tgz";

/// The leading bytes a zip archive begins with ("PK\x03\x04", the local file
/// header signature of the ZIP specification), by which an object whose revision
/// stated no `bundleType` is identified.
const ZIP_MAGIC: &[u8] = b"PK\x03\x04";

/// The leading bytes a gzip member begins with (`1f 8b`, RFC 1952's ID1/ID2), which
/// is what a `tgz` bundle carries.
const GZIP_MAGIC: &[u8] = &[0x1f, 0x8b];

/// The entries of one revision bundle, by the path each one carries inside the
/// archive.
///
/// Held on the deployment once its revision is resolved, so the archive is
/// unpacked once however many instances the deployment installs it on and however
/// many passes run over the record.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RevisionBundle {
    /// The entries, keyed by their path inside the archive — in path order, so an
    /// install walks them deterministically.
    entries: BTreeMap<String, String>,
}

impl RevisionBundle {
    /// The bundle `body` is, read as `bundle_type` says — or by the archive's own
    /// leading bytes where the revision stated none.
    ///
    /// `None` for an object this emulator unpacks nothing out of: one past
    /// [`MAX_BUNDLE_BYTES`], and one whose bytes are no archive of a format above
    /// and no text either.
    #[must_use]
    pub fn unpack(bundle_type: Option<&str>, body: &[u8]) -> Option<Self> {
        if body.len() > MAX_BUNDLE_BYTES {
            return None;
        }
        let named = bundle_type.map(str::trim).filter(|named| !named.is_empty());
        let entries = match named {
            Some(named) if named.eq_ignore_ascii_case(BUNDLE_TYPE_ZIP) => unpack_zip(body)?,
            Some(named) if named.eq_ignore_ascii_case(BUNDLE_TYPE_TGZ) => unpack_tgz(body)?,
            Some(named) if named.eq_ignore_ascii_case(BUNDLE_TYPE_TAR) => unpack_tar(body)?,
            // `YAML` and `JSON`: the object is the AppSpec file itself.
            Some(_) => one_document(body)?,
            // A revision that named no type: the format the bytes themselves
            // identify, and otherwise a document.
            None if body.starts_with(ZIP_MAGIC) => unpack_zip(body)?,
            None if body.starts_with(GZIP_MAGIC) => unpack_tgz(body)?,
            None => unpack_tar(body).or_else(|| one_document(body))?,
        };
        Some(Self { entries })
    }

    /// The bundle one AppSpec document IS: the single entry an unarchived revision
    /// carries, for a revision that keeps its AppSpec in the request itself rather
    /// than in an object.
    #[must_use]
    pub fn of_app_spec(content: &str) -> Self {
        Self {
            entries: BTreeMap::from([(APP_SPEC_NAMES[0].to_string(), content.to_string())]),
        }
    }

    /// The AppSpec document this bundle carries, or `None` for a bundle that
    /// carries none — a revision whose AppSpec file is not in the root of the
    /// archive, which is a revision CodeDeploy cannot deploy.
    #[must_use]
    pub fn app_spec(&self) -> Option<&str> {
        APP_SPEC_NAMES
            .iter()
            .find_map(|name| self.entries.get(*name))
            .map(String::as_str)
    }

    /// What this bundle holds at `path`, or `None` when it holds nothing there.
    #[must_use]
    pub fn entry(&self, path: &str) -> Option<&str> {
        self.entries.get(&entry_path(path)).map(String::as_str)
    }

    /// Every entry of this bundle, as `(path, contents)` in path order.
    pub fn entries(&self) -> impl Iterator<Item = (&str, &str)> {
        self.entries
            .iter()
            .map(|(path, contents)| (path.as_str(), contents.as_str()))
    }

    /// The entries UNDER the directory `prefix` names, as `(relative path,
    /// contents)`: what one `files` entry whose `source` is a directory copies, in
    /// path order.
    ///
    /// The whole archive is under the root, which is what a `source` of `/` names.
    pub fn entries_under(&self, prefix: &str) -> Vec<(String, String)> {
        let prefix = entry_path(prefix);
        let under = if prefix.is_empty() {
            String::new()
        } else {
            format!("{prefix}/")
        };
        self.entries
            .iter()
            .filter_map(|(path, contents)| {
                let relative = path.strip_prefix(&under)?;
                (!relative.is_empty()).then(|| (relative.to_string(), contents.clone()))
            })
            .collect()
    }

    /// Whether this bundle holds no entry at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// One path as a bundle keys it: the archive's own form, with the leading `/` or
/// `./` a document may write stripped, since an entry of an archive is a path
/// RELATIVE to the archive's root.
#[must_use]
pub fn entry_path(path: &str) -> String {
    let trimmed = path.trim();
    let mut stripped = trimmed.trim_start_matches('/');
    while let Some(rest) = stripped.strip_prefix("./") {
        stripped = rest;
    }
    stripped.trim_end_matches('/').to_string()
}

/// The entries of a zip archive, read with the `zip` crate; `None` when the bytes
/// are no zip archive.
fn unpack_zip(body: &[u8]) -> Option<BTreeMap<String, String>> {
    let mut archive = ::zip::ZipArchive::new(::std::io::Cursor::new(body)).ok()?;
    let mut entries: BTreeMap<String, String> = BTreeMap::new();
    let mut held: usize = 0;
    for index in 0..archive.len() {
        if entries.len() >= MAX_BUNDLE_ENTRIES {
            break;
        }
        let Ok(mut entry) = archive.by_index(index) else {
            continue;
        };
        if entry.is_dir() || entry.size() > MAX_ENTRY_BYTES as u64 {
            continue;
        }
        let Some(path) = entry.name().ok().map(|name| entry_path(name.as_ref())) else {
            // A member whose name is not text is not a path this emulator can put
            // on a guest's storage.
            continue;
        };
        if path.is_empty() {
            continue;
        }
        let mut contents = String::new();
        // A member that is not text is one this emulator cannot put on a guest's
        // storage, and reading it leaves whatever was decoded behind: it is left
        // out of the bundle rather than carried in half. The read is bounded again
        // here, so a header that understates what the member expands to cannot ask
        // this process for more than one entry's worth.
        if ::std::io::Read::take(&mut entry, MAX_ENTRY_BYTES as u64)
            .read_to_string(&mut contents)
            .is_ok()
        {
            held = held.saturating_add(path.len()).saturating_add(contents.len());
            if held > MAX_BUNDLE_BYTES {
                // The archive expands to more than one bundle holds, so the rest of
                // it is not read: what the whole UNPACKED bundle comes to is bounded
                // as well as what the object itself does.
                break;
            }
            entries.insert(path, contents);
        }
    }
    Some(entries)
}

/// The entries of a tar archive, read with the `tar` crate; `None` when the bytes
/// are no tar archive.
fn unpack_tar(body: &[u8]) -> Option<BTreeMap<String, String>> {
    let mut archive = ::tar::Archive::new(::std::io::Cursor::new(body));
    let mut entries: BTreeMap<String, String> = BTreeMap::new();
    let mut held: usize = 0;
    for entry in archive.entries().ok()? {
        if entries.len() >= MAX_BUNDLE_ENTRIES {
            break;
        }
        let Ok(mut entry) = entry else {
            // A header this reader could not make sense of: the archive is not one
            // this emulator unpacks, rather than one with a missing member.
            return None;
        };
        if !entry.header().entry_type().is_file() || entry.size() > MAX_ENTRY_BYTES as u64 {
            continue;
        }
        let Ok(path) = entry.path() else {
            continue;
        };
        let path = entry_path(&path.to_string_lossy());
        if path.is_empty() {
            continue;
        }
        let mut contents = String::new();
        // Bounded again as the member is read, for the reason [`unpack_zip`] states.
        if ::std::io::Read::take(&mut entry, MAX_ENTRY_BYTES as u64)
            .read_to_string(&mut contents)
            .is_ok()
        {
            held = held.saturating_add(path.len()).saturating_add(contents.len());
            if held > MAX_BUNDLE_BYTES {
                break;
            }
            entries.insert(path, contents);
        }
    }
    (!entries.is_empty()).then_some(entries)
}

/// The entries of a compressed tar archive: the `tgz` form, decompressed with
/// `flate2` and then read as a tar archive.
///
/// The decompressed stream is read within [`MAX_BUNDLE_BYTES`] rather than by
/// however much the member expands to, so a small object cannot ask this process
/// for an unbounded one.
fn unpack_tgz(body: &[u8]) -> Option<BTreeMap<String, String>> {
    let mut decoder = ::flate2::read::GzDecoder::new(::std::io::Cursor::new(body));
    let mut decompressed: Vec<u8> = Vec::new();
    ::std::io::Read::take(&mut decoder, MAX_BUNDLE_BYTES as u64)
        .read_to_end(&mut decompressed)
        .ok()?;
    unpack_tar(&decompressed)
}

/// The one entry a `YAML` or `JSON` bundle holds: the object's own bytes as the
/// AppSpec file. `None` for bytes that are no text, or more of them than one entry
/// carries.
fn one_document(body: &[u8]) -> Option<BTreeMap<String, String>> {
    if body.len() > MAX_ENTRY_BYTES {
        return None;
    }
    let content = ::std::str::from_utf8(body).ok()?;
    Some(BTreeMap::from([(
        APP_SPEC_NAMES[0].to_string(),
        content.to_string(),
    )]))
}

#[cfg(test)]
mod tests {
    //! What [`RevisionBundle::unpack`] reads out of an object, over the bundle
    //! forms `S3Location$bundleType` names — cheap here, where a row costs an
    //! archive in memory rather than a bucket, an object and a deployment.

    use super::*;

    /// A zip archive of `entries`, written the way a client writes a revision
    /// bundle.
    fn zipped(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut writer = ::zip::ZipWriter::new(::std::io::Cursor::new(Vec::new()));
        let options: ::zip::write::FileOptions<'_, '_, ()> =
            ::zip::write::FileOptions::default();
        for (path, contents) in entries {
            writer
                .start_file(*path, options)
                .expect("the entry is well formed");
            ::std::io::Write::write_all(&mut writer, contents.as_bytes())
                .expect("the entry is written");
        }
        writer
            .finish()
            .expect("the archive is finished")
            .into_inner()
    }

    /// A tar archive of `entries`, optionally gzip-compressed.
    fn tarred(entries: &[(&str, &str)], compress: bool) -> Vec<u8> {
        let mut builder = ::tar::Builder::new(Vec::new());
        for (path, contents) in entries {
            let mut header = ::tar::Header::new_gnu();
            header.set_size(contents.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, *path, contents.as_bytes())
                .expect("the entry is appended");
        }
        let archive = builder.into_inner().expect("the archive is finished");
        if !compress {
            return archive;
        }
        let mut encoder =
            ::flate2::write::GzEncoder::new(Vec::new(), ::flate2::Compression::default());
        ::std::io::Write::write_all(&mut encoder, &archive).expect("the archive is compressed");
        encoder.finish().expect("the archive is compressed")
    }

    /// RULE: a bundle is unpacked by the archive format it is in — stated or
    /// identified by its own leading bytes — and its AppSpec file is the entry in
    /// the archive's root.
    #[test]
    fn a_bundle_is_unpacked_by_its_format() {
        let document = "version: 0.0\nos: linux\n";
        let script = "#!/bin/bash\necho started\n";
        let entries: &[(&str, &str)] = &[
            (APP_SPEC_NAMES[0], document),
            ("scripts/start.sh", script),
        ];
        // (the case, the stated bundle type, the object's bytes)
        let cases: Vec<(&str, Option<&str>, Vec<u8>)> = vec![
            ("a zip bundle", Some("zip"), zipped(entries)),
            ("a zip bundle stating no type", None, zipped(entries)),
            ("a tar bundle", Some("tar"), tarred(entries, false)),
            ("a tar bundle stating no type", None, tarred(entries, false)),
            ("a tgz bundle", Some("tgz"), tarred(entries, true)),
            ("a tgz bundle stating no type", None, tarred(entries, true)),
        ];
        for (case, bundle_type, body) in &cases {
            let bundle = RevisionBundle::unpack(*bundle_type, body)
                .unwrap_or_else(|| panic!("case {case:?}: the bundle must unpack"));
            assert_eq!(
                bundle.app_spec(),
                Some(document),
                "case {case:?}: expected the AppSpec document, got {:?}",
                bundle.app_spec()
            );
            assert_eq!(
                bundle.entry("scripts/start.sh"),
                Some(script),
                "case {case:?}: expected the script entry, got {:?}",
                bundle.entry("scripts/start.sh")
            );
        }
        // A `YAML`/`JSON` bundle is the AppSpec document itself, and a bundle whose
        // root holds no AppSpec file carries none.
        let document_bundle = RevisionBundle::unpack(Some("YAML"), document.as_bytes())
            .expect("case \"a YAML bundle\": the bundle must unpack");
        assert_eq!(
            document_bundle.app_spec(),
            Some(document),
            "case \"a YAML bundle\": expected the object itself as the AppSpec"
        );
        let nested = zipped(&[("revision/appspec.yml", document)]);
        let nested = RevisionBundle::unpack(Some("zip"), &nested)
            .expect("case \"an AppSpec outside the root\": the bundle must unpack");
        assert!(
            nested.app_spec().is_none(),
            "case \"an AppSpec outside the root\": expected no AppSpec, got {:?}",
            nested.app_spec()
        );
    }

    /// RULE: every bound on what one bundle costs is this emulator's — the object
    /// it reads at all, the entries it holds and the bytes one entry carries — and
    /// an entry past one is left out rather than carried in part.
    #[test]
    fn a_bundle_is_unpacked_within_its_bounds() {
        let past_object = vec![b'a'; MAX_BUNDLE_BYTES + 1];
        assert!(
            RevisionBundle::unpack(Some("YAML"), &past_object).is_none(),
            "case \"an object past the bundle bound\": expected nothing unpacked"
        );
        let entries: Vec<(String, String)> = (0..MAX_BUNDLE_ENTRIES + 4)
            .map(|index| (format!("file-{index:03}"), "contents\n".to_string()))
            .collect();
        let rows: Vec<(&str, &str)> = entries
            .iter()
            .map(|(path, contents)| (path.as_str(), contents.as_str()))
            .collect();
        let bundle = RevisionBundle::unpack(Some("zip"), &zipped(&rows))
            .expect("case \"more entries than one bundle holds\": the bundle must unpack");
        assert_eq!(
            bundle.entries().count(),
            MAX_BUNDLE_ENTRIES,
            "case \"more entries than one bundle holds\": expected the bound \
             {MAX_BUNDLE_ENTRIES}, got {}",
            bundle.entries().count()
        );
        let long = "x".repeat(MAX_ENTRY_BYTES + 1);
        let bundle = RevisionBundle::unpack(
            Some("zip"),
            &zipped(&[(APP_SPEC_NAMES[0], "version: 0.0\n"), ("big", &long)]),
        )
        .expect("case \"an entry past the entry bound\": the bundle must unpack");
        assert!(
            bundle.entry("big").is_none(),
            "case \"an entry past the entry bound\": expected the entry left out"
        );
        assert_eq!(
            bundle.app_spec(),
            Some("version: 0.0\n"),
            "case \"an entry past the entry bound\": expected the other entries kept"
        );
    }

    /// RULE: a `source` that names a directory copies the entries UNDER it, and
    /// the root names every entry of the archive.
    #[test]
    fn the_entries_under_a_source_are_the_ones_it_copies() {
        let bundle = RevisionBundle::unpack(
            Some("zip"),
            &zipped(&[
                (APP_SPEC_NAMES[0], "version: 0.0\n"),
                ("site/index.html", "<p>one</p>"),
                ("site/css/app.css", "body{}"),
            ]),
        )
        .expect("the bundle must unpack");
        // (the source the AppSpec names, the relative paths it copies)
        let cases: &[(&str, &[&str])] = &[
            ("site", &["css/app.css", "index.html"]),
            ("site/", &["css/app.css", "index.html"]),
            ("/", &["appspec.yml", "site/css/app.css", "site/index.html"]),
            ("", &["appspec.yml", "site/css/app.css", "site/index.html"]),
            ("absent", &[]),
        ];
        for (source, expected) in cases {
            let under: Vec<String> = bundle
                .entries_under(source)
                .into_iter()
                .map(|(path, _)| path)
                .collect();
            assert_eq!(
                under, *expected,
                "case {source:?}: expected {expected:?}, got {under:?}"
            );
        }
    }
}

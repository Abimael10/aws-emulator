// SPDX-License-Identifier: CC0-1.0

//! The SOURCE of the standard library a `python3.x` execution environment's runtime ships.
//!
//! A managed runtime keeps its standard library as FILES under its own library directory, so a
//! traceback frame standing inside one of those modules is rendered the same way a frame of
//! the deployment package is: the location line and the source line at that location. The
//! interpreter this crate embeds imports that same library from a frozen copy — compiled code
//! objects carrying no text — so a frame inside it would have no line to show.
//!
//! This module is the text those code objects were compiled from, taken into the binary when
//! this crate is built (`build.rs`) out of the very library the frozen copy is built from: the
//! line reported at a position is the line the running code really was compiled from, no host
//! path is opened while a function runs, and nothing about a frame is written out by hand.
//!
//! It is keyed the way a frame names its module — [`library_source`] — because the unit a
//! frozen module is compiled under is its module name rather than a path
//! ([`super::bootstrap`]'s `_frame_file`, which renders that name as the module's file under
//! the library directory).

include!(concat!(env!("OUT_DIR"), "/python_library_sources.rs"));

/// What a module's own source file is named by, inside the library and in the frame that names
/// it.
const MODULE_SUFFIX: &str = ".py";

/// The file a PACKAGE's own code is held in, which is what a frame naming the package itself
/// is standing in.
const PACKAGE_MODULE: &str = "__init__.py";

/// The source of the standard-library module `named`, or `None` for a name the library this
/// runtime ships holds no module under.
///
/// `named` is the unit a frame states it is in, which for a module of the frozen library is
/// that module's dotted NAME (`os`, `json.decoder`) and not a path: each component of the name
/// is a directory of the library and the last one its own file, which is how an import
/// resolved it in the first place. A name that is already written as a path is read as one, so
/// one reading serves both.
#[must_use]
pub fn library_source(named: &str) -> Option<&'static str> {
    let stem = named.strip_suffix(MODULE_SUFFIX).unwrap_or(named);
    if stem.is_empty() {
        return None;
    }
    let path = stem.replace('.', "/");
    held(&format!("{path}{MODULE_SUFFIX}")).or_else(|| held(&format!("{path}/{PACKAGE_MODULE}")))
}

/// The source held under one path inside the library, read off the sorted table the build
/// wrote.
fn held(path: &str) -> Option<&'static str> {
    LIBRARY_SOURCES
        .binary_search_by_key(&path, |(held, _source)| *held)
        .ok()
        .map(|at| LIBRARY_SOURCES[at].1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The library this runtime ships is HELD here: every entry is a module file, the table is
    /// ordered (which is what the lookup above reads it by), and each held path resolves back
    /// to its own source through the name a frame would state for it.
    #[test]
    fn every_held_entry_is_a_module_of_the_library_and_resolves_back() {
        assert!(
            !LIBRARY_SOURCES.is_empty(),
            "expected the standard library the interpreter ships, got no modules at all"
        );
        let mut previous = "";
        for (path, source) in LIBRARY_SOURCES {
            assert!(
                path.ends_with(MODULE_SUFFIX),
                "case {path:?}: every held entry is a module file, got {path:?}"
            );
            assert!(
                previous < *path,
                "case {path:?}: the table is read by binary search, so it must be ordered; \
                 {previous:?} is not before {path:?}"
            );
            previous = path;
            // The name a frame of this module states: the path with its separators as the
            // dots of a module name, and a package named by the directory its own file is in.
            let named = path
                .strip_suffix(&format!("/{PACKAGE_MODULE}"))
                .map(str::to_string)
                .unwrap_or_else(|| {
                    path.strip_suffix(MODULE_SUFFIX).unwrap_or(path).to_string()
                })
                .replace('/', ".");
            let resolved = library_source(&named);
            assert_eq!(
                resolved,
                Some(*source),
                "case {named:?}: expected the source held at {path:?}, got {:?}",
                resolved.map(str::len)
            );
        }
    }

    /// A name the library holds no module under resolves to no source, so a frame of something
    /// else is never given a line out of this table.
    #[test]
    fn a_name_the_library_holds_nothing_under_resolves_to_no_source() {
        // Source of the names: the two mounts a frame can otherwise name
        // (`super::bootstrap`'s task root and runtime root), a module no standard library
        // ships, and the pseudo-files an interpreter renders for code it was handed as text.
        let unknown: &[&str] = &[
            "",
            ".py",
            "index",
            "not_a_standard_library_module",
            "<string>",
            "/var/task/index.py",
        ];
        for named in unknown {
            let resolved = library_source(named);
            assert_eq!(
                resolved.map(str::len),
                None,
                "case {named:?}: expected no source, got {:?}",
                resolved.map(str::len)
            );
        }
    }

    /// The modules a handler reaches most of — and the ones a traceback passes through — are
    /// among them, each as a module and as a package.
    ///
    /// Source of the names: the standard-library modules this runtime offers
    /// (`super::host_posix`'s test catalog), named here as a frame of one would state it.
    #[test]
    fn the_modules_the_runtime_offers_carry_their_source() {
        // (the name a frame states, a fragment of that module's own source)
        let cases: &[(&str, &str)] = &[
            ("os", "class _Environ("),
            ("posixpath", "def join("),
            ("json.decoder", "class JSONDecoder("),
            ("json", "def dumps("),
            ("logging", "class Logger("),
            ("traceback", "def format_exception("),
        ];
        for (named, fragment) in cases {
            let resolved = library_source(named);
            let source = resolved.unwrap_or_default();
            assert!(
                source.contains(fragment),
                "case {named:?}: expected the module's own source, carrying {fragment:?}, got \
                 {} characters",
                source.len()
            );
        }
    }
}

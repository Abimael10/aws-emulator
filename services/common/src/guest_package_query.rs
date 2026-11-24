// SPDX-License-Identifier: CC0-1.0

//! The PACKAGE QUERY a guest's package database is read with: `rpm`, as the invocation states
//! it and as the answer is rendered.
//!
//! # Why it is shared
//!
//! Which packages a machine has installed is not one component's fact: the service that owns the
//! machine knows what its own programs installed in it, and the service that keeps a node's
//! inventory knows the build of the agent it reports for that node. Both of them install the
//! query program into a guest's session, so both read the SAME invocation grammar and render the
//! SAME answer — that is what this module is: the option grammar `rpm` takes a QUERY in, the
//! glob a query names a package by, and the query FORMAT it renders a package under. What each
//! consumer keeps is only the packages it holds a build of.
//!
//! # What is real here
//!
//! Nothing here reads a package database of the host, starts a process or invents a build: a
//! package is rendered out of the facts its holder passed in, a tag the package carries no value
//! for renders as nothing (the way `rpm` renders one), and an invocation written in a form this
//! grammar does not read is answered by nothing at all — which the session reports as a command
//! it could not run.

/// The program a guest of this platform queries its package database with.
pub const QUERY_PROGRAM: &str = "rpm";

/// The format `rpm` renders a queried package with where the invocation named none:
/// `name-version-release.architecture`, the form `rpm -q` writes one line of per package.
pub const DEFAULT_QUERY_FORMAT: &str = "%{NAME}-%{VERSION}-%{RELEASE}.%{ARCH}";

/// The architecture `rpm` names the one Amazon EC2 reports as `arm64`: the same machines, under
/// the name the package tooling of these distributions uses.
pub const RPM_ARM64: &str = "aarch64";

/// The architecture name Amazon EC2 reports for those machines (`Image$Architecture`).
pub const EC2_ARM64: &str = "arm64";

/// What a package built for no particular machine reports as its architecture, which is what a
/// package of a machine whose own architecture this emulator does not hold is reported as.
pub const ARCHITECTURE_INDEPENDENT: &str = "noarch";

/// How many bytes ONE invocation writes, however long a format the caller wrote: the format is
/// a caller's text, so what one command can make this process build is bounded here.
const MAX_OUTPUT_BYTES: usize = 4 * 1024;

/// The architecture `rpm` names the machine Amazon EC2 reports `architecture` for, and
/// [`ARCHITECTURE_INDEPENDENT`] for a machine whose architecture its holder does not know.
#[must_use]
pub fn rpm_architecture(architecture: &str) -> String {
    match architecture.trim() {
        "" => ARCHITECTURE_INDEPENDENT.to_string(),
        EC2_ARM64 => RPM_ARM64.to_string(),
        held => held.to_string(),
    }
}

/// ONE package a guest's database holds, as a query of it reports one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueriedPackage {
    /// The package's name.
    pub name: String,
    /// The version installed.
    pub version: String,
    /// The package build of that version, which `rpm` reports as its `RELEASE`.
    pub release: String,
    /// The architecture the package was built for, as the package tooling names one
    /// ([`rpm_architecture`]).
    pub architecture: String,
}

/// One invocation of the query program, as its words state it.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct PackageQueryInvocation {
    /// The packages it names, each as the caller wrote it — a name or a glob.
    pub patterns: Vec<String>,
    /// Whether it queries EVERY installed package (`-a`, `--all`), which names no package of
    /// its own.
    pub all: bool,
    /// The format it renders each package with (`--queryformat`, `--qf`), where it named one.
    pub format: Option<String>,
}

impl PackageQueryInvocation {
    /// The invocation `arguments` state, or `None` for one written in a form this grammar does
    /// not read — anything that is not a QUERY, and any option it performs nothing of.
    #[must_use]
    pub fn read(arguments: &[String]) -> Option<Self> {
        let mut invocation = Self::default();
        let mut query = false;
        let mut words = arguments.iter().map(String::as_str);
        while let Some(word) = words.next() {
            if let Some(long) = word.strip_prefix("--") {
                let (option, inline) = match long.split_once('=') {
                    Some((option, value)) => (option, Some(value)),
                    None => (long, None),
                };
                match option {
                    "query" => query = true,
                    "all" => invocation.all = true,
                    "queryformat" | "qf" => {
                        let value = match inline {
                            Some(value) => value,
                            // The format is the word after the option, which an invocation
                            // that ends on the option never wrote.
                            None => words.next()?,
                        };
                        invocation.format = Some(value.to_string());
                    }
                    // Every other option is one this grammar performs nothing of.
                    _ => return None,
                }
                continue;
            }
            if let Some(short) = word.strip_prefix('-').filter(|short| !short.is_empty()) {
                for letter in short.chars() {
                    match letter {
                        'q' => query = true,
                        'a' => invocation.all = true,
                        _ => return None,
                    }
                }
                continue;
            }
            invocation.patterns.push(word.to_string());
        }
        // Only a QUERY is read here, and a query has to name what it is about: rpm reports an
        // invocation naming neither a package nor every package as its own failure, which is
        // not a form this grammar reads.
        (query && (invocation.all || !invocation.patterns.is_empty())).then_some(invocation)
    }

    /// Whether this invocation asks about the package `name` names: every installed package,
    /// or a name or glob that package's name satisfies.
    #[must_use]
    pub fn names(&self, name: &str) -> bool {
        self.all && self.patterns.is_empty()
            || self
                .patterns
                .iter()
                .any(|pattern| matches_pattern(pattern, name))
    }

    /// Whether every package this invocation NAMES is one of `held` — what a holder of PART of
    /// a machine's database asks before it answers at all, so a query about a package it does
    /// not hold is left to whoever holds the rest.
    #[must_use]
    pub fn names_only(&self, held: &[QueriedPackage]) -> bool {
        !self.all
            && !self.patterns.is_empty()
            && self.patterns.iter().all(|pattern| {
                held.iter()
                    .any(|package| matches_pattern(pattern, &package.name))
            })
    }

    /// The format this invocation renders each package under: the one it named, else
    /// [`DEFAULT_QUERY_FORMAT`].
    #[must_use]
    pub fn format(&self) -> &str {
        self.format.as_deref().unwrap_or(DEFAULT_QUERY_FORMAT)
    }
}

/// The lines one queried package renders to under `format`: the format's own text with every
/// tag it names replaced by that package's value for it, split where the format breaks a line.
#[must_use]
pub fn rendered_package(package: &QueriedPackage, format: &str) -> Vec<String> {
    let mut rendered = String::new();
    let mut rest = format;
    while let Some(start) = rest.find("%{") {
        if rendered.len() + start > MAX_OUTPUT_BYTES {
            break;
        }
        rendered.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            // A tag the format never closes is text, which is what rpm writes back for a
            // format it cannot read as a tag.
            rendered.push_str(&rest[start..]);
            rest = "";
            break;
        };
        // A tag this package has no value for renders as nothing, the way rpm renders a tag the
        // package does not carry.
        if let Some(value) = tag(package, &after[..end]) {
            rendered.push_str(&value);
        }
        rest = &after[end + 1..];
    }
    rendered.push_str(rest);
    if rendered.len() > MAX_OUTPUT_BYTES {
        let mut at = MAX_OUTPUT_BYTES;
        while at > 0 && !rendered.is_char_boundary(at) {
            at -= 1;
        }
        rendered.truncate(at);
    }
    // The format's own escapes are what break its output into lines: rpm reads `\n` and `\t` in
    // a query format as the characters they name.
    let mut lines: Vec<String> = rendered
        .replace("\\t", "\t")
        .split("\\n")
        .map(str::to_string)
        .collect();
    // A format ending in a line break describes no line after the break.
    if lines.len() > 1 && lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines
}

/// That package's value for the query tag `named` names, or `None` for a tag it carries no
/// value for.
///
/// The names are the ones `rpm` documents for its query format, read case-insensitively as rpm
/// reads them.
fn tag(package: &QueriedPackage, named: &str) -> Option<String> {
    let name = named.trim().to_ascii_uppercase();
    let evr = || format!("{}-{}", package.version, package.release);
    let nvr = || format!("{}-{}", package.name, evr());
    match name.as_str() {
        "NAME" => Some(package.name.clone()),
        "VERSION" => Some(package.version.clone()),
        "RELEASE" => Some(package.release.clone()),
        "ARCH" | "ARCHITECTURE" => Some(package.architecture.clone()),
        "EVR" => Some(evr()),
        "NVR" => Some(nvr()),
        "NVRA" => Some(format!("{}.{}", nvr(), package.architecture)),
        _ => None,
    }
}

/// Whether the package name `name` satisfies the glob `pattern` a query named it by: `*` stands
/// for any run of characters, as a package glob's does, and every other character stands for
/// itself.
fn matches_pattern(pattern: &str, name: &str) -> bool {
    let mut parts = pattern.trim().split('*');
    let Some(first) = parts.next() else {
        return false;
    };
    let Some(mut rest) = name.strip_prefix(first) else {
        return false;
    };
    let mut last: Option<&str> = None;
    for part in parts {
        if let Some(previous) = last.replace(part) {
            // A part between two wildcards may sit anywhere after what matched before it.
            match rest.find(previous) {
                Some(at) => rest = &rest[at + previous.len()..],
                None => return false,
            }
        }
    }
    match last {
        // The pattern ended with a wildcard's trailing text, which the name has to end with.
        Some(tail) => rest.len() >= tail.len() && rest.ends_with(tail),
        // The pattern carried no wildcard at all, so it is the whole name.
        None => rest.is_empty(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The words of an invocation, as a session hands them over.
    fn words(written: &[&str]) -> Vec<String> {
        written.iter().map(|word| (*word).to_string()).collect()
    }

    /// One package to render.
    fn package() -> QueriedPackage {
        QueriedPackage {
            name: "amazon-ssm-agent".to_string(),
            version: "3.3.1142.0".to_string(),
            release: "1".to_string(),
            architecture: "x86_64".to_string(),
        }
    }

    /// RULE: a query names a package by a name or a glob, and every other character of the
    /// glob stands for itself.
    #[test]
    fn a_query_names_a_package_by_a_name_or_a_glob() {
        // (the pattern, whether it names the package)
        let rows: &[(&str, bool)] = &[
            ("amazon-ssm-agent", true),
            ("amazon-ssm-*", true),
            ("*-agent", true),
            ("amazon-*-agent", true),
            ("*ssm*", true),
            ("amazon-cloudwatch-agent", false),
            ("amazon-ssm-agent-extra", false),
            ("*-cloudwatch", false),
            ("", false),
        ];
        for (pattern, expected) in rows {
            let named = matches_pattern(pattern, &package().name);
            assert!(
                named == *expected,
                "case {pattern:?}: expected {expected}, got {named}"
            );
        }
    }

    /// RULE: only a QUERY is read, and only in the option forms rpm takes one in.
    #[test]
    fn only_a_query_is_read() {
        // (the case, the words, whether the grammar reads them)
        let rows: &[(&str, &[&str], bool)] = &[
            ("a query naming a package", &["-q", "amazon-ssm-agent"], true),
            ("the long spelling", &["--query", "amazon-ssm-agent"], true),
            ("every installed package", &["-qa"], true),
            ("a format as the next word", &["-q", "--qf", "%{NAME}", "amazon-ssm-agent"], true),
            ("a format joined to the option", &["-q", "--queryformat=%{VERSION}", "amazon-ssm-agent"], true),
            ("a query naming nothing at all", &["-q"], false),
            ("a format with nothing after it", &["-q", "--qf"], false),
            ("an install rather than a query", &["-i", "amazon-ssm-agent"], false),
            ("an option this grammar has none of", &["-q", "--provides", "amazon-ssm-agent"], false),
            ("no words at all", &[], false),
        ];
        for (case, written, expected) in rows {
            let read = PackageQueryInvocation::read(&words(written)).is_some();
            assert!(
                read == *expected,
                "case {case:?}: expected read={expected}, got {read}"
            );
        }
    }

    /// RULE: a holder of part of a machine's database answers only a query whose every named
    /// package it holds.
    #[test]
    fn a_partial_database_answers_only_what_it_holds() {
        let held = vec![package()];
        // (the case, the words, whether a partial holder answers)
        let rows: &[(&str, &[&str], bool)] = &[
            ("the package it holds", &["-q", "amazon-ssm-agent"], true),
            ("a glob the package satisfies", &["-q", "amazon-ssm-*"], true),
            ("a package it does not hold", &["-q", "amazon-cloudwatch-agent"], false),
            (
                "one it holds beside one it does not",
                &["-q", "amazon-ssm-agent", "amazon-cloudwatch-agent"],
                false,
            ),
            ("every installed package", &["-qa"], false),
        ];
        for (case, written, expected) in rows {
            let invocation = PackageQueryInvocation::read(&words(written))
                .unwrap_or_else(|| panic!("case {case:?}: the grammar reads these words"));
            let answers = invocation.names_only(&held);
            assert!(
                answers == *expected,
                "case {case:?}: expected answers={expected}, got {answers}"
            );
        }
    }

    /// RULE: a package renders under the format the invocation named, with each tag replaced by
    /// that package's value for it — and a tag it carries no value for renders as nothing.
    #[test]
    fn a_package_renders_under_the_format_it_was_queried_with() {
        // (the format, the lines it renders to)
        let rows: &[(&str, &[&str])] = &[
            (DEFAULT_QUERY_FORMAT, &["amazon-ssm-agent-3.3.1142.0-1.x86_64"]),
            ("%{NAME}", &["amazon-ssm-agent"]),
            ("%{VERSION}", &["3.3.1142.0"]),
            ("%{EVR}", &["3.3.1142.0-1"]),
            ("%{NVRA}", &["amazon-ssm-agent-3.3.1142.0-1.x86_64"]),
            ("%{NAME} %{ARCH}", &["amazon-ssm-agent x86_64"]),
            // A tag the package carries no value for renders as nothing.
            ("[%{SUMMARY}]", &["[]"]),
            // The format's own escapes break its output into lines.
            ("%{NAME}\\n%{VERSION}\\n", &["amazon-ssm-agent", "3.3.1142.0"]),
            // A tag the format never closes is text.
            ("%{NAME", &["%{NAME"]),
        ];
        for (format, expected) in rows {
            let rendered = rendered_package(&package(), format);
            assert_eq!(
                rendered, *expected,
                "case {format:?}: expected {expected:?}, got {rendered:?}"
            );
        }
    }

    /// RULE: the architecture a package reports is the package tooling's name for the machine's.
    #[test]
    fn the_architecture_is_the_package_toolings_name_for_it() {
        // (what Amazon EC2 reports, what the package tooling calls it)
        let rows: &[(&str, &str)] = &[
            ("x86_64", "x86_64"),
            (EC2_ARM64, RPM_ARM64),
            ("i386", "i386"),
            ("", ARCHITECTURE_INDEPENDENT),
            ("  ", ARCHITECTURE_INDEPENDENT),
        ];
        for (reported, expected) in rows {
            let named = rpm_architecture(reported);
            assert_eq!(
                named, *expected,
                "case {reported:?}: expected {expected:?}, got {named:?}"
            );
        }
    }
}

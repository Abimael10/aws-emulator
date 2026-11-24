// SPDX-License-Identifier: CC0-1.0

//! The configuration a source bundle CARRIES: the `.ebextensions/*.config` files
//! a deployment of that bundle applies to the instance before the application is
//! started.
//!
//! `CreateApplicationVersion$Process` names these files ("the configuration files
//! (*.config files in the .ebextensions folder) in the source bundle"), and
//! Elastic Beanstalk's own documentation states what a deployment DOES with each
//! section of one:
//!
//! * `option_settings:` is merged into the environment's configuration at deploy
//!   time, so an option set only in the bundle applies to the environment — while
//!   a value set for the same option through the API wins over it (Elastic
//!   Beanstalk's documented precedence: settings applied directly to the
//!   environment take precedence over the ones a `.ebextensions` configuration
//!   file sets);
//! * `files:` entries are WRITTEN to the paths they declare, with the content they
//!   declare;
//! * `commands:` run on the instance before the application is set up, and
//!   `container_commands:` run after the application has been extracted and
//!   before the web process is started — so the side effects of both exist on the
//!   instance by the time the application runs.
//!
//! What this module owns is only READING those files: which settings, files and
//! commands one deployment carries. APPLYING them is the deployment's own work,
//! performed on the instance where the application runs
//! ([`super::deployed_application`], [`super::python_program`]).
//!
//! Two forms of each section are read, because a configuration file may be
//! written either way: a MAPPING keyed by what the entry is about (the namespace
//! of a setting, the path of a file, the name of a command — which is also what
//! orders the commands, since Elastic Beanstalk runs container commands in the
//! alphabetical order of their keys) and a SEQUENCE of entries that name the same
//! thing in members. Everything a form does not state is absent rather than
//! invented.
//!
//! The bound on reading them is the one this crate already applies to a caller's
//! YAML ([`super::read_bounded_yaml`]) plus the bounds on the text each entry may
//! carry here: these documents come out of a caller's archive.

use super::*;

/// How many files one deployment's configuration may declare, how many commands
/// it may run, and how much text one declared file may carry.
///
/// Bounds on CALLER content: the declarations are held in this process for as long
/// as the version exists and are applied inside the instance this emulator runs
/// the application on, so each is capped rather than trusted. Generous for the
/// configuration files a deployment really carries.
pub const MAX_PROVISIONED_FILES: usize = 32;
/// See [`MAX_PROVISIONED_FILES`].
pub const MAX_PROVISIONED_COMMANDS: usize = 32;
/// See [`MAX_PROVISIONED_FILES`].
pub const MAX_PROVISIONED_FILE_BYTES: usize = 64 * 1024;
/// See [`MAX_PROVISIONED_FILES`].
pub const MAX_COMMAND_BYTES: usize = 4 * 1024;

/// The section of a configuration file that sets configuration options.
const SECTION_OPTION_SETTINGS: &str = "option_settings";
/// The section that writes files on the instance.
const SECTION_FILES: &str = "files";
/// The section that runs commands before the application is set up.
const SECTION_COMMANDS: &str = "commands";
/// The section that runs commands after it has been extracted, before the web
/// process is started.
const SECTION_CONTAINER_COMMANDS: &str = "container_commands";

/// One file a deployment's configuration declares on the instance.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeclaredFile {
    /// The path on the instance, exactly as the configuration declares it.
    pub path: String,
    /// The content it declares for that path. A file whose content a
    /// configuration does not state — one it names a `source:` to fetch for — is
    /// declared with none, because nothing here fetches a caller-named URL.
    pub content: String,
    /// The `mode:` it declares, if any, as the configuration wrote it (`000644`).
    pub mode: Option<String>,
}

/// One command a deployment's configuration runs on the instance.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeclaredCommand {
    /// The key the configuration gave it, which is what ORDERS the commands.
    pub name: String,
    /// The command line, as a shell is handed it.
    pub command: String,
    /// The directory it runs in, when the configuration states one.
    pub cwd: Option<String>,
}

/// Everything the `.ebextensions` of one source bundle declare.
#[derive(Debug, Clone, Default)]
pub struct BundleConfiguration {
    /// The options the bundle sets, in the order the files declare them.
    pub option_settings: Vec<ConfigurationOptionSetting>,
    /// The files it writes on the instance.
    pub files: Vec<DeclaredFile>,
    /// The commands that run before the application is set up, in key order.
    pub commands: Vec<DeclaredCommand>,
    /// The commands that run after it is extracted and before the web process
    /// starts, in key order.
    pub container_commands: Vec<DeclaredCommand>,
}

impl BundleConfiguration {
    /// Whether the bundle declares nothing at all, which is what a bundle with no
    /// `.ebextensions` leaves.
    pub fn is_empty(&self) -> bool {
        self.option_settings.is_empty()
            && self.files.is_empty()
            && self.commands.is_empty()
            && self.container_commands.is_empty()
    }

    /// Whether anything of it is APPLIED on the instance — a file to write or a
    /// command to run — which is what makes a deployment provision the instance at
    /// all.
    pub fn provisions_the_instance(&self) -> bool {
        !self.files.is_empty() || !self.commands.is_empty() || !self.container_commands.is_empty()
    }
}

/// What the `.ebextensions/*.config` files of `bundle` declare.
///
/// The files are read in the order of their names, which is the order Elastic
/// Beanstalk applies them in, and only the files that folder itself holds
/// (`*.config` IN `.ebextensions`, so an entry deeper under it is not one) — the
/// same set pre-processing validates ([`super::version_processing`]).
#[must_use]
pub fn bundle_configuration(bundle: &ApplicationBundle) -> BundleConfiguration {
    let mut read = BundleConfiguration::default();
    for (path, bytes) in &bundle.files {
        if !is_configuration_file(path) {
            continue;
        }
        let Ok(text) = ::std::str::from_utf8(bytes) else {
            continue;
        };
        let BoundedYaml::Documents(documents) = read_bounded_yaml(text) else {
            ::tracing::debug!(
                entry = %path,
                "a configuration file of a source bundle is not a YAML document this emulator \
                 parses, so the deployment applies nothing it declares",
            );
            continue;
        };
        for document in &documents {
            read_document(document, &mut read);
        }
    }
    read
}

/// Whether `path`, an entry of the source bundle, is one of its configuration
/// files: a `*.config` file IN the `.ebextensions` folder.
fn is_configuration_file(path: &str) -> bool {
    path.strip_prefix(EBEXTENSIONS_FOLDER)
        .is_some_and(|name| !name.contains('/') && name.ends_with(CONFIG_FILE_EXTENSION))
}

/// Read every section one configuration document states into `read`.
fn read_document(document: &::yaml_rust2::Yaml, read: &mut BundleConfiguration) {
    let Some(sections) = document.as_hash() else {
        return;
    };
    for (key, value) in sections {
        match key.as_str() {
            Some(SECTION_OPTION_SETTINGS) => read_option_settings(value, read),
            Some(SECTION_FILES) => read_files(value, read),
            Some(SECTION_COMMANDS) => {
                let mut commands = read_commands(value);
                take_up_to(&mut read.commands, &mut commands, MAX_PROVISIONED_COMMANDS);
            }
            Some(SECTION_CONTAINER_COMMANDS) => {
                let mut commands = read_commands(value);
                take_up_to(
                    &mut read.container_commands,
                    &mut commands,
                    MAX_PROVISIONED_COMMANDS,
                );
            }
            // A section this emulator applies nothing for (`packages:`,
            // `services:`, `users:`) is left where it is: nothing is reported as
            // done for work nothing performed.
            _ => {}
        }
    }
}

/// Append what `read` holds to `into`, up to `bound` entries in all.
fn take_up_to<T>(into: &mut Vec<T>, read: &mut Vec<T>, bound: usize) {
    let room = bound.saturating_sub(into.len());
    if read.len() > room {
        read.truncate(room);
    }
    into.append(read);
}

/// Read the `option_settings:` section, in either form a configuration file writes
/// it: a mapping of namespace to the options it sets, and a sequence of entries
/// naming the namespace, the option and the value in members.
fn read_option_settings(value: &::yaml_rust2::Yaml, read: &mut BundleConfiguration) {
    if let Some(namespaces) = value.as_hash() {
        for (namespace, options) in namespaces {
            let Some(namespace) = namespace.as_str() else {
                continue;
            };
            let Some(options) = options.as_hash() else {
                continue;
            };
            for (option_name, option_value) in options {
                let (Some(option_name), Some(option_value)) =
                    (option_name.as_str(), scalar(option_value))
                else {
                    continue;
                };
                push_setting(read, namespace, option_name, &option_value);
            }
        }
        return;
    }
    let Some(entries) = value.as_vec() else {
        return;
    };
    for entry in entries {
        let Some(entry) = entry.as_hash() else {
            continue;
        };
        let member = |name: &str| {
            entry
                .get(&::yaml_rust2::Yaml::String(name.to_string()))
                .and_then(scalar)
        };
        let (Some(namespace), Some(option_name)) = (member("namespace"), member("option_name"))
        else {
            continue;
        };
        let option_value = member("value").unwrap_or_default();
        push_setting(read, &namespace, &option_name, &option_value);
    }
}

/// Record one setting the bundle declares, as the shape every configuration
/// setting of this service travels in.
fn push_setting(read: &mut BundleConfiguration, namespace: &str, option: &str, value: &str) {
    if read.option_settings.len() >= MAX_CONFIGURATION_OPTION_SETTINGS {
        return;
    }
    read.option_settings.push(ConfigurationOptionSetting {
        resource_name: None,
        namespace: Some(namespace.to_string()),
        option_name: Some(option.to_string()),
        value: Some(value.to_string()),
    });
}

/// Read the `files:` section, in either form: a mapping of path to what the
/// entry declares, and a sequence of entries naming the path in a member.
fn read_files(value: &::yaml_rust2::Yaml, read: &mut BundleConfiguration) {
    let declared: Vec<DeclaredFile> = match (value.as_hash(), value.as_vec()) {
        (Some(paths), _) => paths
            .iter()
            .filter_map(|(path, entry)| declared_file(path.as_str()?, entry))
            .collect(),
        (None, Some(entries)) => entries
            .iter()
            .filter_map(|entry| {
                let path = entry
                    .as_hash()?
                    .get(&::yaml_rust2::Yaml::String("path".to_string()))
                    .and_then(scalar)?;
                declared_file(&path, entry)
            })
            .collect(),
        (None, None) => Vec::new(),
    };
    let mut declared = declared;
    take_up_to(&mut read.files, &mut declared, MAX_PROVISIONED_FILES);
}

/// One `files:` entry: the content and the mode it declares for `path`.
fn declared_file(path: &str, entry: &::yaml_rust2::Yaml) -> Option<DeclaredFile> {
    if path.is_empty() {
        return None;
    }
    let (content, mode) = match entry.as_hash() {
        Some(members) => {
            let member = |name: &str| {
                members
                    .get(&::yaml_rust2::Yaml::String(name.to_string()))
                    .and_then(scalar)
            };
            (member("content").unwrap_or_default(), member("mode"))
        }
        // A path declared with its content directly, which is the shortest form a
        // configuration file writes.
        None => (scalar(entry)?, None),
    };
    let mut content = content;
    truncate_on_a_boundary(&mut content, MAX_PROVISIONED_FILE_BYTES);
    Some(DeclaredFile {
        path: path.to_string(),
        content,
        mode,
    })
}

/// Read a command section, in either form: a mapping of name to what the command
/// declares — in KEY ORDER, which is the order Elastic Beanstalk runs them in —
/// and a sequence of entries naming the command in a member.
fn read_commands(value: &::yaml_rust2::Yaml) -> Vec<DeclaredCommand> {
    let declared: Vec<DeclaredCommand> = match (value.as_hash(), value.as_vec()) {
        (Some(named), _) => {
            let mut declared: Vec<DeclaredCommand> = named
                .iter()
                .filter_map(|(name, entry)| declared_command(name.as_str()?, entry))
                .collect();
            declared.sort_by(|left, right| left.name.cmp(&right.name));
            declared
        }
        (None, Some(entries)) => entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| {
                let named = entry
                    .as_hash()
                    .and_then(|members| {
                        members
                            .get(&::yaml_rust2::Yaml::String("name".to_string()))
                            .and_then(scalar)
                    })
                    .unwrap_or_else(|| index.to_string());
                declared_command(&named, entry)
            })
            .collect(),
        (None, None) => Vec::new(),
    };
    declared
}

/// One command entry: the command line and the directory it runs in.
fn declared_command(name: &str, entry: &::yaml_rust2::Yaml) -> Option<DeclaredCommand> {
    let (command, cwd) = match entry.as_hash() {
        Some(members) => {
            let member = |named: &str| {
                members
                    .get(&::yaml_rust2::Yaml::String(named.to_string()))
                    .and_then(scalar)
            };
            (member("command")?, member("cwd"))
        }
        // A command declared directly, with nothing else about it.
        None => (scalar(entry)?, None),
    };
    let mut command = command;
    truncate_on_a_boundary(&mut command, MAX_COMMAND_BYTES);
    (!command.trim().is_empty()).then(|| DeclaredCommand {
        name: name.to_string(),
        command,
        cwd,
    })
}

/// A YAML scalar as the text a configuration file states, or `None` for a node
/// that is no scalar — a value a configuration states as a collection says
/// nothing this emulator could write to a file or hand to a shell.
///
/// A number and a boolean are the text the document carries, so a `mode:` written
/// `000644` and a value written `8080` are the strings a configuration states.
fn scalar(value: &::yaml_rust2::Yaml) -> Option<String> {
    match value {
        ::yaml_rust2::Yaml::String(text) => Some(text.clone()),
        ::yaml_rust2::Yaml::Integer(number) => Some(number.to_string()),
        ::yaml_rust2::Yaml::Real(text) => Some(text.clone()),
        ::yaml_rust2::Yaml::Boolean(flag) => Some(flag.to_string()),
        _ => None,
    }
}

/// Hold `text` to `bound` bytes, on a character boundary so what is kept is still
/// a string.
fn truncate_on_a_boundary(text: &mut String, bound: usize) {
    if text.len() <= bound {
        return;
    }
    let mut end = bound;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bundle carrying `files`, read the way a source bundle is.
    fn bundle(files: &[(&str, &[u8])]) -> ApplicationBundle {
        ApplicationBundle {
            files: files
                .iter()
                .map(|(path, bytes)| ((*path).to_string(), bytes.to_vec()))
                .collect(),
        }
    }

    /// RULE: the `option_settings:` of a configuration file are the options the
    /// bundle sets, in either form a file writes them.
    #[test]
    fn the_option_settings_are_read_in_either_form() {
        const MAPPING: &[u8] = br#"
option_settings:
  aws:elasticbeanstalk:application:environment:
    FROM_BUNDLE: carried
    PORT_NUMBER: 8080
"#;
        const SEQUENCE: &[u8] = br#"
option_settings:
  - namespace: aws:elasticbeanstalk:application:environment
    option_name: FROM_BUNDLE
    value: carried
  - namespace: aws:elasticbeanstalk:application:environment
    option_name: PORT_NUMBER
    value: 8080
"#;
        for (case, document) in [("the mapping form", MAPPING), ("the sequence form", SEQUENCE)] {
            let read = bundle_configuration(&bundle(&[(".ebextensions/env.config", document)]));
            let settings: Vec<(&str, &str)> = read
                .option_settings
                .iter()
                .filter_map(|setting| {
                    Some((setting.option_name.as_deref()?, setting.value.as_deref()?))
                })
                .collect();
            assert_eq!(
                settings,
                vec![("FROM_BUNDLE", "carried"), ("PORT_NUMBER", "8080")],
                "case {case:?}: expected the options the file sets, got {settings:?}"
            );
            let namespaces: Vec<&str> = read
                .option_settings
                .iter()
                .filter_map(|setting| setting.namespace.as_deref())
                .collect();
            assert!(
                namespaces
                    .iter()
                    .all(|namespace| *namespace == NAMESPACE_APPLICATION_ENVIRONMENT),
                "case {case:?}: expected the namespace the file names, got {namespaces:?}"
            );
        }
    }

    /// RULE: the `files:` and command sections are what the deployment applies on
    /// the instance — the declared path with the declared content, and the
    /// commands in the order their keys put them in.
    #[test]
    fn the_applied_sections_are_read_in_key_order() {
        const DOCUMENT: &[u8] = br#"
files:
  "/tmp/written.txt":
    mode: "000644"
    owner: root
    content: |
      written by the configuration
  "/tmp/short.txt": inline content
container_commands:
  02_second:
    command: printf second > /tmp/second
  01_first:
    command: printf first > /tmp/first
    cwd: /var/app/staging
commands:
  00_before:
    command: printf before > /tmp/before
"#;
        let read = bundle_configuration(&bundle(&[(".ebextensions/01-provision.config", DOCUMENT)]));
        let files: Vec<(&str, &str, Option<&str>)> = read
            .files
            .iter()
            .map(|file| (file.path.as_str(), file.content.as_str(), file.mode.as_deref()))
            .collect();
        assert_eq!(
            files,
            vec![
                (
                    "/tmp/written.txt",
                    "written by the configuration\n",
                    Some("000644")
                ),
                ("/tmp/short.txt", "inline content", None),
            ],
            "expected the declared files, in the order the document declares them, got {files:?}"
        );
        let ordered: Vec<&str> = read
            .container_commands
            .iter()
            .map(|command| command.name.as_str())
            .collect();
        assert_eq!(
            ordered,
            vec!["01_first", "02_second"],
            "expected the container commands in key order, got {ordered:?}"
        );
        assert_eq!(
            read.container_commands
                .first()
                .and_then(|command| command.cwd.as_deref()),
            Some("/var/app/staging"),
            "expected the directory the entry declares, got {:?}",
            read.container_commands
        );
        let before: Vec<&str> = read
            .commands
            .iter()
            .map(|command| command.command.as_str())
            .collect();
        assert_eq!(
            before,
            vec!["printf before > /tmp/before"],
            "expected the commands that run before the application is set up, got {before:?}"
        );
    }

    /// RULE: only a `*.config` file IN the `.ebextensions` folder is a
    /// configuration file of the bundle, and a bundle carrying none declares
    /// nothing.
    #[test]
    fn only_the_configuration_files_of_the_folder_are_read() {
        const DOCUMENT: &[u8] = b"files:\n  \"/tmp/x\": y\n";
        let cases: &[(&str, &str, bool)] = &[
            ("a configuration file of the folder", ".ebextensions/a.config", true),
            ("a file deeper under the folder", ".ebextensions/nested/a.config", false),
            ("another file of the folder", ".ebextensions/a.yaml", false),
            ("a configuration file at the bundle's root", "a.config", false),
        ];
        for (case, path, declares) in cases {
            let read = bundle_configuration(&bundle(&[(path, DOCUMENT)]));
            assert_eq!(
                !read.is_empty(),
                *declares,
                "case {case:?}: expected declares={declares}, got {read:?}"
            );
        }
    }

    /// RULE: a configuration file that does not parse, and a section this
    /// emulator applies nothing for, leave the deployment with nothing to apply —
    /// never with an entry nobody declared.
    #[test]
    fn an_unreadable_file_declares_nothing() {
        let cases: &[(&str, &[u8])] = &[
            ("a document that does not parse", b"option_settings: [unclosed\n"),
            ("a section nothing is applied for", b"packages:\n  yum:\n    git: []\n"),
            ("an empty document", b""),
        ];
        for (case, document) in cases {
            let read = bundle_configuration(&bundle(&[(".ebextensions/a.config", document)]));
            assert!(
                read.is_empty() && !read.provisions_the_instance(),
                "case {case:?}: expected nothing declared, got {read:?}"
            );
        }
    }
}

// SPDX-License-Identifier: CC0-1.0

//! The HADOOP COMMANDS a cluster's nodes provide: `hadoop` and `hdfs`, as the environment of a
//! step's command line provides them.
//!
//! # The rule this module implements
//!
//! "command-runner.jar … lets you run commands on your cluster" (Amazon EMR documentation), so
//! a `command-runner.jar` step's arguments are a command line and what that command line does
//! is the step's own work. The cluster's nodes have the Hadoop distribution its release
//! installs ([`super::hadoop_configuration`]), so the commands that distribution ships are
//! commands of that line: `hadoop fs` / `hdfs dfs`, "the FS shell … invoked by `bin/hadoop fs
//! <args>`" whose "commands take path URIs as arguments" (Apache Hadoop, "Filesystem Shell"),
//! `hdfs getconf -confKey <key>`, which "gets configuration values from the configuration
//! directory" (Apache Hadoop, "HDFS Commands Guide"), and `hadoop version`, which prints the
//! version of Hadoop that is installed.
//!
//! So a step whose script writes through `hadoop fs` REALLY writes: the object a `-put` of an
//! `s3://` path names exists in this emulator's object store once the step has run, the file a
//! `-put` of an HDFS path names is in the cluster's own HDFS and the NEXT step of that cluster
//! reads it back ([`super::node_file_systems`]), and the status the command exits with is the
//! status the step ends in — zero when the operation happened and one when the filesystem
//! refused it, which is what `set -e` around it stops on.
//!
//! This module is ONE PROGRAM of that command line's environment
//! ([`::service_common::posix_shell_session`], whose session-local utilities, bounds and
//! containment are stated there): the invocations below and nothing else. NO host command,
//! process or shell is started here or anywhere this calls, no host path is opened, and an
//! invocation this program does not perform — another command of the distribution, another
//! form of one of these, an option that changes what one does — is answered with `None`, which
//! the session reports as a command it could not run and the step then reports as work that was
//! not run. Nothing a caller can write makes that list longer.

use ::service_common::posix_shell_files::FileRefusal;
use ::service_common::posix_shell_session::{
    command_base_name, EnvironmentPrograms, ProgramFiles, ProgramOutcome,
};
use ::service_common::posix_shell_volume::EntryKind;

use super::*;

/// The names a command line invokes the cluster's Hadoop distribution by.
pub const HADOOP_PROGRAMS: [&str; 2] = ["hadoop", "hdfs"];

/// `hdfs getconf`, which reads a value out of the cluster's resolved configuration.
const GETCONF_COMMAND: &str = "getconf";

/// The option `getconf` names the property it reads by.
const CONF_KEY_OPTION: &str = "-confKey";

/// `hadoop version`.
const VERSION_COMMAND: &str = "version";

/// The status a command answers when the filesystem performed what it asked: the shell's own
/// success, which is what a step completing follows from.
const PERFORMED: i32 = 0;

/// The status the filesystem shell exits with when an operation of it did not happen.
const REFUSED: i32 = 1;

/// How many path operands ONE invocation walks. CHOSEN: a bound on what one command of a step's
/// command line costs, far above any real invocation.
const MAX_PATH_OPERANDS: usize = 32;

/// What `hdfs getconf` reports for a property the cluster's configuration does not carry, as
/// the command words it, exiting non-zero.
#[must_use]
pub fn missing_configuration_message(key: &str) -> String {
    format!("Configuration {key} is missing.")
}

/// The user an Amazon EMR cluster's commands run as, which is the owner and the group a listing
/// of a file one of them wrote reports (Amazon EMR Management Guide — the `hadoop` user).
const CLUSTER_USER: &str = "hadoop";

/// The permissions a listing reports for a FILE and for a DIRECTORY: what Hadoop's default
/// `fs.permissions.umask-mode` of `022` leaves of the created-file and created-directory modes.
const FILE_PERMISSIONS: &str = "-rw-r--r--";
const DIRECTORY_PERMISSIONS: &str = "drwxr-xr-x";

/// How a listing writes the instant an entry was last written, and how `-stat`'s `%y` does:
/// the two formats those commands print (Apache Hadoop, "Filesystem Shell").
const LISTING_TIME_FORMAT: &str = "%Y-%m-%d %H:%M";
const STAT_TIME_FORMAT: &str = "%Y-%m-%d %H:%M:%S";

/// ONE file carried from one of the cluster's filesystems to another, as the invocation asked
/// for it.
struct Carried<'a> {
    /// The command of the filesystem shell that asked, which is what its diagnostic names.
    command: &'a str,
    source: &'a HadoopPath,
    destination: &'a HadoopPath,
    /// Whether the invocation asked to overwrite a destination that is already there.
    overwrite: bool,
    /// What the command was given on its own standard input, which is what a source that is
    /// the stream operand carries.
    standard_input: Option<&'a str>,
}

/// The Hadoop distribution installed on a cluster's nodes, as the environment of one command of
/// one step provides it.
#[derive(Debug, Clone)]
pub struct HadoopCli {
    /// The filesystems the paths of its invocations name.
    systems: HadoopFileSystems,
}

/// The Hadoop commands the nodes of `cluster` provide, and `None` for a cluster whose release
/// installs no Hadoop this emulator can account for — whose nodes then have no such command.
///
/// `objects` is the ONE program of the command's environment that reaches Amazon S3, so an
/// EMRFS path reaches the store under the same credential and the same budget as the command's
/// other calls ([`super::step_command_transfers`]).
#[must_use]
pub fn cluster_hadoop_cli(
    cluster: &ClusterRecord,
    objects: Option<StepObjectCli>,
) -> Option<HadoopCli> {
    let configuration = cluster_hadoop_configuration(cluster);
    configuration.version()?;
    Some(HadoopCli {
        systems: HadoopFileSystems {
            hdfs: cluster.hdfs.clone(),
            objects,
            configuration,
        },
    })
}

impl EnvironmentPrograms for HadoopCli {
    fn run(
        &self,
        name: &str,
        arguments: &[String],
        files: &mut dyn ProgramFiles,
    ) -> Option<ProgramOutcome> {
        self.run_with_input(name, arguments, None, files)
    }

    /// The invocations this module's header states, performed against the cluster's
    /// filesystems; `None` for every other program name and every other invocation, which the
    /// session reports as a command it could not run.
    fn run_with_input(
        &self,
        name: &str,
        arguments: &[String],
        standard_input: Option<&str>,
        files: &mut dyn ProgramFiles,
    ) -> Option<ProgramOutcome> {
        let program = command_base_name(name);
        if !HADOOP_PROGRAMS.contains(&program) {
            return None;
        }
        let (command, operands) = arguments.split_first()?;
        match (program, command.as_str()) {
            // The filesystem shell: `hadoop fs` and `hdfs dfs` are the two spellings of it.
            ("hadoop", "fs") | ("hdfs", "dfs") => {
                self.file_system_shell(operands, standard_input, files)
            }
            ("hdfs", GETCONF_COMMAND) => self.reported_configuration(operands),
            (_, VERSION_COMMAND) if operands.is_empty() => self.reported_version(),
            _ => None,
        }
    }
}

impl HadoopCli {
    /// What `hadoop version` prints: the version of Hadoop installed on the cluster's nodes,
    /// on the line the command writes it.
    fn reported_version(&self) -> Option<ProgramOutcome> {
        let version = self.systems.configuration.version()?;
        Some(ProgramOutcome {
            status: PERFORMED,
            output: vec![format!("Hadoop {version}")],
        })
    }

    /// What `hdfs getconf -confKey <key>` answers: the value the cluster's nodes resolve for
    /// that property, or the command's own report that the configuration does not carry it.
    fn reported_configuration(&self, operands: &[String]) -> Option<ProgramOutcome> {
        let [option, key] = operands else {
            return None;
        };
        if option.as_str() != CONF_KEY_OPTION {
            return None;
        }
        let Some(value) = self.systems.configuration.value(key) else {
            return Some(ProgramOutcome {
                status: REFUSED,
                output: vec![missing_configuration_message(key)],
            });
        };
        Some(ProgramOutcome {
            status: PERFORMED,
            output: vec![value],
        })
    }

    /// One invocation of the FILESYSTEM SHELL: the command its first operand names, performed
    /// over the paths after it.
    fn file_system_shell(
        &self,
        operands: &[String],
        standard_input: Option<&str>,
        files: &mut dyn ProgramFiles,
    ) -> Option<ProgramOutcome> {
        let (command, rest) = operands.split_first()?;
        let command = command.as_str();
        if rest.len() > MAX_PATH_OPERANDS {
            return None;
        }
        match command {
            "-mkdir" => self.made_directories(command, rest, files),
            "-put" | "-copyFromLocal" => self.carried(command, rest, true, standard_input, files),
            "-get" | "-copyToLocal" => self.carried(command, rest, false, standard_input, files),
            "-cp" => self.copied(command, rest, files),
            "-mv" => self.moved(command, rest, files),
            "-cat" | "-text" => self.printed(command, rest, files),
            "-rm" => self.removed(command, rest, files),
            "-du" => self.disk_usage(command, rest, files),
            "-stat" => self.printed_statistics(command, rest, files),
            "-test" => self.tested(rest, files),
            "-touchz" => self.touched(command, rest, files),
            "-ls" => self.listed(command, rest, files),
            _ => None,
        }
    }

    /// `-mkdir [-p] <path>…`: "takes path uri's as argument and creates directories"; `-p`
    /// "creates parent directories along the path".
    fn made_directories(
        &self,
        command: &str,
        operands: &[String],
        files: &mut dyn ProgramFiles,
    ) -> Option<ProgramOutcome> {
        let (flags, paths) = split_flags(operands, &["-p"])?;
        let parents = flags.contains(&"-p");
        let paths = resolved_paths(&paths, false)?;
        let mut answered = Answered::default();
        for path in &paths {
            // Without `-p` a directory that is already there is the shell's own failure, and
            // with it the command answers for the whole chain.
            if !parents && self.systems.is_directory(path, files) {
                answered.refuse(command, path, &HadoopRefusal::Path(FileRefusal::AlreadyExists));
                continue;
            }
            if let Err(refusal) = self.systems.make_directories(path, files) {
                answered.refuse(command, path, &refusal);
            }
        }
        Some(answered.outcome())
    }

    /// `-put`/`-copyFromLocal` (`local` sources) and `-get`/`-copyToLocal` (a local
    /// destination): "copy single src, or multiple srcs … to the destination file system";
    /// `-f` "overwrites the destination if it already exists".
    fn carried(
        &self,
        command: &str,
        operands: &[String],
        from_local: bool,
        standard_input: Option<&str>,
        files: &mut dyn ProgramFiles,
    ) -> Option<ProgramOutcome> {
        let (flags, written) = split_flags(operands, &["-f"])?;
        let overwrite = flags.contains(&"-f");
        let (destination, sources) = written.split_last()?;
        if sources.is_empty() {
            return None;
        }
        let destination = hadoop_path(destination, !from_local)?;
        let sources = resolved_paths(sources, from_local)?;
        let mut answered = Answered::default();
        for source in &sources {
            self.carry(
                &Carried {
                    command,
                    source,
                    destination: &destination,
                    overwrite,
                    standard_input,
                },
                files,
                &mut answered,
            );
        }
        Some(answered.outcome())
    }

    /// `-cp [-f] <src> <dst>`: "copy files from source to destination", between any two of the
    /// cluster's filesystems.
    fn copied(
        &self,
        command: &str,
        operands: &[String],
        files: &mut dyn ProgramFiles,
    ) -> Option<ProgramOutcome> {
        let (flags, written) = split_flags(operands, &["-f"])?;
        let overwrite = flags.contains(&"-f");
        let [source, destination] = written[..] else {
            return None;
        };
        let source = hadoop_path(source, false)?;
        let destination = hadoop_path(destination, false)?;
        let mut answered = Answered::default();
        self.carry(
            &Carried {
                command,
                source: &source,
                destination: &destination,
                overwrite,
                standard_input: None,
            },
            files,
            &mut answered,
        );
        Some(answered.outcome())
    }

    /// `-mv <src> <dst>`: "moves files from source to destination" — the copy, and then the
    /// source taken away once the destination holds it.
    fn moved(
        &self,
        command: &str,
        operands: &[String],
        files: &mut dyn ProgramFiles,
    ) -> Option<ProgramOutcome> {
        let (flags, written) = split_flags(operands, &[])?;
        let _ = flags;
        let [source, destination] = written[..] else {
            return None;
        };
        let source = hadoop_path(source, false)?;
        let destination = hadoop_path(destination, false)?;
        let mut answered = Answered::default();
        self.carry(
            &Carried {
                command,
                source: &source,
                destination: &destination,
                overwrite: false,
                standard_input: None,
            },
            files,
            &mut answered,
        );
        if answered.status == PERFORMED {
            if let Err(refusal) = self.systems.remove(&source, false, files) {
                answered.refuse(command, &source, &refusal);
            }
        }
        Some(answered.outcome())
    }

    /// Carry ONE file from `source` to `destination`, recording what it came to.
    ///
    /// A destination that is a DIRECTORY takes the file under its own name, which is what a
    /// copy into a directory does; a destination that already holds a file is the shell's own
    /// `File exists` unless the invocation asked to overwrite it.
    fn carry(
        &self,
        carried: &Carried<'_>,
        files: &mut dyn ProgramFiles,
        answered: &mut Answered,
    ) {
        let Carried {
            command,
            source,
            destination,
            overwrite,
            standard_input,
        } = *carried;
        let into = match (
            self.systems.is_directory(destination, files),
            source.base_name().as_str(),
        ) {
            (true, "") => destination.clone(),
            (true, name) => destination.joined(name),
            (false, _) => destination.clone(),
        };
        if !overwrite && !matches!(into, HadoopPath::Stream) {
            match self.systems.entry(&into, files) {
                Ok(Some(_)) => {
                    answered.refuse(
                        command,
                        &into,
                        &HadoopRefusal::Path(FileRefusal::AlreadyExists),
                    );
                    return;
                }
                Ok(None) => {}
                Err(refusal) => {
                    answered.refuse(command, &into, &refusal);
                    return;
                }
            }
        }
        let contents = match self.systems.read(source, standard_input, files) {
            Ok(contents) => contents,
            Err(refusal) => {
                answered.refuse(command, source, &refusal);
                return;
            }
        };
        match self.systems.write(&into, &contents, files) {
            Ok(written) => answered.wrote(written),
            Err(refusal) => answered.refuse(command, &into, &refusal),
        }
    }

    /// `-cat`/`-text <path>…`: "copies source paths to stdout".
    fn printed(
        &self,
        command: &str,
        operands: &[String],
        files: &mut dyn ProgramFiles,
    ) -> Option<ProgramOutcome> {
        let (flags, written) = split_flags(operands, &[])?;
        let _ = flags;
        let paths = resolved_paths(&written, false)?;
        if paths.is_empty() {
            return None;
        }
        let mut answered = Answered::default();
        for path in &paths {
            match self.systems.read(path, None, files) {
                Ok(contents) => answered.wrote(contents.lines().map(str::to_string).collect()),
                Err(refusal) => answered.refuse(command, path, &refusal),
            }
        }
        Some(answered.outcome())
    }

    /// `-rm [-f] [-r] <path>…`: "delete files specified as args"; `-r` "deletes the directory
    /// and any content under it", `-f` "will not display a diagnostic message or modify the
    /// exit status to reflect an error if the file does not exist".
    fn removed(
        &self,
        command: &str,
        operands: &[String],
        files: &mut dyn ProgramFiles,
    ) -> Option<ProgramOutcome> {
        let (flags, written) = split_flags(operands, &["-f", "-r", "-R", "-skipTrash"])?;
        let recursive = flags.contains(&"-r") || flags.contains(&"-R");
        let quiet = flags.contains(&"-f");
        let paths = resolved_paths(&written, false)?;
        if paths.is_empty() {
            return None;
        }
        let mut answered = Answered::default();
        for path in &paths {
            match self.systems.remove(path, recursive, files) {
                Ok(()) => answered.wrote(vec![format!("Deleted {}", path.shown())]),
                Err(HadoopRefusal::Path(FileRefusal::NotFound)) if quiet => {}
                Err(refusal) => answered.refuse(command, path, &refusal),
            }
        }
        Some(answered.outcome())
    }

    /// `-du [-s] <path>…`: "displays sizes of files and directories contained in the given
    /// directory or the length of a file"; `-s` "will result in an aggregate summary of file
    /// lengths being displayed, rather than the individual files".
    ///
    /// The two columns are the ones the command prints: the bytes the path holds, and the bytes
    /// it consumes in the filesystem — the size times the replication that filesystem keeps.
    fn disk_usage(
        &self,
        command: &str,
        operands: &[String],
        files: &mut dyn ProgramFiles,
    ) -> Option<ProgramOutcome> {
        let (flags, written) = split_flags(operands, &["-s"])?;
        let summary = flags.contains(&"-s");
        let paths = resolved_paths(&written, false)?;
        if paths.is_empty() {
            return None;
        }
        let mut answered = Answered::default();
        for path in &paths {
            let listed: Vec<HadoopPath> = if summary || !self.systems.is_directory(path, files) {
                vec![path.clone()]
            } else {
                match self.systems.listed(path, files) {
                    Ok(entries) => entries.into_iter().map(|(under, _)| under).collect(),
                    Err(refusal) => {
                        answered.refuse(command, path, &refusal);
                        continue;
                    }
                }
            };
            for one in &listed {
                match self.systems.size_under(one, files) {
                    Ok(size) => {
                        let consumed = size.saturating_mul(u64::from(self.systems.replication(one)));
                        answered.wrote(vec![format!("{size}  {consumed}  {}", one.shown())]);
                    }
                    Err(refusal) => answered.refuse(command, one, &refusal),
                }
            }
        }
        Some(answered.outcome())
    }

    /// `-stat [format] <path>…`: "print statistics about the file/directory … in the specified
    /// format", whose specifiers are the ones the command documents.
    fn printed_statistics(
        &self,
        command: &str,
        operands: &[String],
        files: &mut dyn ProgramFiles,
    ) -> Option<ProgramOutcome> {
        let (flags, written) = split_flags(operands, &[])?;
        let _ = flags;
        // The format is the first operand when the invocation carries one; a `-stat` with only
        // paths prints the modification time, which is the command's own default format.
        let (format, written) = match written.split_first() {
            Some((first, rest)) if first.contains('%') && !rest.is_empty() => {
                ((*first).to_string(), rest.to_vec())
            }
            _ => ("%y".to_string(), written.clone()),
        };
        let paths = resolved_paths(&written, false)?;
        if paths.is_empty() {
            return None;
        }
        let mut answered = Answered::default();
        for path in &paths {
            match self.systems.entry(path, files) {
                Ok(Some(entry)) => {
                    // A specifier this program answers nothing for: the invocation is one the
                    // session reports it could not run rather than a line with a hole in it.
                    let line = self.statistics(&format, path, &entry)?;
                    answered.wrote(vec![line]);
                }
                Ok(None) => answered.refuse(
                    command,
                    path,
                    &HadoopRefusal::Path(FileRefusal::NotFound),
                ),
                Err(refusal) => answered.refuse(command, path, &refusal),
            }
        }
        Some(answered.outcome())
    }

    /// `format` with each specifier replaced by what it stands for, and `None` when it carries
    /// a specifier this program answers nothing for — including a time on a filesystem that
    /// keeps none.
    fn statistics(&self, format: &str, path: &HadoopPath, entry: &HadoopEntry) -> Option<String> {
        let mut rendered = String::new();
        let mut specifiers = format.split('%');
        rendered.push_str(specifiers.next().unwrap_or_default());
        for rest in specifiers {
            let mut characters = rest.chars();
            let specifier = characters.next()?;
            let value = match specifier {
                // "filesize in blocks (%b)" is the bytes the file holds.
                'b' => entry.size.to_string(),
                'F' => match entry.kind {
                    EntryKind::Directory => "directory".to_string(),
                    _ => "regular file".to_string(),
                },
                'n' => path.base_name(),
                'o' => self.systems.block_size().to_string(),
                'r' => self.systems.replication(path).to_string(),
                'u' | 'g' => CLUSTER_USER.to_string(),
                'y' => rendered_time(entry.modified?, STAT_TIME_FORMAT),
                'Y' => entry
                    .modified?
                    .duration_since(::std::time::UNIX_EPOCH)
                    .ok()?
                    .as_millis()
                    .to_string(),
                '%' => "%".to_string(),
                _ => return None,
            };
            rendered.push_str(&value);
            rendered.push_str(characters.as_str());
        }
        Some(rendered)
    }

    /// `-test -[defsz] <path>`: "answer[s] … 0 if true" for the one property the invocation
    /// names, and writes nothing either way.
    fn tested(
        &self,
        operands: &[String],
        files: &mut dyn ProgramFiles,
    ) -> Option<ProgramOutcome> {
        let [option, written] = operands else {
            return None;
        };
        let path = hadoop_path(written, false)?;
        let entry = self.systems.entry(&path, files).ok().flatten();
        let holds = match option.as_str() {
            // "-d: f is a directory, return 0."
            "-d" => entry.is_some_and(|entry| entry.kind == EntryKind::Directory),
            // "-e: file exists, return 0."
            "-e" => entry.is_some(),
            // "-f: f is a file, return 0."
            "-f" => entry.is_some_and(|entry| entry.kind == EntryKind::File),
            // "-s: file is greater than zero bytes in size, return 0."
            "-s" => entry.is_some_and(|entry| entry.size > 0),
            // "-z: file is zero bytes in size, return 0."
            "-z" => entry.is_some_and(|entry| entry.size == 0),
            _ => return None,
        };
        Some(ProgramOutcome {
            status: if holds { PERFORMED } else { REFUSED },
            output: Vec::new(),
        })
    }

    /// `-touchz <path>…`: "create a file of zero length".
    fn touched(
        &self,
        command: &str,
        operands: &[String],
        files: &mut dyn ProgramFiles,
    ) -> Option<ProgramOutcome> {
        let (flags, written) = split_flags(operands, &[])?;
        let _ = flags;
        let paths = resolved_paths(&written, false)?;
        if paths.is_empty() {
            return None;
        }
        let mut answered = Answered::default();
        for path in &paths {
            match self.systems.entry(path, files) {
                // "An error is returned if the file exists with non-zero length."
                Ok(Some(entry)) if entry.size > 0 => answered.refuse(
                    command,
                    path,
                    &HadoopRefusal::Path(FileRefusal::AlreadyExists),
                ),
                Ok(_) => {
                    if let Err(refusal) = self.systems.write(path, "", files) {
                        answered.refuse(command, path, &refusal);
                    }
                }
                Err(refusal) => answered.refuse(command, path, &refusal),
            }
        }
        Some(answered.outcome())
    }

    /// `-ls <path>`: the entries under the path, in the long format the command prints — the
    /// entry's permissions, the copies the filesystem keeps of it, its owner and group, the
    /// bytes it holds, when it was last written, and its path.
    fn listed(
        &self,
        command: &str,
        operands: &[String],
        files: &mut dyn ProgramFiles,
    ) -> Option<ProgramOutcome> {
        let (flags, written) = split_flags(operands, &[])?;
        let _ = flags;
        let paths = resolved_paths(&written, false)?;
        if paths.is_empty() {
            return None;
        }
        let mut answered = Answered::default();
        for path in &paths {
            let entries = match self.systems.entry(path, files) {
                Ok(Some(entry)) if entry.kind != EntryKind::Directory => {
                    vec![(path.clone(), entry)]
                }
                Ok(Some(_)) => match self.systems.listed(path, files) {
                    Ok(entries) => {
                        answered.wrote(vec![format!("Found {} items", entries.len())]);
                        entries
                    }
                    Err(refusal) => {
                        answered.refuse(command, path, &refusal);
                        continue;
                    }
                },
                Ok(None) => {
                    answered.refuse(command, path, &HadoopRefusal::Path(FileRefusal::NotFound));
                    continue;
                }
                Err(refusal) => {
                    answered.refuse(command, path, &refusal);
                    continue;
                }
            };
            for (under, entry) in &entries {
                // A filesystem that keeps no instant for an entry has no listing in this
                // format to print for it.
                let written_at = rendered_time(entry.modified?, LISTING_TIME_FORMAT);
                let (permissions, copies) = match entry.kind {
                    EntryKind::Directory => (DIRECTORY_PERMISSIONS, "-".to_string()),
                    _ => (
                        FILE_PERMISSIONS,
                        self.systems.replication(under).to_string(),
                    ),
                };
                answered.wrote(vec![format!(
                    "{permissions} {copies:>3} {CLUSTER_USER} {CLUSTER_USER} {:>10} \
                     {written_at} {}",
                    entry.size,
                    under.shown()
                )]);
            }
        }
        Some(answered.outcome())
    }
}

/// What one invocation of the filesystem shell has answered so far: the lines it wrote and
/// whether every operation of it happened.
#[derive(Debug, Default)]
struct Answered {
    status: i32,
    output: Vec<String>,
}

impl Answered {
    /// Record `lines` as what the command wrote.
    fn wrote(&mut self, lines: Vec<String>) {
        self.output.extend(lines);
    }

    /// Record that the filesystem refused the operation on `path`: the command reports it and
    /// exits non-zero, and the operations after it in the same invocation still happen — which
    /// is what the shell does with a list of paths.
    fn refuse(&mut self, command: &str, path: &HadoopPath, refusal: &HadoopRefusal) {
        self.output
            .push(refusal.reported(command, &path.shown()));
        self.status = REFUSED;
    }

    /// This invocation as the program's own answer.
    fn outcome(self) -> ProgramOutcome {
        ProgramOutcome {
            status: self.status,
            output: self.output,
        }
    }
}

/// The FLAGS and the OPERANDS of one invocation, and `None` when it carries an option this
/// command does not take — an option changes what a command does, so an invocation carrying one
/// this program does not perform is not this invocation.
///
/// The command's own stream operand is an OPERAND rather than an option, however it is spelled.
fn split_flags<'a>(
    operands: &'a [String],
    allowed: &[&str],
) -> Option<(Vec<&'a str>, Vec<&'a str>)> {
    let mut flags: Vec<&str> = Vec::new();
    let mut paths: Vec<&str> = Vec::new();
    for operand in operands {
        let word = operand.as_str();
        if word == STREAM_PATH || !word.starts_with('-') {
            paths.push(word);
            continue;
        }
        if !allowed.contains(&word) {
            return None;
        }
        flags.push(word);
    }
    Some((flags, paths))
}

/// What each of `written` names, and `None` when one of them names nothing this cluster has a
/// filesystem for.
fn resolved_paths(written: &[&str], local: bool) -> Option<Vec<HadoopPath>> {
    written
        .iter()
        .map(|path| hadoop_path(path, local))
        .collect()
}

/// `at` as a command of the distribution prints an instant, in UTC.
fn rendered_time(at: ::std::time::SystemTime, format: &str) -> String {
    ::chrono::DateTime::<::chrono::Utc>::from(at)
        .format(format)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::{split_flags, HADOOP_PROGRAMS};

    /// RULE: an option changes what a command does, so an invocation carrying one the command
    /// does not take is not an invocation this program performs — and the command's own stream
    /// operand is an operand, not an option.
    ///
    /// The rows span a command's own flags, a flag another command takes, the stream operand
    /// and an invocation with no flag at all.
    #[test]
    fn only_the_options_a_command_takes_are_read() {
        // (case, operands, the flags it takes, what it splits into)
        type Row = (&'static str, &'static [&'static str], &'static [&'static str], Option<(Vec<&'static str>, Vec<&'static str>)>);
        let rows: [Row; 5] = [
            (
                "a flag the command takes",
                &["-f", "/tmp/one", "s3://reports/one"],
                &["-f"],
                Some((vec!["-f"], vec!["/tmp/one", "s3://reports/one"])),
            ),
            (
                "no flag at all",
                &["/tmp/one", "s3://reports/one"],
                &["-f"],
                Some((Vec::new(), vec!["/tmp/one", "s3://reports/one"])),
            ),
            (
                "the command's own stream operand",
                &["-", "hdfs:///tmp/one"],
                &[],
                Some((Vec::new(), vec!["-", "hdfs:///tmp/one"])),
            ),
            (
                "a flag another command takes",
                &["-r", "/tmp/one", "s3://reports/one"],
                &["-f"],
                None,
            ),
            (
                "two flags, one of them the command's",
                &["-f", "-p", "/tmp/one", "s3://reports/one"],
                &["-f"],
                None,
            ),
        ];
        for (case, operands, allowed, expected) in rows {
            let words: Vec<String> = operands.iter().map(|word| (*word).to_string()).collect();
            let split = split_flags(&words, allowed);
            assert_eq!(
                split, expected,
                "case {case:?}: expected {expected:?}, got {split:?}"
            );
        }
        assert!(
            HADOOP_PROGRAMS.contains(&"hadoop") && HADOOP_PROGRAMS.contains(&"hdfs"),
            "the distribution is invoked by both of the names it ships"
        );
    }
}

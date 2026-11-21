// SPDX-License-Identifier: CC0-1.0

//! The application an environment RUNS: the files of the version deployed to it,
//! the log that application's own code writes, and what it answers a request
//! with.
//!
//! `CreateEnvironment$VersionLabel` is "the name of the application version to
//! deploy" and `UpdateEnvironment` "deploys the named application version to the
//! environment", so an environment's deployment has observable consequences: the
//! application's own log lines (which the environment information a
//! `RequestEnvironmentInfo` compiles carries beside this service's activity log)
//! and the content it serves at the environment's own address.
//!
//! Both come from RUNNING the deployed code, never from anything this emulator
//! makes up: the version's `SourceBundle` is downloaded from object storage
//! through the one bounded reader this crate already has
//! ([`super::download_caller_archive`]), the archive is read by the archive
//! reader, and the entry point it carries is evaluated by the embedded engine
//! ([`super::application_runtime`]). A bundle with no entry point this emulator
//! has an engine for leaves the environment serving the FILES the bundle carries
//! and writing no application log at all, rather than output nobody produced.
//!
//! The bundle is read once, when the VERSION is created, and kept on the version
//! record: an environment deploys a version by label, so every environment
//! running that label runs the same files, and the read happens where a create
//! can reach object storage.

use super::*;

/// The archive reader's entry is a `Read`, and the trait has to be in scope for
/// `read_to_end` ([`ApplicationBundle::read`]).
use ::std::io::Read as _;

/// How many entries of one source bundle are kept.
///
/// A bound on CALLER content: the bundle's files are held in this process for as
/// long as the version exists, so the count is capped rather than trusted.
pub const MAX_BUNDLE_FILES: usize = 64;

/// How many bytes of ONE file of the bundle are kept.
pub const MAX_BUNDLE_FILE_BYTES: usize = 512 * 1024;

/// How many bytes of the bundle are kept in total, over every file — the bound on
/// the PRODUCT the count bound cannot give on its own.
pub const MAX_BUNDLE_BYTES: usize = 2 * 1024 * 1024;

/// How many entries of one source bundle are EXAMINED.
///
/// A bound on the WORK the read does, beside the bound on what it keeps
/// ([`MAX_BUNDLE_FILES`]): an archive may declare any number of entries, and an
/// entry that does not fit is skipped without being kept, so without this an
/// archive of many entries would cost one decompression each however little of it
/// is stored.
pub const MAX_BUNDLE_ENTRIES: usize = 4 * MAX_BUNDLE_FILES;

/// The file names an Elastic Beanstalk Node.js platform starts a deployed
/// application from, in the order it looks for them — after the `main` of the
/// bundle's `package.json`, which comes first. (Elastic Beanstalk's Node.js
/// platform documentation: the platform runs the `main` a `package.json` names,
/// and otherwise `app.js`.)
const ENTRY_POINT_CANDIDATES: &[&str] = &["app.js", "server.js", "index.js", "main.js"];

/// The file a request for the application's root is served from when the bundle
/// carries no entry point this emulator can run: the document name a web server
/// serves a directory's root from.
const ROOT_DOCUMENT: &str = "index.html";

/// What the archive IS, for the log line a read that learned nothing leaves
/// behind. See [`super::download_caller_archive`].
const BUNDLE_SUBJECT: &str = "the source bundle an environment runs the application from";

/// The configuration-option namespace whose settings are the deployed
/// application's own environment variables: `aws:elasticbeanstalk:application:
/// environment`, which Elastic Beanstalk's configuration-option reference
/// documents as the environment properties passed to the application.
pub const NAMESPACE_APPLICATION_ENVIRONMENT: &str = "aws:elasticbeanstalk:application:environment";

/// The variable Elastic Beanstalk sets to the port its proxy forwards to, which
/// is the port a deployed application listens on.
const PORT_VARIABLE: &str = "PORT";

/// The value of that variable on the platforms this service publishes.
const PORT_VALUE: &str = "8080";

/// The log file an application's standard output goes to on an Elastic Beanstalk
/// instance, as the compiled environment information names it.
pub const APPLICATION_STDOUT_LOG: &str = "/var/log/web.stdout.log";

/// The log file this service's own activity goes to on that instance, which is
/// the other file the compiled environment information carries.
pub const ACTIVITY_LOG: &str = "/var/log/eb-activity.log";

/// The rule the compiled information draws above and below each file name, so a
/// bundle of several log files says which file each section came from.
pub const LOG_SECTION_RULE: &str = "-------------------------------------";

/// How many deployed applications this emulator runs AT ONCE.
///
/// An EMULATOR bound: each run holds the engine's own heap
/// ([`super::application_runtime`]) on a blocking thread, so this is what bounds
/// what those runs can hold and occupy however many requests arrive together.
/// What it bounds is the PRODUCT: this many runs, each inside the per-run memory
/// ceiling its engine is given ([`super::python_application`]'s and
/// [`super::application_runtime`]'s) and on a stack of
/// [`PYTHON_APPLICATION_STACK_BYTES`], is the most this process holds for
/// deployed applications at any moment.
const MAX_CONCURRENT_RUNS: usize = 6;

/// How long a request waits for one of those run slots before it is answered as
/// an application that produced nothing, rather than queueing behind other
/// callers' code.
///
/// Long enough to wait out part of a run already in flight: one run of a deployed
/// application that starts a server of its own imports the standard library that
/// server is built on into a fresh interpreter, which is seconds of work
/// ([`super::python_application`]), so a wait shorter than that would drop a
/// request that only arrived while another was being served.
const RUN_PERMIT_WAIT: Duration = Duration::from_secs(15);

/// The run slots [`MAX_CONCURRENT_RUNS`] declares, over every environment of this
/// process: the bound is on what the ENGINE may hold at once, which is a property
/// of the process rather than of one environment or one request.
fn application_runs() -> &'static ::tokio::sync::Semaphore {
    static RUNS: ::std::sync::OnceLock<::tokio::sync::Semaphore> = ::std::sync::OnceLock::new();
    RUNS.get_or_init(|| ::tokio::sync::Semaphore::new(MAX_CONCURRENT_RUNS))
}

/// The files one application version's source bundle carries: what an environment
/// that deploys the version runs and serves.
#[derive(Debug, Clone, Default)]
pub struct ApplicationBundle {
    /// The files of the archive, by their path inside it.
    pub files: BTreeMap<String, Vec<u8>>,
}

impl ApplicationBundle {
    /// The bundle `bytes` carries, inside this module's bounds: a `.zip` is what
    /// an Elastic Beanstalk source bundle is, so the archive FORMAT is read by
    /// the archive reader rather than by this emulator.
    ///
    /// A directory entry, an entry whose name escapes the archive and an entry
    /// past [`MAX_BUNDLE_FILE_BYTES`] are left out: the bundle is the files that
    /// could be read, which is what an environment then runs and serves.
    ///
    /// Every bound here is on what THIS process does with a caller's archive:
    /// [`MAX_BUNDLE_ENTRIES`] entries are examined, an entry whose own declared
    /// size does not fit is skipped before it is inflated at all, and the
    /// inflated bytes are charged against [`MAX_BUNDLE_BYTES`] whether or not the
    /// entry is kept — so the work one archive costs is bounded by that budget
    /// rather than multiplied by how many entries it declares. The read is
    /// synchronous and CPU-bound, so its caller performs it off the async
    /// executor ([`read_application_bundle`]).
    pub fn read(bytes: &[u8]) -> Self {
        let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        let Ok(mut archive) = ::zip::ZipArchive::new(::std::io::Cursor::new(bytes)) else {
            ::tracing::warn!(
                "an application version's source bundle is not an archive this emulator can \
                 read, so an environment deploying it runs none of its files",
            );
            return Self::default();
        };
        let mut budget = MAX_BUNDLE_BYTES;
        for index in 0..archive.len().min(MAX_BUNDLE_ENTRIES) {
            if files.len() >= MAX_BUNDLE_FILES || budget == 0 {
                break;
            }
            let Ok(mut entry) = archive.by_index(index) else {
                continue;
            };
            if entry.is_dir() {
                continue;
            }
            // The archive reader reports a name as an `Option`, because a name
            // that escapes the archive is not a path a file may be read into.
            let Some(name) = entry
                .enclosed_name()
                .and_then(|name| name.to_str().map(str::to_string))
            else {
                continue;
            };
            let room = budget.min(MAX_BUNDLE_FILE_BYTES);
            // The size the archive DECLARES for the entry: a caller's number, so
            // it is trusted for nothing but skipping an entry that cannot fit
            // anyway — which is what keeps an archive of entries that do not fit
            // from costing a decompression each.
            if entry.size() > room as u64 {
                continue;
            }
            let mut body = Vec::new();
            // One byte past the room left, so a file that does not fit is left
            // out rather than stored truncated.
            let read = ::std::io::Read::take(&mut entry, room as u64 + 1).read_to_end(&mut body);
            // What was inflated is spent whether or not it is kept, so the whole
            // read costs at most the budget.
            budget = budget.saturating_sub(body.len());
            if read.is_err() || body.len() > room {
                continue;
            }
            files.insert(name, body);
        }
        Self { files }
    }

    /// The source of the bundle's file `path`, when it carries one and it is
    /// text.
    pub fn source(&self, path: &str) -> Option<&str> {
        let source = self.files.get(path)?;
        ::std::str::from_utf8(source).ok()
    }

    /// Whether this bundle carries nothing at all, which is what a bundle no read
    /// reached and one no archive reader would read both leave.
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

/// What a deployed bundle is STARTED from, which is also which engine of this
/// process RUNS it.
///
/// An Elastic Beanstalk platform starts the application with the command the
/// bundle's own `Procfile` gives its web process, and otherwise with its own
/// default command — which starts a file of the deployment in the language of that
/// platform: a Node.js platform the `main` of the bundle's `package.json` (and
/// otherwise `app.js`), a Python platform the module its `WSGIPath` names. A
/// bundle carrying no such file is one this emulator starts no application from,
/// and its environment serves the bundle's FILES instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplicationEntry {
    /// The JavaScript file the platform starts, run by the embedded JavaScript
    /// engine ([`super::application_runtime`]).
    JavaScript(String),
    /// The WSGI entry point the platform's own server serves, run by the embedded
    /// Python interpreter ([`super::python_application`]).
    Python(PythonEntry),
    /// The Python PROGRAM the start command runs, which starts a server of its
    /// own ([`super::python_program`]).
    PythonProgram(PythonProgram),
}

/// What the bundle `bundle` deployed on a platform of category
/// `platform_category`, under the configuration `option_settings`, is started
/// from — or `None` for a deployment this emulator starts no application from.
///
/// The bundle's own `Procfile` decides first, where it carries one: the command it
/// gives the web process is the command the platform starts the application with,
/// whatever the platform's own default command would have started
/// ([`started_by`]).
///
/// Otherwise the PLATFORM decides, where this emulator embeds an engine for that
/// platform's own language: a Python platform starts the module its `WSGIPath`
/// names ([`python_entry`]) and a Node.js platform the file a Node.js platform
/// starts, so a bundle carrying files of both languages is run by the engine of
/// the platform it was deployed to. For a platform whose own language this
/// emulator embeds no engine for — and for a custom platform version, whose
/// language this service knows nothing about — the entry point is whichever of the
/// two the BUNDLE carries, looked for in one order so that one deployment has one
/// entry point: such an environment runs the application its deployment carries or
/// none.
#[must_use]
pub fn application_entry(
    bundle: &ApplicationBundle,
    platform_category: Option<&str>,
    option_settings: &[ConfigurationOptionSetting],
) -> Option<ApplicationEntry> {
    if let Some(command) = procfile_command(bundle, WEB_PROCESS) {
        return started_by(bundle, &command);
    }
    match platform_category {
        Some(PLATFORM_CATEGORY_PYTHON) => {
            return python_entry(bundle, option_settings).map(ApplicationEntry::Python)
        }
        Some(PLATFORM_CATEGORY_NODEJS) => {
            return javascript_entry_of(&bundle.files).map(ApplicationEntry::JavaScript)
        }
        _ => {}
    }
    javascript_entry_of(&bundle.files)
        .map(ApplicationEntry::JavaScript)
        .or_else(|| python_entry(bundle, option_settings).map(ApplicationEntry::Python))
}

/// What the start command `command` starts out of `bundle` — and `None` for a
/// command this emulator embeds no engine for, which starts nothing.
///
/// A command states both the engine and what it runs, so the three forms are read
/// in one order and the first one the deployment answers is the application: the
/// Python program the interpreter is handed ([`python_program`]), the WSGI
/// application a Python server command serves ([`python_wsgi_specified`]), and the
/// JavaScript file a Node.js command runs ([`javascript_program`]).
fn started_by(bundle: &ApplicationBundle, command: &str) -> Option<ApplicationEntry> {
    python_program(bundle, command)
        .map(ApplicationEntry::PythonProgram)
        .or_else(|| python_wsgi_specified(bundle, command).map(ApplicationEntry::Python))
        .or_else(|| javascript_program(bundle, command).map(ApplicationEntry::JavaScript))
}

/// The file names a command runs a Node.js program with, under either name a
/// platform's own start command spells the runtime.
const NODEJS_RUNTIMES: &[&str] = &["node", "nodejs"];

/// The JavaScript file `command` runs out of `bundle`, or `None` when it runs
/// none: the first path the runtime is handed past its own options, which has to
/// be a file of the deployment the embedded JavaScript engine can run.
fn javascript_program(bundle: &ApplicationBundle, command: &str) -> Option<String> {
    let mut tokens = command.split_whitespace();
    let runtime = tokens.next()?;
    let runtime = runtime.rsplit('/').next().unwrap_or(runtime);
    if !NODEJS_RUNTIMES.contains(&runtime) {
        return None;
    }
    let file = tokens.find(|token| !token.starts_with('-'))?;
    let file = file.trim_start_matches("./");
    (JAVASCRIPT_SUFFIXES
        .iter()
        .any(|suffix| file.ends_with(suffix))
        && bundle.files.contains_key(file))
    .then(|| file.to_string())
}

/// The suffixes of a file the embedded JavaScript engine runs.
const JAVASCRIPT_SUFFIXES: &[&str] = &[".js", ".mjs", ".cjs"];

/// The `platform_category` of the platforms whose own language this emulator
/// embeds an engine for, as the published platform catalog states it
/// ([`super::SOLUTION_STACKS`]).
pub const PLATFORM_CATEGORY_PYTHON: &str = "Python";
/// See [`PLATFORM_CATEGORY_PYTHON`].
pub const PLATFORM_CATEGORY_NODEJS: &str = "Node.js";

/// The application one environment is running: the version it deploys, the files
/// of that version's bundle, what its platform starts them from and the process
/// environment the environment's configuration gives it.
#[derive(Debug, Clone)]
pub struct DeployedApplication {
    /// The version label the environment reports running.
    pub version_label: String,
    pub bundle: ApplicationBundle,
    /// What the environment's platform starts that bundle from
    /// ([`application_entry`]), and `None` for a bundle it starts nothing from.
    pub entry: Option<ApplicationEntry>,
    /// The environment variables the application reads out of its own process
    /// environment.
    pub environment: Vec<(String, String)>,
}

/// What serving ONE request produced: the response the client is answered with,
/// and what the application wrote to its own output while answering it.
///
/// Both come out of the same run, so an environment that streams its instance logs
/// delivers exactly the lines the request that produced them did
/// ([`super::stream_instance_logs`]).
#[derive(Debug, Clone, Default)]
pub struct Served {
    /// The response the client reads, and `None` for an address the application
    /// answered nothing at and the bundle carries no file for.
    pub response: Option<GuestResponse>,
    /// The lines the application wrote while answering the request, in order.
    pub output: Vec<String>,
}

impl DeployedApplication {
    /// START the application: evaluate its entry point with no request to serve,
    /// which is what writes its start-up log.
    ///
    /// `None` when the bundle carries no entry point this emulator has an engine
    /// for — such an environment's application writes no log of its own.
    pub async fn start(&self) -> Option<GuestRun> {
        self.run(None).await
    }

    /// What STARTING the application wrote to its own output: the lines the
    /// instance's standard-output log file carries once the deployment has
    /// started the application, which is what an environment streaming its
    /// instance logs delivers.
    ///
    /// Empty for an environment running no application this emulator starts — no
    /// output means no event, rather than a line nobody wrote.
    pub async fn startup_output(&self) -> Vec<String> {
        let Some(run) = self.start().await else {
            return Vec::new();
        };
        application_log_lines(&run)
            .into_iter()
            .map(str::to_string)
            .collect()
    }

    /// What the application answers `request` with, and what it WROTE while
    /// answering it: the response its own request handler ends, and otherwise the
    /// file of the bundle the request addresses.
    ///
    /// The response is `None` when the application answers nothing at that address
    /// and the bundle carries no file for it either, which the plane then answers
    /// as a path nothing is served at. The output is the lines the instance's
    /// standard-output log file gains from this one request, which is what an
    /// environment streaming its instance logs delivers for it.
    pub async fn serve(&self, request: &GuestRequest) -> Served {
        let mut served = Served::default();
        if let Some(run) = self.run(Some(request)).await {
            served.output = run
                .served
                .split_inclusive('\n')
                .map(|line| line.strip_suffix('\n').unwrap_or(line))
                .filter(|line| !line.is_empty())
                .map(str::to_string)
                .collect();
            if let Some(response) = run.response {
                served.response = Some(response);
                return served;
            }
            if let Some(failure) = run.failure {
                ::tracing::warn!(
                    detail = %failure,
                    path = %request.path,
                    "the deployed application did not answer the request it was handed",
                );
            }
        }
        served.response = self.served_file(&request.path);
        served
    }

    /// Run the entry point on a blocking thread, handing it `request` when there
    /// is one: the engine is synchronous, and its interrupt handler is what stops
    /// code with no exit.
    ///
    /// One of [`MAX_CONCURRENT_RUNS`] slots is held for the run, so what the
    /// engine holds at once is bounded however many requests arrive together; a
    /// request that waits [`RUN_PERMIT_WAIT`] for one is answered as an
    /// application that produced nothing rather than queued behind other
    /// callers' code.
    async fn run(&self, request: Option<&GuestRequest>) -> Option<GuestRun> {
        match self.entry.as_ref()? {
            ApplicationEntry::JavaScript(file) => self.run_javascript(file, request).await,
            ApplicationEntry::Python(entry) => self.run_python(entry, request).await,
            ApplicationEntry::PythonProgram(program) => self.run_program(program, request).await,
        }
    }

    /// [`Self::run`] for a bundle the JavaScript engine runs.
    ///
    /// The permit is held by the RUN rather than by the waiting task: a blocking
    /// task cannot be cancelled, so a request that stopped waiting for its run
    /// would otherwise give the slot back while the engine was still holding its
    /// own heap on that thread.
    async fn run_javascript(
        &self,
        file: &str,
        request: Option<&GuestRequest>,
    ) -> Option<GuestRun> {
        let entry = self.bundle.source(file)?.to_string();
        let environment = self.environment.clone();
        let request = request.cloned();
        let permit = ::tokio::time::timeout(RUN_PERMIT_WAIT, application_runs().acquire()).await;
        let permit = match permit {
            Ok(Ok(permit)) => permit,
            _ => {
                ::tracing::warn!(
                    "the deployed application was not run: every run slot this emulator holds \
                     was taken",
                );
                return None;
            }
        };
        let run = ::tokio::task::spawn_blocking(move || {
            // Held for as long as the run lasts, whoever is still waiting for it.
            let _permit = permit;
            run_application(&entry, &environment, request.as_ref())
        })
        .await;
        match run {
            Ok(run) => Some(run),
            Err(failure) => {
                ::tracing::warn!(
                    detail = %failure,
                    "the thread the deployed application ran on did not finish",
                );
                None
            }
        }
    }

    /// [`Self::run`] for a bundle the Python interpreter runs: the interpreter is
    /// synchronous and its stop lands at the next traced event, so the run happens
    /// on a THREAD of its own with a stack of its own
    /// ([`PYTHON_APPLICATION_STACK_BYTES`]) and the task waits for it only as long
    /// as the engine's own spans allow ([`PYTHON_APPLICATION_HOST_WAIT`]).
    ///
    /// One of [`MAX_CONCURRENT_RUNS`] slots is held for the run, by the THREAD
    /// rather than by the waiting task, so what the engines hold at once is
    /// bounded even where a request stopped waiting for its run.
    async fn run_python(
        &self,
        entry: &PythonEntry,
        request: Option<&GuestRequest>,
    ) -> Option<GuestRun> {
        let modules = python_modules(&self.bundle);
        if modules.is_empty() {
            return None;
        }
        let entry = entry.clone();
        let environment = self.environment.clone();
        let request = request.cloned();
        run_on_interpreter(move || {
            run_python_application(&entry, &modules, &environment, request.as_ref())
        })
        .await
    }

    /// [`Self::run`] for a deployment the platform starts as a PROGRAM: the same
    /// interpreter, the same bounds and the same thread of its own
    /// ([`Self::run_python`]) — what differs is that the program starts a server
    /// of its own and the request is handed to THAT server
    /// ([`super::python_program`]).
    async fn run_program(
        &self,
        program: &PythonProgram,
        request: Option<&GuestRequest>,
    ) -> Option<GuestRun> {
        let source = self.bundle.source(&program.file)?.to_string();
        let modules = python_modules(&self.bundle);
        let program = program.clone();
        let environment = self.environment.clone();
        let request = request.cloned();
        run_on_interpreter(move || {
            run_python_program(&program, &source, &modules, &environment, request.as_ref())
        })
        .await
    }

    /// The file of the bundle a request path addresses: the path with its leading
    /// separator and any query string removed, and the root document for a
    /// request for the application's root.
    fn served_file(&self, path: &str) -> Option<GuestResponse> {
        let path = path.split(['?', '#']).next().unwrap_or(path);
        let name = path.trim_start_matches('/');
        let name = if name.is_empty() { ROOT_DOCUMENT } else { name };
        let body = self.bundle.files.get(name)?;
        Some(GuestResponse {
            status: 200,
            headers: vec![(
                "content-type".to_string(),
                document_media_type(name).to_string(),
            )],
            body: body.clone(),
        })
    }
}

/// Perform `run` on a thread of its OWN, with a stack of its own
/// ([`PYTHON_APPLICATION_STACK_BYTES`]): the Python interpreter is synchronous and
/// its stop lands at the next traced event, so the task waits for the run only as
/// long as the engine's own spans allow ([`PYTHON_APPLICATION_HOST_WAIT`]).
///
/// One of [`MAX_CONCURRENT_RUNS`] slots is held for the run, by the THREAD rather
/// than by the waiting task, so what the engines hold at once is bounded even
/// where a request stopped waiting for its run.
///
/// `None` for a run this emulator did not perform at all — one it had no slot or
/// no thread for, and one that had still not answered a grace period past the
/// spans the engine allows it.
async fn run_on_interpreter<R>(run: R) -> Option<GuestRun>
where
    R: FnOnce() -> GuestRun + Send + 'static,
{
    let permit = ::tokio::time::timeout(RUN_PERMIT_WAIT, application_runs().acquire()).await;
    let permit = match permit {
        Ok(Ok(permit)) => permit,
        _ => {
            ::tracing::warn!(
                "the deployed application was not run: every run slot this emulator holds \
                 was taken",
            );
            return None;
        }
    };
    let (sender, receiver) = ::tokio::sync::oneshot::channel();
    let spawned = ::std::thread::Builder::new()
        .name("eb-python-application".to_string())
        .stack_size(PYTHON_APPLICATION_STACK_BYTES)
        .spawn(move || {
            // Held for as long as the run lasts, whoever is still waiting for it.
            let _permit = permit;
            let _ = sender.send(run());
        });
    if let Err(error) = spawned {
        ::tracing::warn!(
            detail = %error,
            "the thread the deployed application runs on could not be started",
        );
        return None;
    }
    match ::tokio::time::timeout(PYTHON_APPLICATION_HOST_WAIT, receiver).await {
        Ok(Ok(run)) => Some(run),
        // The run has still not answered a grace period past the spans the engine
        // itself allows, which it has by then been stopped on: it is given up on
        // and reported as an application that produced nothing.
        _ => {
            ::tracing::warn!(
                "the deployed application's own run did not answer inside the spans this \
                 emulator allows it",
            );
            None
        }
    }
}

/// The lines one run of the application wrote to its own log: its standard output
/// then its standard error, with the empty trailing line a text stream ends in
/// dropped — byte for byte as the application wrote them, so non-ASCII text
/// survives unchanged.
pub fn application_log_lines(run: &GuestRun) -> Vec<&str> {
    [run.stdout.as_str(), run.stderr.as_str()]
        .into_iter()
        .flat_map(|stream| stream.split_inclusive('\n'))
        .map(|line| line.strip_suffix('\n').unwrap_or(line))
        .filter(|line| !line.is_empty())
        .collect()
}

/// The environment variables the deployed application reads: every setting of the
/// environment-properties namespace, plus the port the platform tells the
/// application to listen on.
pub fn application_environment(
    option_settings: &[ConfigurationOptionSetting],
) -> Vec<(String, String)> {
    let mut environment: Vec<(String, String)> =
        vec![(PORT_VARIABLE.to_string(), PORT_VALUE.to_string())];
    for setting in option_settings {
        if setting.namespace.as_deref() != Some(NAMESPACE_APPLICATION_ENVIRONMENT) {
            continue;
        }
        let (Some(name), Some(value)) = (setting.option_name.as_deref(), setting.value.as_deref())
        else {
            continue;
        };
        environment.push((name.to_string(), value.to_string()));
    }
    environment
}

/// Read the source bundle at `location` out of object storage, in process: the
/// files an environment that deploys this version runs.
///
/// The archive is READ on a blocking thread: inflating a caller's archive is
/// synchronous, CPU-bound work ([`ApplicationBundle::read`], which bounds what it
/// costs), so it never occupies an async worker of this process.
///
/// `None` when there is nothing to read — no object the read reached, or an
/// archive the archive reader will not read. A version then carries no files, and
/// an environment deploying it runs no application of its own rather than one
/// whose output was invented.
pub async fn read_application_bundle(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    location: &S3Location,
) -> Option<ApplicationBundle> {
    let bytes = match download_caller_archive(ctx, region, location, BUNDLE_SUBJECT).await {
        DownloadedArchive::Bytes(bytes) => bytes,
        DownloadedArchive::NotReadable | DownloadedArchive::NotRead => return None,
    };
    let bundle = ::tokio::task::spawn_blocking(move || ApplicationBundle::read(&bytes))
        .await
        .unwrap_or_else(|failure| {
            ::tracing::warn!(
                detail = %failure,
                "the thread the source bundle was read on did not finish, so the version \
                 carries none of its files",
            );
            ApplicationBundle::default()
        });
    (!bundle.is_empty()).then_some(bundle)
}

/// The media type a bundle file is served under, by the extension it carries —
/// the handful a deployed web application's own documents use. Anything else is
/// served as the bytes it is.
fn document_media_type(name: &str) -> &'static str {
    match name.rsplit_once('.').map(|(_, extension)| extension) {
        Some("html" | "htm") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("json") => "application/json",
        Some("txt" | "log") => "text/plain; charset=utf-8",
        Some("svg") => "image/svg+xml",
        _ => "application/octet-stream",
    }
}

/// The file the Node.js platform starts the application from: the `main` the
/// bundle's `package.json` names, when the bundle carries that file, and
/// otherwise the first of [`ENTRY_POINT_CANDIDATES`] it carries.
///
/// Only a JavaScript file answers here: JavaScript is what the embedded engine of
/// that platform runs, so a bundle whose entry point is a module of another
/// language is started by the engine for that language instead
/// ([`application_entry`]).
fn javascript_entry_of(files: &BTreeMap<String, Vec<u8>>) -> Option<String> {
    let main = files
        .get("package.json")
        .and_then(|manifest| ::std::str::from_utf8(manifest).ok())
        .and_then(|manifest| ::serde_json::from_str::<::serde_json::Value>(manifest).ok())
        .and_then(|manifest| {
            manifest
                .get("main")
                .and_then(::serde_json::Value::as_str)
                .map(str::to_string)
        })
        .map(|main| main.trim_start_matches("./").to_string())
        .filter(|main| files.contains_key(main));
    main.into_iter()
        .chain(
            ENTRY_POINT_CANDIDATES
                .iter()
                .filter(|candidate| files.contains_key(**candidate))
                .map(|candidate| (*candidate).to_string()),
        )
        .find(|name| name.ends_with(".js"))
}

/// The content the environment information compiled from one of an environment's
/// instances carries: the log FILES that instance holds.
///
/// "Compiles the last lines from the application server log files of every Amazon
/// EC2 instance in your environment" — so the content is this service's own
/// activity log and the standard output of the application the environment runs,
/// each introduced by the path it has on the instance, the way a compiled bundle
/// names the file each section came from.
///
/// An environment running no application of its own has one log file, and the
/// content is that file: there is no second section to name, so nothing is
/// written about a file that does not exist. `max_lines` has already been applied
/// to `activity`; it is applied to the application's lines here, since it is the
/// tail of each log that a `tail` compiles.
pub fn compiled_environment_information(
    activity: &str,
    application: &[&str],
    max_lines: Option<usize>,
) -> String {
    if application.is_empty() {
        return activity.to_string();
    }
    let dropped = application
        .len()
        .saturating_sub(max_lines.unwrap_or(application.len()));
    let application = application[dropped..].join("\n");
    format!(
        "{rule}\n{ACTIVITY_LOG}\n{rule}\n{activity}\
         {rule}\n{APPLICATION_STDOUT_LOG}\n{rule}\n{application}\n",
        rule = LOG_SECTION_RULE,
    )
}

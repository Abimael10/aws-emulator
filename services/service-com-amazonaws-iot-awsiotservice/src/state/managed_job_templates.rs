// SPDX-License-Identifier: CC0-1.0

//! The AWS-MANAGED job template catalog this service serves: the reference data
//! `ListManagedJobTemplates` pages over and `DescribeManagedJobTemplate` reads one
//! entry of.
//!
//! No operation of this service creates, updates or deletes one — the whole surface is
//! "Returns a list of managed job templates"
//! (`smithy_query(query="operation",operation="ListManagedJobTemplates")`) and "View
//! details of a managed job template"
//! (`smithy_query(query="operation",operation="DescribeManagedJobTemplate")`) — so the
//! collection is PUBLISHED data: it is seeded complete when the store is first touched
//! ([`ManagedJobTemplateCatalog::default`]) and only ever read. A reader therefore
//! changes nothing, and two readers of one entry cannot disagree.
//!
//! Every member of an entry is the SERVICE's published text, identical for every caller
//! in every account and region: the template's description, the one step its document
//! runs, and the `documentParameters` that document leaves open — "a map of key-value
//! pairs containing the patterns that need to be replaced in a managed template job
//! document schema" (`smithy_query(query="shape",shape="DocumentParameter")`). The
//! document is BUILT from that parameter table ([`CatalogRow::document`]), so a
//! parameter and the `${aws:iot:parameter:<key>}` placeholder it fills cannot drift
//! apart.
//!
//! Each entry is a REMOTE ACTION the AWS IoT Device Client performs. Two facts follow
//! from the action's TYPE and are not written out per row: the step's action name is the
//! template name without its `AWS-` prefix, and a `runHandler` step declares the two
//! parameters that are about running a handler (where the handlers live and which user
//! to run one as) while a `runCommand` step declares only the user, because it runs no
//! handler.
//!
//! `AWS-Reboot` is the entry the model itself names
//! (`smithy_query(query="doc",shape="DescribeManagedJobTemplateResponse",member="templateName")`:
//! "The unique name of a managed template, such as AWS-Reboot"); the rest of the table
//! is the published set of the device client's remote actions — running a command,
//! rebooting, downloading a file, and installing or removing applications and starting,
//! stopping or restarting system services. [`CATALOG`] holds them in the order the
//! service publishes them, which is the order a listing reports.

use crate::ops::types::{DocumentParameter, ManagedJobTemplateSummary};

/// The `templateArn` of the managed job template `template_name` at
/// `template_version`, as served for a request signed for `region`.
///
/// The ACCOUNT segment is EMPTY: a managed template is AWS-managed data rather than
/// this account's, exactly as the provider-service catalog of a sibling service spells
/// its own published ARNs. The resource is `jobtemplate/<name>:<version>` — the
/// resource type AWS IoT job template ARNs use, with the version the ARN addresses
/// after it, so the ARN of one version names that version and not the template as a
/// whole.
pub fn managed_job_template_arn(
    region: &str,
    template_name: &str,
    template_version: &str,
) -> String {
    format!("arn:aws:iot:{region}::jobtemplate/{template_name}:{template_version}")
}

/// One VERSION of a managed job template: everything that is about the version rather
/// than about the template.
#[derive(Debug, Clone)]
pub struct ManagedJobTemplateVersion {
    /// `templateVersion`, a `ManagedTemplateVersion` (`^[1-9]+.[0-9]+$`).
    pub template_version: String,
    /// `document` — "The document schema for a managed job template".
    pub document: String,
    /// `documentParameters` — the patterns of that document a caller fills in when it
    /// creates a job from the template.
    pub document_parameters: Vec<DocumentParameter>,
}

/// One managed job template as this service publishes it: the facts that hold for the
/// template, and every version of it, in published order.
#[derive(Debug, Clone)]
pub struct ManagedJobTemplate {
    /// `templateName` — "The unique Name for a managed template".
    pub template_name: String,
    /// `description` — "The description for a managed template".
    pub description: String,
    /// `environments` — "A list of environments that are supported with the managed job
    /// template".
    pub environments: Vec<String>,
    /// Every published version of this template, in the order the service publishes
    /// them.
    ///
    /// Ordered rather than a map, because `ListManagedJobTemplates` is `@paginated`
    /// and, with a `templateName`, lists "the versions of the managed job templates that
    /// have the specified template name" — a page boundary over versions is only
    /// meaningful over a stable order.
    pub versions: Vec<ManagedJobTemplateVersion>,
}

impl ManagedJobTemplate {
    /// The version a request that names none is answered with: "If not specified, the
    /// pre-defined default version is returned"
    /// (`DescribeManagedJobTemplate$templateVersion`), which is the LATEST version
    /// published for the template.
    ///
    /// Ordered by the version's two numbers rather than by its text, so `1.10` is later
    /// than `1.9`. `None` only for a template with no published version, which
    /// [`managed_job_template_catalog`] never builds.
    pub fn default_version(&self) -> Option<&ManagedJobTemplateVersion> {
        self.versions
            .iter()
            .max_by_key(|version| version_order(&version.template_version))
    }

    /// The version `requested` names, or the default version when it names none;
    /// `None` when this template publishes no such version.
    pub fn version(&self, requested: Option<&str>) -> Option<&ManagedJobTemplateVersion> {
        match requested {
            Some(wanted) => self
                .versions
                .iter()
                .find(|version| version.template_version == wanted),
            None => self.default_version(),
        }
    }

    /// One (template, version) pair as the modelled `ManagedJobTemplateSummary` a
    /// listing carries.
    ///
    /// Every member is read off this catalog entry, and the ARN is minted for the
    /// region the request was signed for, so what a listing reports and what
    /// `DescribeManagedJobTemplate` reports for the same pair cannot disagree.
    pub fn summary(
        &self,
        region: &str,
        version: &ManagedJobTemplateVersion,
    ) -> ManagedJobTemplateSummary {
        ManagedJobTemplateSummary {
            template_arn: Some(managed_job_template_arn(
                region,
                &self.template_name,
                &version.template_version,
            )),
            template_name: Some(self.template_name.clone()),
            description: Some(self.description.clone()),
            environments: Some(self.environments.clone()),
            template_version: Some(version.template_version.clone()),
        }
    }
}

/// The two numbers of a `ManagedTemplateVersion` (`^[1-9]+.[0-9]+$`), which is the
/// order versions of one template are published in. A field that is not a number sorts
/// as zero, so the comparison is total over any text the catalog holds.
fn version_order(template_version: &str) -> (u32, u32) {
    let (major, minor) = template_version
        .split_once('.')
        .unwrap_or((template_version, "0"));
    (
        major.parse().unwrap_or_default(),
        minor.parse().unwrap_or_default(),
    )
}

/// The managed job template catalog as the store holds it.
///
/// A struct of its own, rather than a bare collection on the state struct, so that the
/// state's derived `Default` — what the engine builds the store with on first touch —
/// seeds the AWS-managed collection COMPLETE while every customer-owned collection
/// beside it starts empty.
#[derive(Debug, Clone)]
pub struct ManagedJobTemplateCatalog {
    /// Every published template, in PUBLISHED order: the order the service lists the
    /// catalog in, which is a property of the catalog rather than of a reader, so a
    /// listing and a paged walk of it report the same sequence.
    entries: Vec<ManagedJobTemplate>,
}

impl Default for ManagedJobTemplateCatalog {
    fn default() -> Self {
        Self {
            entries: managed_job_template_catalog(),
        }
    }
}

impl ManagedJobTemplateCatalog {
    /// The template `template_name` names, or `None` when the catalog publishes none
    /// under that name. The name is the template's identity and the `@httpLabel` of
    /// `/managed-job-templates/{templateName}`.
    pub fn template(&self, template_name: &str) -> Option<&ManagedJobTemplate> {
        self.entries
            .iter()
            .find(|template| template.template_name == template_name)
    }

    /// Every published (template, version) PAIR, in catalog order: the order the
    /// templates are published in, then the order their versions are.
    ///
    /// A pair and not a template, because one entry of a listing is one version of one
    /// template — with a `templateName` the listing is of "the versions of the managed
    /// job templates that have the specified template name".
    pub fn pairs(
        &self,
    ) -> impl Iterator<Item = (&ManagedJobTemplate, &ManagedJobTemplateVersion)> + '_ {
        self.entries.iter().flat_map(|template| {
            template
                .versions
                .iter()
                .map(move |version| (template, version))
        })
    }
}

/// What kind of value a template parameter takes, which fixes the `regex` the catalog
/// publishes as "a regular expression of the patterns that need to be replaced"
/// (`DocumentParameter$regex`).
///
/// Carried as a KIND rather than as a string per parameter so that two parameters that
/// take the same kind of value are published with the same rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ValueKind {
    /// Any text at most this many characters long: the service publishes the bound as
    /// the repetition count of a single-character expression.
    Text(usize),
    /// One or more name tokens separated by whitespace, which is how the device
    /// client's handlers take a list of packages or of system services.
    NameList,
    /// Any text at all, with no bound published for it.
    AnyText,
}

/// The expression the service publishes for a list of name tokens separated by
/// whitespace ([`ValueKind::NameList`]).
const NAME_LIST_REGEX: &str = r"([a-zA-Z0-9+_\-\.]+[\s]?)+";

/// The expression the service publishes for a value it bounds in no way
/// ([`ValueKind::AnyText`]).
const ANY_TEXT_REGEX: &str = "(.*?)";

impl ValueKind {
    /// The `regex` the catalog publishes for a parameter of this kind.
    fn regex(self) -> String {
        match self {
            Self::Text(max) => format!("(.){{0,{max}}}"),
            Self::NameList => NAME_LIST_REGEX.to_string(),
            Self::AnyText => ANY_TEXT_REGEX.to_string(),
        }
    }
}

/// One parameter of a managed template's document: the pattern a caller replaces, what
/// it is for, the kind of value that goes there and the example the catalog illustrates
/// it with.
struct CatalogParameter {
    /// `key` — "Key of the map field containing the patterns that need to be replaced in
    /// a managed template job document schema", a `ParameterKey`
    /// (`^[a-zA-Z0-9_-]+$`).
    key: &'static str,
    /// `description` — what the caller has to put there, as the service words it.
    description: &'static str,
    kind: ValueKind,
    /// `example` — "an example illustrating a pattern that need to be replaced in a
    /// managed template job document schema". `None` for a parameter the service
    /// publishes no example for, which is then absent from the response rather than
    /// invented here.
    example: Option<&'static str>,
    /// `optional` — "Specifies whether a pattern that needs to be replaced in a managed
    /// template job document schema is optional or required".
    optional: bool,
}

impl CatalogParameter {
    /// This parameter as the modelled `DocumentParameter` the catalog publishes.
    fn document_parameter(&self) -> DocumentParameter {
        DocumentParameter {
            key: Some(self.key.to_string()),
            description: Some(self.description.to_string()),
            regex: Some(self.kind.regex()),
            example: self.example.map(str::to_string),
            optional: self.optional,
        }
    }

    /// The placeholder the document carries where this parameter's value goes: the
    /// substitution form a managed template's document is written in,
    /// `${aws:iot:parameter:<key>}`.
    fn placeholder(&self) -> String {
        format!("${{aws:iot:parameter:{}}}", self.key)
    }
}

/// Where the device client's job handlers live on the device, which every `runHandler`
/// template's document carries as its step's `path`. Optional: a device client
/// configured with its own handler directory needs none.
const PATH_TO_HANDLER: CatalogParameter = CatalogParameter {
    key: "pathToHandler",
    description: "Path to handler on the device. If not specified, then device client \
                  will use the current working directory.",
    kind: ValueKind::Text(4096),
    example: Some("/path/to/handler/script"),
    optional: true,
};

/// Which user the step runs as, which every managed template's document carries as its
/// step's `runAsUser`. Optional: the device client otherwise runs it as the user it runs
/// as itself.
const RUN_AS_USER: CatalogParameter = CatalogParameter {
    key: "runAsUser",
    description: "Execute handler as another user. If not specified, then handler is \
                  executed as the same user as device client.",
    kind: ValueKind::Text(256),
    example: Some("user1"),
    optional: true,
};

/// What the one step of a managed template's document DOES, which is the step's `type`.
///
/// The two forms differ in how the template's own parameters reach the step: a handler
/// takes them as positional arguments beside the directory it is found in, and a
/// command takes each one as an input field of its own name.
enum StepAction {
    /// `runHandler` — the step runs one of the device client's job handlers, named here.
    RunHandler(&'static str),
    /// `runCommand` — the step runs the command the caller supplies.
    RunCommand,
}

impl StepAction {
    /// The step's `type`.
    const fn step_type(&self) -> &'static str {
        match self {
            Self::RunHandler(_) => "runHandler",
            Self::RunCommand => "runCommand",
        }
    }

    /// The parameters a template of this action kind declares beside its own: a handler
    /// is found in a directory and run as a user, and a command is only run as a user.
    fn handler_parameters(&self) -> &'static [CatalogParameter] {
        match self {
            Self::RunHandler(_) => RUN_HANDLER_PARAMETERS,
            Self::RunCommand => RUN_COMMAND_PARAMETERS,
        }
    }
}

/// The parameters a `runHandler` template declares beside its own: the directory the
/// handler is found in and the user it runs as.
const RUN_HANDLER_PARAMETERS: &[CatalogParameter] = &[RUN_AS_USER, PATH_TO_HANDLER];

/// The parameters a `runCommand` template declares beside its own: the user the command
/// runs as. No handler is run, so no handler directory is published.
const RUN_COMMAND_PARAMETERS: &[CatalogParameter] = &[RUN_AS_USER];

/// One row of the catalog table: everything that differs between managed templates. The
/// rest of an entry — its ARN, its document, its step's name, the handler parameters it
/// declares — is DERIVED from the row, so no two members of one entry can disagree.
struct CatalogRow {
    template_name: &'static str,
    description: &'static str,
    /// The published version of this template.
    template_version: &'static str,
    /// The environments the template is supported with. An `Environment`
    /// (`^[^\p{C}]+$`).
    environments: &'static [&'static str],
    /// What the template's one step does.
    action: StepAction,
    /// The template's OWN parameters, in the order the document passes them to the
    /// step. The handler parameters follow from the action kind and are not repeated
    /// here.
    parameters: &'static [CatalogParameter],
}

impl CatalogRow {
    /// The name of the one step the template's document runs: the template's name
    /// without the `AWS-` prefix every published template carries.
    fn step_name(&self) -> &'static str {
        self.template_name
            .strip_prefix(TEMPLATE_NAME_PREFIX)
            .unwrap_or(self.template_name)
    }

    /// Every parameter this template declares: its own, then the ones that come with its
    /// action kind.
    fn parameters(&self) -> impl Iterator<Item = &CatalogParameter> {
        self.parameters
            .iter()
            .chain(self.action.handler_parameters().iter())
    }

    /// The template's `document`: one step of the row's action kind, with a
    /// `${aws:iot:parameter:<key>}` placeholder wherever one of the row's own parameters
    /// goes and the user as the step's `runAsUser`.
    ///
    /// Built from the row rather than written out per template, so a document can carry
    /// no placeholder the entry does not declare a parameter for.
    fn document(&self) -> String {
        let mut input = ::serde_json::Map::new();
        match self.action {
            StepAction::RunHandler(handler) => {
                input.insert("handler".to_string(), handler.into());
                if !self.parameters.is_empty() {
                    let args: Vec<::serde_json::Value> = self
                        .parameters
                        .iter()
                        .map(|parameter| parameter.placeholder().into())
                        .collect();
                    input.insert("args".to_string(), args.into());
                }
                input.insert("path".to_string(), PATH_TO_HANDLER.placeholder().into());
            }
            StepAction::RunCommand => {
                for parameter in self.parameters {
                    input.insert(parameter.key.to_string(), parameter.placeholder().into());
                }
            }
        }
        let run_as_user = RUN_AS_USER.placeholder();
        let document = ::serde_json::json!({
            "version": JOB_DOCUMENT_VERSION,
            "steps": [{
                "action": {
                    "name": self.step_name(),
                    "type": self.action.step_type(),
                    "input": ::serde_json::Value::Object(input),
                    "runAsUser": run_as_user,
                }
            }]
        });
        document.to_string()
    }

    /// This row expanded into the published entry.
    fn template(&self) -> ManagedJobTemplate {
        let version = ManagedJobTemplateVersion {
            template_version: self.template_version.to_string(),
            document: self.document(),
            document_parameters: self
                .parameters()
                .map(CatalogParameter::document_parameter)
                .collect(),
        };
        ManagedJobTemplate {
            template_name: self.template_name.to_string(),
            description: self.description.to_string(),
            environments: self
                .environments
                .iter()
                .map(|environment| (*environment).to_string())
                .collect(),
            versions: vec![version],
        }
    }
}

/// The `version` every managed template's job document declares: the schema version of
/// the document itself, which is what an AWS IoT job document carries beside its
/// `steps`.
const JOB_DOCUMENT_VERSION: &str = "1.0";

/// The prefix every published managed template's name carries, which its step's action
/// name is the name without.
const TEMPLATE_NAME_PREFIX: &str = "AWS-";

/// The environment the device client's remote actions are supported in.
const LINUX: &str = "LINUX";

/// The first published version of a managed template: the version every entry of this
/// catalog carries, in the `<major>.<minor>` form `ManagedTemplateVersion`
/// (`^[1-9]+.[0-9]+$`) fixes.
const FIRST_TEMPLATE_VERSION: &str = "1.0";

/// The published managed job templates, in the order the service publishes them: one row
/// per remote action AWS publishes for the AWS IoT Device Client, which is the whole of
/// what this catalog serves.
///
/// `AWS-Reboot` is named by the model itself
/// (`smithy_query(query="doc",shape="DescribeManagedJobTemplateResponse",member="templateName")`),
/// and the other rows are the client's remaining published actions: run a command, fetch
/// a file, install or remove applications, and start, stop or restart system services.
/// The table is COMPLETE rather than a sample — a listing of it is the answer to "which
/// managed templates are there", so a missing row would be a wrong answer and not a
/// short one — and its ORDER is published data too, because a caller that reads the
/// first entry or pages with `maxResults` reads the service's order and not an
/// alphabetical one.
const CATALOG: &[CatalogRow] = &[
    CatalogRow {
        template_name: "AWS-Run-Command",
        description: "A managed job template for running a shell command.",
        template_version: FIRST_TEMPLATE_VERSION,
        environments: &[LINUX],
        action: StepAction::RunCommand,
        parameters: &[CatalogParameter {
            key: "command",
            description: "A comma separated string of command, any comma contained in \
                          the command itself needs to be escaped.",
            kind: ValueKind::Text(4096),
            example: Some(r"echo,Hello\,World!"),
            optional: false,
        }],
    },
    CatalogRow {
        template_name: "AWS-Reboot",
        description: "A managed job template for rebooting the device.",
        template_version: FIRST_TEMPLATE_VERSION,
        environments: &[LINUX],
        action: StepAction::RunHandler("reboot.sh"),
        parameters: &[],
    },
    CatalogRow {
        template_name: "AWS-Remove-Application",
        description: "A managed job template for uninstalling one or more applications.",
        template_version: FIRST_TEMPLATE_VERSION,
        environments: &[LINUX],
        action: StepAction::RunHandler("remove-packages.sh"),
        parameters: &[CatalogParameter {
            key: "packages",
            description: "A space separated list of packages to uninstall.",
            kind: ValueKind::NameList,
            example: Some("lftp dos2unix tcpdump"),
            optional: false,
        }],
    },
    CatalogRow {
        template_name: "AWS-Stop-Application",
        description: "A managed job template for stopping one or more system services.",
        template_version: FIRST_TEMPLATE_VERSION,
        environments: &[LINUX],
        action: StepAction::RunHandler("stop-services.sh"),
        parameters: &[CatalogParameter {
            key: "services",
            description: "A space separated list of system services to stop.",
            kind: ValueKind::NameList,
            example: Some("cron httpd bluetooth"),
            optional: false,
        }],
    },
    CatalogRow {
        template_name: "AWS-Start-Application",
        description: "A managed job template for starting one or more system services.",
        template_version: FIRST_TEMPLATE_VERSION,
        environments: &[LINUX],
        action: StepAction::RunHandler("start-services.sh"),
        parameters: &[CatalogParameter {
            key: "services",
            description: "A space separated list of system services to start.",
            kind: ValueKind::NameList,
            example: Some("cron httpd bluetooth"),
            optional: false,
        }],
    },
    CatalogRow {
        template_name: "AWS-Restart-Application",
        description: "A managed job template for restarting one or more system services.",
        template_version: FIRST_TEMPLATE_VERSION,
        environments: &[LINUX],
        action: StepAction::RunHandler("restart-services.sh"),
        parameters: &[CatalogParameter {
            key: "services",
            description: "A space separated list of system services to restart.",
            kind: ValueKind::NameList,
            example: Some("cron httpd bluetooth"),
            optional: false,
        }],
    },
    CatalogRow {
        template_name: "AWS-Install-Application",
        description: "A managed job template for installing one or more applications.",
        template_version: FIRST_TEMPLATE_VERSION,
        environments: &[LINUX],
        action: StepAction::RunHandler("install-packages.sh"),
        parameters: &[CatalogParameter {
            key: "packages",
            description: "A space separated list of packages to install.",
            kind: ValueKind::NameList,
            example: Some("lftp dos2unix tcpdump"),
            optional: false,
        }],
    },
    CatalogRow {
        template_name: "AWS-Download-File",
        description: "A managed job template for downloading a file.",
        template_version: FIRST_TEMPLATE_VERSION,
        environments: &[LINUX],
        action: StepAction::RunHandler("download-file.sh"),
        parameters: &[
            CatalogParameter {
                key: "downloadUrl",
                description: "URL of file to download.",
                kind: ValueKind::AnyText,
                // The service illustrates a parameter with an `example` only where it
                // publishes one; this one carries none, so the response leaves the
                // optional member out rather than carrying text authored here.
                example: None,
                optional: false,
            },
            CatalogParameter {
                key: "filePath",
                description: "Path on the device where downloaded file is written.",
                kind: ValueKind::AnyText,
                example: Some("/path/to/file"),
                optional: false,
            },
        ],
    },
];

/// The catalog as the store holds it: [`CATALOG`] expanded into published entries, in
/// published order.
///
/// Every derived member is a function of its row, so the catalog is identical on every
/// boot: nothing here reads a clock, a request or a random source.
pub fn managed_job_template_catalog() -> Vec<ManagedJobTemplate> {
    CATALOG.iter().map(CatalogRow::template).collect()
}

/// The prefix every name in AWS's MANAGED job template namespace carries, which every row
/// of [`CATALOG`] is published under (`a_published_name_is_in_the_managed_namespace` pins
/// that). A managed template is AWS's to name, so a `templateName` outside this namespace
/// is not one of them and never could be — which is what decides HOW an unpublished name
/// is refused ([`unpublished_managed_job_template`]).
pub const MANAGED_NAME_PREFIX: &str = "AWS-";

/// Whether `template_name` is a name in AWS's managed job template namespace, i.e. one a
/// managed template could be published under. Exact text: the published names carry the
/// prefix as written, so a differently-cased spelling of it is another namespace.
pub fn is_managed_job_template_name_form(template_name: &str) -> bool {
    template_name.starts_with(MANAGED_NAME_PREFIX)
}

/// How this service refuses a request that names a managed job template the catalog does
/// not publish — the ONE decision, so a listing's filter and a describe cannot classify or
/// word the same miss two ways.
///
/// The catalog is AWS-PUBLISHED reference data rather than account state, the same for
/// every caller on every boot, and AWS publishes it under its own namespace
/// ([`MANAGED_NAME_PREFIX`]). Which of the two modelled refusals a miss earns follows from
/// that: a name IN the managed namespace could name a managed template, so one the catalog
/// does not hold is a resource that is not there ([`Self::NotFound`]), while a name outside
/// the namespace is not a managed template name at all and no catalog could ever hold it,
/// which makes it a value the request got wrong ([`Self::Invalid`]) — the same class an
/// unpublished `templateVersion` earns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnpublishedManagedJobTemplate {
    /// `ResourceNotFoundException` (HTTP 404, "The specified resource does not exist"),
    /// carrying [`managed_job_template_not_found_message`].
    NotFound(String),
    /// `InvalidRequestException` (HTTP 400, "The request is not valid"), carrying
    /// [`invalid_managed_job_template_name_message`].
    Invalid(String),
}

/// The refusal `template_name` earns from a catalog that does not publish it, with the
/// sentence that refusal carries.
pub fn unpublished_managed_job_template(template_name: &str) -> UnpublishedManagedJobTemplate {
    if is_managed_job_template_name_form(template_name) {
        return UnpublishedManagedJobTemplate::NotFound(managed_job_template_not_found_message(
            template_name,
        ));
    }
    UnpublishedManagedJobTemplate::Invalid(invalid_managed_job_template_name_message(
        template_name,
    ))
}

/// The sentence a `templateName` in AWS's managed namespace that the catalog does not hold
/// is refused with: fixed text naming the kind of resource that was looked up, around the
/// name the request sent.
pub fn managed_job_template_not_found_message(template_name: &str) -> String {
    format!("ManagedJobTemplate with name: {template_name} not found")
}

/// The sentence a `templateName` outside AWS's managed namespace is refused with: fixed
/// text naming the member that was rejected, then the name the request sent — worded the
/// way the unpublished-version refusal of the same catalog is
/// ([`invalid_managed_job_template_version_message`]).
pub fn invalid_managed_job_template_name_message(template_name: &str) -> String {
    format!("Invalid ManagedJobTemplate name: {template_name}")
}

/// How this service refuses a `templateVersion` a published template does not publish.
///
/// The template itself IS published here, so what the request named outside the catalog
/// is the version member: an `InvalidRequestException` (HTTP 400, "The request is not
/// valid") naming the member that was rejected and the version the request sent, which
/// keeps a published template asked for at an unpublished version distinguishable from a
/// template the catalog does not publish at all.
pub fn invalid_managed_job_template_version_message(template_version: &str) -> String {
    format!("Invalid ManagedJobTemplate version: {template_version}")
}

/// Whether `template_version` is a `ManagedTemplateVersion`: the modelled
/// `^[1-9]+.[0-9]+$` — one or more leading digits of which the first is not zero, a
/// dot, then one or more digits.
///
/// A continuation token carries the version it resumes after, so a listing checks a
/// caller-supplied one against the model before it looks for it in the catalog.
pub fn is_modeled_managed_template_version(template_version: &str) -> bool {
    let Some((major, minor)) = template_version.split_once('.') else {
        return false;
    };
    !major.is_empty()
        && !major.starts_with('0')
        && major.chars().all(|digit| digit.is_ascii_digit())
        && !minor.is_empty()
        && minor.chars().all(|digit| digit.is_ascii_digit())
}

/// Whether `template_name` is a `ManagedJobTemplateName`: modelled `length 1..64` with
/// no pattern of its own. Smithy `length` counts CHARACTERS, so this does too.
pub fn is_modeled_managed_job_template_name(template_name: &str) -> bool {
    (1..=64).contains(&template_name.chars().count())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The invariant [`MANAGED_NAME_PREFIX`] claims: AWS publishes its managed templates
    /// under its own namespace, so every row of the catalog carries that prefix — which is
    /// what makes a name outside it one no catalog could hold.
    #[test]
    fn a_published_name_is_in_the_managed_namespace() {
        for row in CATALOG {
            assert!(
                is_managed_job_template_name_form(row.template_name),
                "case {:?}: a published template is named in the managed namespace",
                row.template_name
            );
        }
    }

    /// The classification of a name the catalog does not hold, on each side of the
    /// namespace: in it, the resource is simply not there; outside it, the name is not a
    /// managed template name at all.
    #[test]
    fn an_unpublished_name_is_classified_by_the_namespace_it_is_in() {
        assert_eq!(
            unpublished_managed_job_template("AWS-Not-Published"),
            UnpublishedManagedJobTemplate::NotFound(
                "ManagedJobTemplate with name: AWS-Not-Published not found".to_string()
            ),
            "a name in the managed namespace that is not published is a not-found"
        );
        assert_eq!(
            unpublished_managed_job_template("not-managed"),
            UnpublishedManagedJobTemplate::Invalid(
                "Invalid ManagedJobTemplate name: not-managed".to_string()
            ),
            "a name outside the managed namespace is not a managed template name"
        );
    }
}

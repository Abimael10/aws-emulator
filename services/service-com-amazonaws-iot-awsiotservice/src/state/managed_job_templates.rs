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
//! Each entry is a REMOTE ACTION AWS publishes for the AWS IoT Device Client: the
//! template's job document runs one of the client's job handlers, and the patterns the
//! document leaves open are exactly the entry's `documentParameters` — "a map of
//! key-value pairs containing the patterns that need to be replaced in a managed
//! template job document schema"
//! (`smithy_query(query="shape",shape="DocumentParameter")`). The document is BUILT
//! from that parameter table ([`CatalogRow::document`]), so a parameter and the
//! `${aws:iot:parameter:<key>}` placeholder it fills cannot drift apart, and every
//! template declares the two parameters that are about the handler rather than about
//! the action ([`COMMON_PARAMETERS`]).
//!
//! `AWS-Reboot` is the entry the model itself names
//! (`smithy_query(query="doc",shape="DescribeManagedJobTemplateResponse",member="templateName")`:
//! "The unique name of a managed template, such as AWS-Reboot"); the rest of the table
//! is the published set of the device client's remote actions — downloading a file,
//! running a command, and installing, removing, starting, stopping or restarting an
//! application.

use std::collections::BTreeMap;

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
/// template, and every version of it, keyed by `templateVersion`.
#[derive(Debug, Clone)]
pub struct ManagedJobTemplate {
    /// `templateName` — "The unique Name for a managed template".
    pub template_name: String,
    /// `description` — "The description for a managed template".
    pub description: String,
    /// `environments` — "A list of environments that are supported with the managed job
    /// template".
    pub environments: Vec<String>,
    /// Every published version of this template, keyed by `templateVersion`.
    ///
    /// A `BTreeMap` because `ListManagedJobTemplates` is `@paginated` and, with a
    /// `templateName`, lists "the versions of the managed job templates that have the
    /// specified template name" — a page boundary over versions is only meaningful over
    /// a stable order.
    pub versions: BTreeMap<String, ManagedJobTemplateVersion>,
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
            .values()
            .max_by_key(|version| version_order(&version.template_version))
    }

    /// The version `requested` names, or the default version when it names none;
    /// `None` when this template publishes no such version.
    pub fn version(&self, requested: Option<&str>) -> Option<&ManagedJobTemplateVersion> {
        match requested {
            Some(wanted) => self.versions.get(wanted),
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
/// A struct of its own, rather than a bare map on the state struct, so that the state's
/// derived `Default` — what the engine builds the store with on first touch — seeds the
/// AWS-managed collection COMPLETE while every customer-owned collection beside it
/// starts empty.
#[derive(Debug, Clone)]
pub struct ManagedJobTemplateCatalog {
    /// Every published template, keyed by `templateName`: the name is the template's
    /// identity and the `@httpLabel` of `/managed-job-templates/{templateName}`.
    entries: BTreeMap<String, ManagedJobTemplate>,
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
    /// under that name.
    pub fn template(&self, template_name: &str) -> Option<&ManagedJobTemplate> {
        self.entries.get(template_name)
    }

    /// Every published (template, version) PAIR, in catalog order: by `templateName`,
    /// then by `templateVersion`.
    ///
    /// A pair and not a template, because one entry of a listing is one version of one
    /// template — with a `templateName` the listing is of "the versions of the managed
    /// job templates that have the specified template name".
    pub fn pairs(
        &self,
    ) -> impl Iterator<Item = (&ManagedJobTemplate, &ManagedJobTemplateVersion)> + '_ {
        self.entries.values().flat_map(|template| {
            template
                .versions
                .values()
                .map(move |version| (template, version))
        })
    }
}

/// What kind of value a template parameter takes, which fixes the `regex` the catalog
/// publishes as "a regular expression of the patterns that need to be replaced" and the
/// `example` that illustrates it (`DocumentParameter$regex`, `$example`).
///
/// Carried as a KIND rather than as two strings per parameter so that two parameters
/// that take the same kind of value are published with the same rule, and a parameter's
/// example always satisfies its own regex.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ValueKind {
    /// An HTTP(S) location the device fetches from.
    Url,
    /// A path on the device's file system.
    Path,
    /// One or more operating-system package names, separated by commas.
    PackageNames,
    /// The name of a service the device's init system manages.
    ServiceName,
    /// A command line, whose arguments the device client splits on commas.
    CommandLine,
    /// The name of an operating-system user.
    UserName,
}

impl ValueKind {
    /// The `regex` the catalog publishes for a parameter of this kind.
    ///
    /// Each one is the character class the value can have: a URL is a scheme and the
    /// rest of the location, a service or user name is a single name token, a package
    /// list is one or more name tokens separated by commas, and a path or a command line
    /// is any text with no control character in it — the same class `JobDescription` is
    /// modelled with (`^[^\p{C}]+$`).
    const fn regex(self) -> &'static str {
        match self {
            Self::Url => "^(http|https)://[^\\p{C}]+$",
            Self::Path | Self::CommandLine => "^[^\\p{C}]+$",
            Self::PackageNames => "^[a-zA-Z0-9+._-]+(,[a-zA-Z0-9+._-]+)*$",
            Self::ServiceName | Self::UserName => "^[a-zA-Z0-9+._-]+$",
        }
    }

    /// The `example` the catalog publishes for a parameter of this kind: "an example
    /// illustrating a pattern that need to be replaced in a managed template job
    /// document schema".
    const fn example(self) -> &'static str {
        match self {
            Self::Url => "https://example.com/device-firmware.1.0",
            Self::Path => "/tmp/device-firmware.1.0",
            Self::PackageNames => "openssl,curl",
            Self::ServiceName => "aws-iot-device-client",
            Self::CommandLine => "ls,-l",
            Self::UserName => "root",
        }
    }
}

/// One parameter of a managed template's document: the pattern a caller replaces, what
/// it is for, and the kind of value that goes there.
struct CatalogParameter {
    /// `key` — "Key of the map field containing the patterns that need to be replaced in
    /// a managed template job document schema", a `ParameterKey`
    /// (`^[a-zA-Z0-9_-]+$`).
    key: &'static str,
    /// `description` — what the caller has to put there.
    description: &'static str,
    kind: ValueKind,
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
            regex: Some(self.kind.regex().to_string()),
            example: Some(self.kind.example().to_string()),
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

/// Where the device client's job handlers live on the device, which every managed
/// template's document carries as its step's `path`. Optional: a device client
/// configured with its own handler directory needs none.
const PATH_TO_HANDLER: CatalogParameter = CatalogParameter {
    key: "pathToHandler",
    description: "The directory on the device that contains the job handler to run. \
                  Optional: the device client falls back to its configured handler \
                  directory.",
    kind: ValueKind::Path,
    optional: true,
};

/// Which user the handler runs as, which every managed template's document carries as
/// its step's `runAsUser`. Optional: the device client otherwise runs it as the user it
/// runs as itself.
const RUN_AS_USER: CatalogParameter = CatalogParameter {
    key: "runAsUser",
    description: "The user to run the job handler as. Optional: the device client \
                  falls back to the user it runs as itself.",
    kind: ValueKind::UserName,
    optional: true,
};

/// The parameters EVERY managed template declares, because they are about running the
/// device client's handler rather than about the action itself.
const COMMON_PARAMETERS: &[CatalogParameter] = &[PATH_TO_HANDLER, RUN_AS_USER];

/// One row of the catalog table: everything that differs between managed templates. The
/// rest of an entry — its ARN, its document, the common parameters it declares — is
/// DERIVED from the row, so no two members of one entry can disagree.
struct CatalogRow {
    template_name: &'static str,
    description: &'static str,
    /// The published version of this template.
    template_version: &'static str,
    /// The environments the template is supported with. An `Environment`
    /// (`^[^\p{C}]+$`).
    environments: &'static [&'static str],
    /// The name of the one step the template's document runs.
    step_name: &'static str,
    /// The device client's job handler that step runs.
    handler: &'static str,
    /// The template's OWN parameters, in the order the document passes them to the
    /// handler as arguments. The common parameters are declared by every template and
    /// are not repeated here.
    parameters: &'static [CatalogParameter],
}

impl CatalogRow {
    /// Every parameter this template declares: its own, then the ones every template
    /// declares.
    fn parameters(&self) -> impl Iterator<Item = &CatalogParameter> {
        self.parameters.iter().chain(COMMON_PARAMETERS.iter())
    }

    /// The template's `document`: one `runHandler` step that runs this row's handler,
    /// with a `${aws:iot:parameter:<key>}` placeholder wherever one of the row's own
    /// parameters goes, the handler directory as the step's `path` and the user as its
    /// `runAsUser`.
    ///
    /// Built from the row rather than written out per template, so a document can carry
    /// no placeholder the entry does not declare a parameter for.
    fn document(&self) -> String {
        let mut input = ::serde_json::Map::new();
        input.insert("handler".to_string(), self.handler.into());
        if !self.parameters.is_empty() {
            let args: Vec<::serde_json::Value> = self
                .parameters
                .iter()
                .map(|parameter| parameter.placeholder().into())
                .collect();
            input.insert("args".to_string(), args.into());
        }
        input.insert("path".to_string(), PATH_TO_HANDLER.placeholder().into());
        let run_as_user = RUN_AS_USER.placeholder();
        let document = ::serde_json::json!({
            "version": JOB_DOCUMENT_VERSION,
            "steps": [{
                "action": {
                    "name": self.step_name,
                    "type": "runHandler",
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
            versions: BTreeMap::from([(version.template_version.clone(), version)]),
        }
    }
}

/// The `version` every managed template's job document declares: the schema version of
/// the document itself, which is what an AWS IoT job document carries beside its
/// `steps`.
const JOB_DOCUMENT_VERSION: &str = "1.0";

/// The environment the device client's remote actions are supported in.
const LINUX: &str = "LINUX";

/// The first published version of a managed template: the version every entry of this
/// catalog carries, in the `<major>.<minor>` form `ManagedTemplateVersion`
/// (`^[1-9]+.[0-9]+$`) fixes.
const FIRST_TEMPLATE_VERSION: &str = "1.0";

/// The published managed job templates: one row per remote action AWS publishes for the
/// AWS IoT Device Client, which is the whole of what this catalog serves.
///
/// `AWS-Reboot` is named by the model itself
/// (`smithy_query(query="doc",shape="DescribeManagedJobTemplateResponse",member="templateName")`),
/// and the other rows are the client's remaining published actions: fetch a file, run a
/// command, and install, remove, start, stop or restart an application. The table is
/// COMPLETE rather than a sample — a listing of it is the answer to "which managed
/// templates are there", so a missing row would be a wrong answer and not a short one.
const CATALOG: &[CatalogRow] = &[
    CatalogRow {
        template_name: "AWS-Download-File",
        description: "A managed job template to download a file to a device.",
        template_version: FIRST_TEMPLATE_VERSION,
        environments: &[LINUX],
        step_name: "downloadFile",
        handler: "download-file.sh",
        parameters: &[
            CatalogParameter {
                key: "downloadUrl",
                description: "The URL the device downloads the file from.",
                kind: ValueKind::Url,
                optional: false,
            },
            CatalogParameter {
                key: "filePath",
                description: "The path on the device the downloaded file is written to.",
                kind: ValueKind::Path,
                optional: false,
            },
        ],
    },
    CatalogRow {
        template_name: "AWS-Install-Application",
        description: "A managed job template to install one or more applications on a device.",
        template_version: FIRST_TEMPLATE_VERSION,
        environments: &[LINUX],
        step_name: "installPackages",
        handler: "install-packages.sh",
        parameters: &[CatalogParameter {
            key: "packages",
            description: "The packages to install, separated by commas.",
            kind: ValueKind::PackageNames,
            optional: false,
        }],
    },
    CatalogRow {
        template_name: "AWS-Reboot",
        description: "A managed job template to reboot a device.",
        template_version: FIRST_TEMPLATE_VERSION,
        environments: &[LINUX],
        step_name: "reboot",
        handler: "reboot.sh",
        parameters: &[],
    },
    CatalogRow {
        template_name: "AWS-Remove-Application",
        description: "A managed job template to remove one or more applications from a device.",
        template_version: FIRST_TEMPLATE_VERSION,
        environments: &[LINUX],
        step_name: "removePackages",
        handler: "remove-packages.sh",
        parameters: &[CatalogParameter {
            key: "packages",
            description: "The packages to remove, separated by commas.",
            kind: ValueKind::PackageNames,
            optional: false,
        }],
    },
    CatalogRow {
        template_name: "AWS-Restart-Application",
        description: "A managed job template to restart an application's service on a device.",
        template_version: FIRST_TEMPLATE_VERSION,
        environments: &[LINUX],
        step_name: "restartService",
        handler: "restart-service.sh",
        parameters: &[CatalogParameter {
            key: "serviceName",
            description: "The name of the service to restart.",
            kind: ValueKind::ServiceName,
            optional: false,
        }],
    },
    CatalogRow {
        template_name: "AWS-Run-Command",
        description: "A managed job template to run a shell command on a device.",
        template_version: FIRST_TEMPLATE_VERSION,
        environments: &[LINUX],
        step_name: "runCommand",
        handler: "run-command.sh",
        parameters: &[CatalogParameter {
            key: "command",
            description: "The command to run, with its arguments separated by commas.",
            kind: ValueKind::CommandLine,
            optional: false,
        }],
    },
    CatalogRow {
        template_name: "AWS-Start-Application",
        description: "A managed job template to start an application's service on a device.",
        template_version: FIRST_TEMPLATE_VERSION,
        environments: &[LINUX],
        step_name: "startService",
        handler: "start-service.sh",
        parameters: &[CatalogParameter {
            key: "serviceName",
            description: "The name of the service to start.",
            kind: ValueKind::ServiceName,
            optional: false,
        }],
    },
    CatalogRow {
        template_name: "AWS-Stop-Application",
        description: "A managed job template to stop an application's service on a device.",
        template_version: FIRST_TEMPLATE_VERSION,
        environments: &[LINUX],
        step_name: "stopService",
        handler: "stop-service.sh",
        parameters: &[CatalogParameter {
            key: "serviceName",
            description: "The name of the service to stop.",
            kind: ValueKind::ServiceName,
            optional: false,
        }],
    },
];

/// The catalog as the store holds it: [`CATALOG`] expanded into published entries, keyed
/// by `templateName`.
///
/// Every derived member is a function of its row, so the catalog is identical on every
/// boot: nothing here reads a clock, a request or a random source.
pub fn managed_job_template_catalog() -> BTreeMap<String, ManagedJobTemplate> {
    CATALOG
        .iter()
        .map(|row| (row.template_name.to_string(), row.template()))
        .collect()
}

/// How this service refuses a request that names a managed job template the catalog does
/// not publish.
///
/// Not a not-found: the catalog is AWS-published reference data rather than a resource of
/// this account, so a `templateName` outside it is an INVALID REQUEST
/// (`InvalidRequestException`, HTTP 400, "The request is not valid"). The fixed text
/// names the shape the value was rejected as, and the variable part is the name the
/// request sent. Every operation that takes a `templateName` refuses it with this one
/// sentence, so a listing's filter and a describe cannot word the same refusal two ways.
pub fn invalid_managed_job_template_name_message(template_name: &str) -> String {
    format!("Invalid ManagedJobTemplate name: {template_name}")
}

/// How this service refuses a `templateVersion` a published template does not publish.
///
/// The same class and the same template form as
/// [`invalid_managed_job_template_name_message`], with the MEMBER that was rejected and
/// the version the request sent — which keeps a published template asked for at an
/// unpublished version distinguishable from a template the catalog does not publish at
/// all.
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

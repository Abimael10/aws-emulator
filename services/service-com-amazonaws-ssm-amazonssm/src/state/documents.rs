// SPDX-License-Identifier: CC0-1.0

//! The DOCUMENTS this service's state holds, and everything the document operations read
//! and write them through: the bounds a document's untrusted content is guarded by, the
//! parse that turns it into the modelled description, the catalog of documents AWS itself
//! publishes, and the stored record one document version is kept as.
//!
//! Moved out of [`super`] unchanged so each file of the state module keeps one concern;
//! every name stays reachable under `crate::state` through that module's re-export.

use super::*;

/// Pre-flight bounds on untrusted content, applied BEFORE any parser builds a tree
/// from it: the YAML parser descends recursively, so the nesting the bytes can ask for
/// has to be bounded while there is still no tree to measure.
///
/// This is a GUARD, not a parser: it knows nothing about YAML context and measures only
/// quantities a hiding trick can push UP but never DOWN — the content's length, the
/// number of flow openers (whose count IS their depth bound, since flow nesting costs
/// neither a line nor a column), the compact indicators on any one line, and a line's
/// leading indentation. Closers are never subtracted, so a `]` inside a scalar cannot
/// buy depth back. The same guard the Greengrass recipe parser
/// (`service-com-amazonaws-greengrassv2-greengrassv2/src/state.rs`) and the
/// CloudFormation template parser apply, for the same reason and with the same bounds;
/// the three copies are to be migrated onto one shared implementation rather than
/// copied a fourth time.
pub(super) fn guard_document_content(content: &str) -> Result<(), ContentRejection> {
    content_within_size(content)?;
    let mut flow_openers = 0usize;
    let mut compact_on_line = 0usize;
    let mut indent = 0usize;
    let mut counting_indent = true;
    for byte in content.bytes() {
        match byte {
            b'\n' | b'\r' => {
                indent = 0;
                compact_on_line = 0;
                counting_indent = true;
            }
            b' ' | b'\t' if counting_indent => {
                indent += 1;
                if indent > MAX_DOCUMENT_INDENT {
                    return Err(ContentRejection::Content(format!(
                        "a line of Content must not be indented past {MAX_DOCUMENT_INDENT} columns"
                    )));
                }
            }
            b'[' | b'{' => {
                counting_indent = false;
                flow_openers += 1;
                if flow_openers > MAX_DOCUMENT_FLOW_OPENERS {
                    return Err(ContentRejection::Content(format!(
                        "Content must not carry more than {MAX_DOCUMENT_FLOW_OPENERS} '[' or '{{' \
                         openers"
                    )));
                }
            }
            b'-' | b'?' => {
                counting_indent = false;
                compact_on_line += 1;
                if compact_on_line > MAX_DOCUMENT_COMPACT_PER_LINE {
                    return Err(ContentRejection::Content(format!(
                        "a line of Content must not carry more than \
                         {MAX_DOCUMENT_COMPACT_PER_LINE} '-' or '?' indicators"
                    )));
                }
            }
            _ => counting_indent = false,
        }
    }
    Ok(())
}

/// Refuses content that uses a YAML alias, BEFORE the loader can expand one.
///
/// `yaml-rust2`'s loader RESOLVES an alias by copying the anchored node, so by the
/// time a tree exists the expansion has already happened — a short alias chain can
/// expand to gigabytes. The parser's own event stream reports an alias as
/// `Event::Alias` and copies nothing, so the content is parsed once for this check and
/// only alias-free content is handed to the loader. An anchor no alias refers to
/// expands nothing and is accepted.
pub(super) fn reject_document_aliases(content: &str) -> Result<(), ContentRejection> {
    #[derive(Default)]
    struct AliasDetector {
        saw_alias: bool,
    }
    impl ::yaml_rust2::parser::EventReceiver for AliasDetector {
        fn on_event(&mut self, event: ::yaml_rust2::parser::Event) {
            if matches!(event, ::yaml_rust2::parser::Event::Alias(_)) {
                self.saw_alias = true;
            }
        }
    }

    let mut detector = AliasDetector::default();
    ::yaml_rust2::parser::Parser::new_from_str(content)
        .load(&mut detector, true)
        .map_err(|why| {
            ContentRejection::Content(format!("Content could not be parsed as a document: {why}"))
        })?;
    if detector.saw_alias {
        return Err(ContentRejection::Content(
            "Content must not use YAML aliases".to_string(),
        ))
    }
    Ok(())
}

/// Look a key up in a document mapping.
pub(super) fn mapping_get<'a>(
    map: &'a ::yaml_rust2::yaml::Hash,
    key: &str,
) -> Option<&'a ::yaml_rust2::Yaml> {
    map.iter()
        .find(|(held, _)| held.as_str() == Some(key))
        .map(|(_, value)| value)
}

/// A document scalar as the string the API reports it as.
pub(super) fn scalar_string(value: &::yaml_rust2::Yaml) -> Option<String> {
    match value {
        ::yaml_rust2::Yaml::String(text) | ::yaml_rust2::Yaml::Real(text) => Some(text.clone()),
        ::yaml_rust2::Yaml::Boolean(flag) => Some(flag.to_string()),
        ::yaml_rust2::Yaml::Integer(number) => Some(number.to_string()),
        _ => None,
    }
}

/// A parameter's `default` as the single string `DefaultValue` reports: the scalar
/// itself, or the JSON rendering of a list or mapping default.
///
/// `budget` is the recursion left, so a deeply nested default cannot drive this into
/// the stack even though [`guard_document_content`] already bounded the nesting; a
/// default that exhausts it is REFUSED rather than reported with the member missing.
pub(super) fn default_value_string(
    name: &str,
    value: &::yaml_rust2::Yaml,
    budget: usize,
) -> Result<String, ContentRejection> {
    if let Some(scalar) = scalar_string(value) {
        return Ok(scalar);
    }
    let rendered = document_as_json(value, budget).ok_or_else(|| {
        ContentRejection::Content(format!(
            "the default of parameter '{name}' is not a value this service can read: it nests \
             past {MAX_DOCUMENT_DEPTH} levels or is keyed by a non-string"
        ))
    })?;
    Ok(rendered.to_string())
}

/// A document node as the JSON value that renders it.
pub(crate) fn document_as_json(
    node: &::yaml_rust2::Yaml,
    budget: usize,
) -> Option<::serde_json::Value> {
    use ::serde_json::Value;
    use ::yaml_rust2::Yaml;
    let budget = budget.checked_sub(1)?;
    Some(match node {
        Yaml::Null | Yaml::BadValue => Value::Null,
        Yaml::Boolean(flag) => Value::Bool(*flag),
        Yaml::Integer(number) => Value::Number((*number).into()),
        Yaml::Real(real) => real
            .parse::<f64>()
            .ok()
            .and_then(::serde_json::Number::from_f64)
            .map_or_else(|| Value::String(real.clone()), Value::Number),
        Yaml::String(text) => Value::String(text.clone()),
        Yaml::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| document_as_json(item, budget))
                .collect::<Option<Vec<_>>>()?,
        ),
        Yaml::Hash(map) => {
            let mut object = ::serde_json::Map::new();
            for (key, value) in map {
                object.insert(key.as_str()?.to_string(), document_as_json(value, budget)?);
            }
            Value::Object(object)
        }
        // An alias is refused before the load by `reject_document_aliases`, so a
        // resolved alias node can never reach here.
        Yaml::Alias(_) => return None,
    })
}

/// The parameters a document declares: one `DocumentParameter` per `parameters` entry,
/// in the content's own order.
///
/// "A description of the parameters for a document"
/// (`DocumentDescription$Parameters`): each entry's `type`, `description` and
/// `default` are reported as the content declares them — the declared type is reported
/// verbatim rather than narrowed, because an Automation document's parameters carry
/// types (`Integer`, `StringMap`, …) beyond the two a Command document's do.
pub(super) fn declared_parameters(
    map: &::yaml_rust2::yaml::Hash,
) -> Result<Vec<DocumentParameter>, ContentRejection> {
    let Some(declared) = mapping_get(map, "parameters") else {
        return Ok(Vec::new());
    };
    if matches!(declared, ::yaml_rust2::Yaml::Null) {
        return Ok(Vec::new());
    }
    let Some(declared) = declared.as_hash() else {
        return Err(ContentRejection::Content(
            "the parameters of a document must be a mapping of parameter name to its definition"
                .to_string(),
        ));
    };
    let mut parameters = Vec::with_capacity(declared.len());
    for (name, definition) in declared {
        let Some(name) = name.as_str() else {
            return Err(ContentRejection::Content(
                "every parameter of a document must be named by a string".to_string(),
            ));
        };
        let Some(definition) = definition.as_hash() else {
            return Err(ContentRejection::Content(format!(
                "the definition of parameter '{name}' must be a mapping"
            )));
        };
        let default_value = match mapping_get(definition, "default") {
            None => None,
            Some(default) => Some(default_value_string(name, default, MAX_DOCUMENT_DEPTH)?),
        };
        parameters.push(DocumentParameter {
            name: Some(name.to_string()),
            r#type: mapping_get(definition, "type").and_then(scalar_string),
            description: mapping_get(definition, "description").and_then(scalar_string),
            default_value,
        });
    }
    Ok(parameters)
}

/// The names of the steps `content` declares, in its own order.
///
/// A schema 2.x document declares its steps as `mainSteps`, each with its own `name`
/// ("The name of the step"), and a schema 1.2 document declares them as the `runtimeConfig`
/// plugin entries, whose keys are the plugin names. A `GetCommandInvocation` asks for one
/// step's detailed results by that name, so both spellings are read.
pub(super) fn declared_step_names(map: &::yaml_rust2::yaml::Hash) -> Vec<String> {
    if let Some(steps) = mapping_get(map, "mainSteps").and_then(::yaml_rust2::Yaml::as_vec) {
        return steps
            .iter()
            .filter_map(|step| mapping_get(step.as_hash()?, "name").and_then(scalar_string))
            .collect();
    }
    mapping_get(map, "runtimeConfig")
        .and_then(::yaml_rust2::Yaml::as_hash)
        .map(|plugins| {
            plugins
                .keys()
                .filter_map(::yaml_rust2::Yaml::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// The ONE document tree `content` holds, or why it is not one document tree this
/// service accepts.
///
/// This is what "one document tree" means for this crate: the create that derives a
/// document's report from its content and the read that re-renders that content in
/// another format ([`document_format`]) both read it through here.
///
/// The pre-flight bounds are applied HERE, before the loader is handed anything, so no
/// caller can reach it without them: the parser descends recursively
/// ([`guard_document_content`]) and resolves an alias by copying the anchored node
/// ([`reject_document_aliases`]), so content that was stored without being measured — a
/// `TEXT` entry a later read re-renders, say — is measured on the way in rather than
/// trusted. All three checks are pure functions of `content`, so applying them again
/// where a caller has already applied them changes nothing.
pub fn load_single_document(content: &str) -> Result<::yaml_rust2::Yaml, ContentRejection> {
    content_within_size(content)?;
    guard_document_content(content)?;
    reject_document_aliases(content)?;
    let mut documents = ::yaml_rust2::YamlLoader::load_from_str(content).map_err(|why| {
        ContentRejection::Content(format!("Content could not be parsed as a document: {why}"))
    })?;
    if documents.len() != 1 {
        return Err(ContentRejection::Content(format!(
            "Content must be exactly one document; found {}",
            documents.len()
        )));
    }
    Ok(documents.remove(0))
}

/// What `content` declares, or why it is not a document this service accepts.
///
/// "The content for the new SSM document in JSON or YAML format"
/// (`CreateDocument`'s `Content` doc), so the content is parsed in the format the
/// request declared — YAML 1.2 is a superset of JSON, so one parser reads either, and
/// `JSON` is additionally required to BE JSON rather than only YAML that resembles it.
/// `TEXT` content (a Change Calendar entry) is free text with no document tree, so
/// nothing is derived from it and nothing in it is refused.
pub fn parse_document_content(
    content: &str,
    format: &str,
    document_type: &str,
) -> Result<ParsedDocumentContent, ContentRejection> {
    // The documented size bound is measured for content of every format, including the
    // one with no document tree to parse.
    content_within_size(content)?;
    if format == DOCUMENT_FORMAT_TEXT {
        return Ok(ParsedDocumentContent::default());
    }
    // The nesting guard runs before the JSON reading below, which builds a tree of its
    // own from the same bytes; [`load_single_document`] applies it (and the alias
    // rejection) again before the YAML loader sees them.
    guard_document_content(content)?;
    if format == DEFAULT_DOCUMENT_FORMAT
        && ::serde_json::from_str::<::serde_json::Value>(content).is_err()
    {
        return Err(ContentRejection::Content(
            "Content isn't a valid JSON document".to_string(),
        ));
    }
    let document = load_single_document(content)?;
    let Some(map) = document.as_hash() else {
        return Err(ContentRejection::Content(
            "Content must be a mapping of document keys to values".to_string(),
        ));
    };

    let schema_version = mapping_get(map, "schemaVersion").and_then(scalar_string);
    let supported = supported_schema_versions(document_type);
    if !supported.is_empty() {
        match schema_version.as_deref() {
            None => {
                return Err(ContentRejection::Content(format!(
                    "Content declares no schemaVersion; a {document_type} document must declare \
                     one of {}",
                    supported.join(", ")
                )))
            }
            Some(declared) if !supported.contains(&declared) => {
                return Err(ContentRejection::SchemaVersion(format!(
                    "Document schema version '{declared}' isn't supported for a {document_type} \
                     document; the supported schema versions are {}",
                    supported.join(", ")
                )))
            }
            Some(_) => {}
        }
    }

    Ok(ParsedDocumentContent {
        schema_version,
        description: mapping_get(map, "description").and_then(scalar_string),
        parameters: declared_parameters(map)?,
        platform_types: document_platforms::declared_platform_types(map),
        step_names: declared_step_names(map),
    })
}

// ---------------------------------------------------------------------------
// The SSM documents Systems Manager publishes
// ---------------------------------------------------------------------------

/// The `Owner` a document Systems Manager publishes reports.
///
/// `Owner` is "the Amazon Web Services user that created the document"
/// (`DocumentDescription$Owner`), and a published document was created by Amazon rather
/// than by the account that reads it — the owner a listing selects it by
/// (`ListDocuments`' `Owner` filter: `Self`, `Amazon`, `Public`, `Private`,
/// `ThirdParty`).
pub const PUBLISHED_DOCUMENT_OWNER: &str = "Amazon";

/// One document of the catalogue Systems Manager publishes: present in every account and
/// Region without anybody creating it, and readable — never writable — by the account
/// that reads it.
#[derive(Debug, Clone, Copy)]
pub struct PublishedDocument {
    /// The name the document resolves by, which is also what a read reports.
    pub name: &'static str,
    /// A `DocumentType` value (`crate::ops::types::DOCUMENT_TYPE_VALUES`).
    pub document_type: &'static str,
    /// The document's body, in [`DEFAULT_DOCUMENT_FORMAT`]. Everything a read reports
    /// beyond the name, the type and the publication date is derived from it by the one
    /// content reader ([`parse_document_content`]), exactly as it is for a document a
    /// caller created.
    pub content: &'static str,
    /// When Amazon published this document, in MILLISECONDS since the Unix epoch.
    ///
    /// `CreatedDate` is "the date the document was created"
    /// (`DocumentVersionInfo$CreatedDate`, `DocumentDescription$CreatedDate`), and nobody
    /// in this account created a published document: Amazon did, once, before any account
    /// read it. So the date is a FACT OF THE CATALOGUE carried here as data rather than a
    /// clock reading taken when this store came into being — identical in every account,
    /// Region and run of the emulator, and unchanged by a restart. Milliseconds, because
    /// that is the precision Systems Manager reports a document date at.
    pub created_date_millis: u64,
}

impl PublishedDocument {
    /// The instant this document was published, as the stored record and every projection
    /// of it carry it ([`PublishedDocument::created_date_millis`]).
    pub fn created_date(&self) -> SystemTime {
        ::std::time::UNIX_EPOCH + ::std::time::Duration::from_millis(self.created_date_millis)
    }
}

/// The body of the runbook that configures versioning on an S3 bucket.
///
/// The parameters each published runbook takes are the ones the "Systems Manager
/// Automation runbook reference" documents for it; the body below is this emulator's
/// stand-in for the published body, declaring those parameters so a read derives them.
pub(super) const CONFIGURE_S3_BUCKET_VERSIONING: &str = r#"{
  "schemaVersion": "0.3",
  "description": "Configures versioning on an Amazon S3 bucket.",
  "assumeRole": "{{ AutomationAssumeRole }}",
  "parameters": {
    "AutomationAssumeRole": {
      "type": "String",
      "description": "(Optional) The ARN of the role that allows Automation to perform the actions on your behalf.",
      "default": ""
    },
    "BucketName": {
      "type": "String",
      "description": "(Required) The name of the S3 bucket to configure versioning on."
    },
    "VersioningState": {
      "type": "String",
      "description": "(Optional) The versioning state of the bucket.",
      "default": "Enabled",
      "allowedValues": ["Enabled", "Suspended"]
    }
  },
  "mainSteps": [
    {
      "name": "PutBucketVersioning",
      "action": "aws:executeAwsApi",
      "inputs": {
        "Service": "s3",
        "Api": "PutBucketVersioning",
        "Bucket": "{{ BucketName }}",
        "VersioningConfiguration": { "Status": "{{ VersioningState }}" }
      }
    },
    {
      "name": "VerifyBucketVersioning",
      "action": "aws:executeAwsApi",
      "inputs": { "Service": "s3", "Api": "GetBucketVersioning", "Bucket": "{{ BucketName }}" }
    }
  ]
}"#;

/// The body of the runbook that blocks public access on an S3 bucket. See
/// [`CONFIGURE_S3_BUCKET_VERSIONING`] for where the parameters come from.
pub(super) const CONFIGURE_S3_BUCKET_PUBLIC_ACCESS_BLOCK: &str = r#"{
  "schemaVersion": "0.3",
  "description": "Configures the public access block of an Amazon S3 bucket.",
  "assumeRole": "{{ AutomationAssumeRole }}",
  "parameters": {
    "AutomationAssumeRole": {
      "type": "String",
      "description": "(Optional) The ARN of the role that allows Automation to perform the actions on your behalf.",
      "default": ""
    },
    "BucketName": {
      "type": "String",
      "description": "(Required) The name of the S3 bucket to configure."
    },
    "BlockPublicAcls": {
      "type": "Boolean",
      "description": "(Optional) Whether Amazon S3 blocks public ACLs for this bucket.",
      "default": true
    },
    "IgnorePublicAcls": {
      "type": "Boolean",
      "description": "(Optional) Whether Amazon S3 ignores public ACLs for this bucket.",
      "default": true
    },
    "BlockPublicPolicy": {
      "type": "Boolean",
      "description": "(Optional) Whether Amazon S3 blocks public bucket policies for this bucket.",
      "default": true
    },
    "RestrictPublicBuckets": {
      "type": "Boolean",
      "description": "(Optional) Whether Amazon S3 restricts public bucket policies for this bucket.",
      "default": true
    }
  },
  "mainSteps": [
    {
      "name": "PutBucketPublicAccessBlock",
      "action": "aws:executeAwsApi",
      "inputs": {
        "Service": "s3",
        "Api": "PutPublicAccessBlock",
        "Bucket": "{{ BucketName }}",
        "PublicAccessBlockConfiguration": {
          "BlockPublicAcls": "{{ BlockPublicAcls }}",
          "IgnorePublicAcls": "{{ IgnorePublicAcls }}",
          "BlockPublicPolicy": "{{ BlockPublicPolicy }}",
          "RestrictPublicBuckets": "{{ RestrictPublicBuckets }}"
        }
      }
    }
  ]
}"#;

/// The body of the runbook that removes a bucket's public read and write grants. See
/// [`CONFIGURE_S3_BUCKET_VERSIONING`] for where the parameters come from.
pub(super) const DISABLE_S3_BUCKET_PUBLIC_READ_WRITE: &str = r#"{
  "schemaVersion": "0.3",
  "description": "Removes the public read and write grants of an Amazon S3 bucket.",
  "assumeRole": "{{ AutomationAssumeRole }}",
  "parameters": {
    "AutomationAssumeRole": {
      "type": "String",
      "description": "(Optional) The ARN of the role that allows Automation to perform the actions on your behalf.",
      "default": ""
    },
    "S3BucketName": {
      "type": "String",
      "description": "(Required) The name of the S3 bucket whose public grants are removed."
    }
  },
  "mainSteps": [
    {
      "name": "PutBucketAcl",
      "action": "aws:executeAwsApi",
      "inputs": {
        "Service": "s3",
        "Api": "PutBucketAcl",
        "Bucket": "{{ S3BucketName }}",
        "ACL": "private"
      }
    }
  ]
}"#;

/// The body of the runbook that turns on default encryption of an S3 bucket. See
/// [`CONFIGURE_S3_BUCKET_VERSIONING`] for where the parameters come from.
pub(super) const ENABLE_S3_BUCKET_ENCRYPTION: &str = r#"{
  "schemaVersion": "0.3",
  "description": "Turns on default server-side encryption of an Amazon S3 bucket.",
  "assumeRole": "{{ AutomationAssumeRole }}",
  "parameters": {
    "AutomationAssumeRole": {
      "type": "String",
      "description": "(Optional) The ARN of the role that allows Automation to perform the actions on your behalf.",
      "default": ""
    },
    "BucketName": {
      "type": "String",
      "description": "(Required) The name of the S3 bucket to encrypt."
    },
    "SSEAlgorithm": {
      "type": "String",
      "description": "(Optional) The server-side encryption algorithm to apply by default.",
      "default": "AES256"
    }
  },
  "mainSteps": [
    {
      "name": "PutBucketEncryption",
      "action": "aws:executeAwsApi",
      "inputs": {
        "Service": "s3",
        "Api": "PutBucketEncryption",
        "Bucket": "{{ BucketName }}",
        "ServerSideEncryptionConfiguration": {
          "Rules": [
            {
              "ApplyServerSideEncryptionByDefault": { "SSEAlgorithm": "{{ SSEAlgorithm }}" }
            }
          ]
        }
      }
    }
  ]
}"#;

/// The body of the runbook that publishes a notification to an SNS topic. See
/// [`CONFIGURE_S3_BUCKET_VERSIONING`] for where the parameters come from.
pub(super) const PUBLISH_SNS_NOTIFICATION: &str = r#"{
  "schemaVersion": "0.3",
  "description": "Publishes a notification to an Amazon SNS topic.",
  "assumeRole": "{{ AutomationAssumeRole }}",
  "parameters": {
    "AutomationAssumeRole": {
      "type": "String",
      "description": "(Optional) The ARN of the role that allows Automation to perform the actions on your behalf.",
      "default": ""
    },
    "TopicArn": {
      "type": "String",
      "description": "(Required) The ARN of the SNS topic to publish to."
    },
    "Message": {
      "type": "String",
      "description": "(Required) The message to publish."
    }
  },
  "mainSteps": [
    {
      "name": "Publish",
      "action": "aws:executeAwsApi",
      "inputs": {
        "Service": "sns",
        "Api": "Publish",
        "TopicArn": "{{ TopicArn }}",
        "Message": "{{ Message }}"
      }
    }
  ]
}"#;

/// The body of the command document that runs a shell script on a managed node.
///
/// A command document's step actions and preconditions are what restrict it to a platform
/// ([`document_platforms::declared_platform_types`]), so the platforms the published
/// document is reported
/// compatible with are declared the same way a created document's are.
pub(super) const RUN_SHELL_SCRIPT: &str = r#"{
  "schemaVersion": "2.2",
  "description": "Run a shell script or specify the commands to run.",
  "parameters": {
    "commands": {
      "type": "StringList",
      "description": "(Required) Specify a shell script or a command to run."
    },
    "workingDirectory": {
      "type": "String",
      "description": "(Optional) The path to the working directory on your managed node.",
      "default": ""
    },
    "executionTimeout": {
      "type": "String",
      "description": "(Optional) The time in seconds for a command to be completed before it is considered to have failed.",
      "default": "3600"
    }
  },
  "mainSteps": [
    {
      "name": "runShellScriptOnLinux",
      "action": "aws:runShellScript",
      "precondition": { "StringEquals": ["platformType", "Linux"] },
      "inputs": {
        "commands": "{{ commands }}",
        "workingDirectory": "{{ workingDirectory }}",
        "timeoutSeconds": "{{ executionTimeout }}"
      }
    },
    {
      "name": "runShellScriptOnMacOS",
      "action": "aws:runShellScript",
      "precondition": { "StringEquals": ["platformType", "MacOS"] },
      "inputs": {
        "commands": "{{ commands }}",
        "workingDirectory": "{{ workingDirectory }}",
        "timeoutSeconds": "{{ executionTimeout }}"
      }
    }
  ]
}"#;

/// The body of the command document that runs a PowerShell script on a managed node. See
/// [`RUN_SHELL_SCRIPT`] for how the platforms are declared.
pub(super) const RUN_POWER_SHELL_SCRIPT: &str = r#"{
  "schemaVersion": "2.2",
  "description": "Run a PowerShell script or specify the commands to run.",
  "parameters": {
    "commands": {
      "type": "StringList",
      "description": "(Required) Specify a PowerShell script or a command to run."
    },
    "workingDirectory": {
      "type": "String",
      "description": "(Optional) The path to the working directory on your managed node.",
      "default": ""
    },
    "executionTimeout": {
      "type": "String",
      "description": "(Optional) The time in seconds for a command to be completed before it is considered to have failed.",
      "default": "3600"
    }
  },
  "mainSteps": [
    {
      "name": "runPowerShellScript",
      "action": "aws:runPowerShellScript",
      "precondition": { "StringEquals": ["platformType", "Windows"] },
      "inputs": {
        "commands": "{{ commands }}",
        "workingDirectory": "{{ workingDirectory }}",
        "timeoutSeconds": "{{ executionTimeout }}"
      }
    }
  ]
}"#;

/// The body of the command document that updates the SSM Agent on a managed node. It
/// declares no precondition, so it is compatible with every platform the model lists.
pub(super) const UPDATE_SSM_AGENT: &str = r#"{
  "schemaVersion": "2.2",
  "description": "Update the SSM Agent to the latest version or specify an older version.",
  "parameters": {
    "version": {
      "type": "String",
      "description": "(Optional) The version of the SSM Agent to install. An empty value installs the latest version.",
      "default": ""
    },
    "allowDowngrade": {
      "type": "String",
      "description": "(Optional) Whether the SSM Agent may be downgraded to an earlier version.",
      "default": "false",
      "allowedValues": ["true", "false"]
    }
  },
  "mainSteps": [
    {
      "name": "updateSSMAgent",
      "action": "aws:updateSsmAgent",
      "inputs": {
        "agentName": "amazon-ssm-agent",
        "source": "https://s3.amazonaws.com/ec2-downloads-windows/SSMAgent/{{version}}/amazon-ssm-agent.msi",
        "allowDowngrade": "{{ allowDowngrade }}",
        "targetVersion": "{{ version }}"
      }
    }
  ]
}"#;

/// The documents Systems Manager publishes, which exist in every account and Region
/// without anybody creating them.
///
/// The names and document types are the ones the AWS Systems Manager User Guide
/// publishes — the "Systems Manager Automation runbook reference" for the `AWS-*`
/// Automation runbooks (the runbooks a Config remediation configuration names as its
/// target), the "Systems Manager command document" reference for the `AWS-*` command
/// documents and the "Session document schema" for the `AWS-Start*Session` session
/// documents. Each body declares the parameters that reference documents for its
/// document, so a read derives its description, schema version, parameters and platform
/// types from the body rather than restating them here.
///
/// Each entry also carries WHEN Amazon published it
/// ([`PublishedDocument::created_date_millis`]), because `CreatedDate` is a fact of the
/// catalogue rather than a reading of this store's clock: the documents the recorded
/// catalogue of a real account (corpus `simparity-wire-aws-v1`) pins a date for carry that
/// date, and the rest carry this emulator's stand-in for their publication date — chosen
/// once, like the bodies — so that every account, Region and run of the emulator reports
/// one document's date identically.
///
/// Every name here starts with one of the prefixes [`RESERVED_DOCUMENT_NAME_PREFIXES`]
/// reserves, which is why no `CreateDocument` can collide with one.
pub const PUBLISHED_DOCUMENTS: &[PublishedDocument] = &[
    PublishedDocument {
        name: "AWS-ConfigureS3BucketVersioning",
        document_type: "Automation",
        content: CONFIGURE_S3_BUCKET_VERSIONING,
        created_date_millis: 1_545_264_000_000,
    },
    PublishedDocument {
        name: "AWS-ConfigureS3BucketPublicAccessBlock",
        document_type: "Automation",
        content: CONFIGURE_S3_BUCKET_PUBLIC_ACCESS_BLOCK,
        created_date_millis: 1_549_152_000_000,
    },
    PublishedDocument {
        name: "AWS-DisableS3BucketPublicReadWrite",
        document_type: "Automation",
        content: DISABLE_S3_BUCKET_PUBLIC_READ_WRITE,
        created_date_millis: 1_547_510_400_000,
    },
    PublishedDocument {
        name: "AWS-EnableS3BucketEncryption",
        document_type: "Automation",
        content: ENABLE_S3_BUCKET_ENCRYPTION,
        created_date_millis: 1_551_398_400_000,
    },
    PublishedDocument {
        name: "AWS-PublishSNSNotification",
        document_type: "Automation",
        content: PUBLISH_SNS_NOTIFICATION,
        created_date_millis: 1_554_076_800_000,
    },
    PublishedDocument {
        name: "AWS-RunShellScript",
        document_type: "Command",
        content: RUN_SHELL_SCRIPT,
        created_date_millis: 1_503_695_717_821,
    },
    PublishedDocument {
        name: "AWS-RunPowerShellScript",
        document_type: "Command",
        content: RUN_POWER_SHELL_SCRIPT,
        created_date_millis: 1_503_695_741_940,
    },
    PublishedDocument {
        name: "AWS-UpdateSSMAgent",
        document_type: "Command",
        content: UPDATE_SSM_AGENT,
        created_date_millis: 1_622_567_950_979,
    },
    PublishedDocument {
        name: inventory::GATHER_SOFTWARE_INVENTORY_DOCUMENT,
        document_type: DOCUMENT_TYPE_POLICY,
        content: inventory::GATHER_SOFTWARE_INVENTORY,
        created_date_millis: 1_564_011_832_472,
    },
    PublishedDocument {
        name: patch_runs::RUN_PATCH_BASELINE_DOCUMENT,
        document_type: "Command",
        content: patch_runs::RUN_PATCH_BASELINE,
        created_date_millis: 1_786_631_569_844,
    },
    // The rest of the Command documents Systems Manager publishes: what Run Command,
    // Distributor, State Manager and the patch and update workflows name. Each document
    // the recorded catalogue pins a date for carries that date; the rest carry the
    // stand-ins this file's header describes.
    PublishedDocument {
        name: "AWS-ConfigureAWSPackage",
        document_type: "Command",
        content: published_command_documents::CONFIGURE_AWS_PACKAGE,
        created_date_millis: 1_706_038_480_989,
    },
    PublishedDocument {
        name: "AWS-InstallApplication",
        document_type: "Command",
        content: published_command_documents::INSTALL_APPLICATION,
        created_date_millis: 1_503_695_753_117,
    },
    PublishedDocument {
        name: "AWS-InstallPowerShellModule",
        document_type: "Command",
        content: published_command_documents::INSTALL_POWER_SHELL_MODULE,
        created_date_millis: 1_503_695_761_438,
    },
    PublishedDocument {
        name: "AWS-InstallWindowsUpdates",
        document_type: "Command",
        content: published_command_documents::INSTALL_WINDOWS_UPDATES,
        created_date_millis: 1_781_017_685_843,
    },
    PublishedDocument {
        name: "AWS-FindWindowsUpdates",
        document_type: "Command",
        content: published_command_documents::FIND_WINDOWS_UPDATES,
        created_date_millis: 1_503_695_776_925,
    },
    PublishedDocument {
        name: "AWS-RunRemoteScript",
        document_type: "Command",
        content: published_command_documents::RUN_REMOTE_SCRIPT,
        created_date_millis: 1_508_978_913_261,
    },
    PublishedDocument {
        name: "AWS-RunDocument",
        document_type: "Command",
        content: published_command_documents::RUN_DOCUMENT,
        created_date_millis: 1_503_695_792_309,
    },
    PublishedDocument {
        name: "AWS-RefreshAssociation",
        document_type: "Command",
        content: published_command_documents::REFRESH_ASSOCIATION,
        created_date_millis: 1_503_695_800_174,
    },
    PublishedDocument {
        name: "AWS-JoinDirectoryServiceDomain",
        document_type: "Command",
        content: published_command_documents::JOIN_DIRECTORY_SERVICE_DOMAIN,
        created_date_millis: 1_503_695_808_461,
    },
    PublishedDocument {
        name: "AWS-ApplyAnsiblePlaybooks",
        document_type: "Command",
        content: published_command_documents::APPLY_ANSIBLE_PLAYBOOKS,
        created_date_millis: 1_758_834_443_390,
    },
    PublishedDocument {
        name: "AWS-ConfigureCloudWatch",
        document_type: "Command",
        content: published_command_documents::CONFIGURE_CLOUD_WATCH,
        created_date_millis: 1_525_720_957_739,
    },
    PublishedDocument {
        name: "AWS-RunInspecChecks",
        document_type: "Command",
        content: published_command_documents::RUN_INSPEC_CHECKS,
        created_date_millis: 1_758_834_488_387,
    },
    // The Session documents Session Manager starts a session with: the two the recorded
    // catalogue pins a date for carry that date, the rest a stand-in publication date.
    PublishedDocument {
        name: "AWS-StartSSHSession",
        document_type: DOCUMENT_TYPE_SESSION,
        content: published_command_documents::START_SSH_SESSION,
        created_date_millis: 1_562_087_817_013,
    },
    PublishedDocument {
        name: "AWS-StartPortForwardingSession",
        document_type: DOCUMENT_TYPE_SESSION,
        content: published_command_documents::START_PORT_FORWARDING_SESSION,
        created_date_millis: 1_563_487_178_453,
    },
    PublishedDocument {
        name: "AWS-StartPortForwardingSessionToRemoteHost",
        document_type: DOCUMENT_TYPE_SESSION,
        content: published_command_documents::START_PORT_FORWARDING_SESSION_TO_REMOTE_HOST,
        created_date_millis: 1_629_741_532_908,
    },
    PublishedDocument {
        name: "AWS-StartInteractiveCommand",
        document_type: DOCUMENT_TYPE_SESSION,
        content: published_command_documents::START_INTERACTIVE_COMMAND,
        created_date_millis: 1_641_332_078_651,
    },
    PublishedDocument {
        name: "AWS-StartNonInteractiveCommand",
        document_type: DOCUMENT_TYPE_SESSION,
        content: published_command_documents::START_NON_INTERACTIVE_COMMAND,
        created_date_millis: 1_641_332_086_774,
    },
    // The Automation runbooks that act on an Amazon EC2 instance or its volumes: each one
    // the recorded catalogue pins a date for carries that date, the rest a stand-in
    // publication date.
    PublishedDocument {
        name: "AWS-StartEC2Instance",
        document_type: "Automation",
        content: published_automation_runbooks::START_EC2_INSTANCE,
        created_date_millis: 1_782_421_644_712,
    },
    PublishedDocument {
        name: "AWS-StopEC2Instance",
        document_type: "Automation",
        content: published_automation_runbooks::STOP_EC2_INSTANCE,
        created_date_millis: 1_531_242_313_476,
    },
    PublishedDocument {
        name: "AWS-RestartEC2Instance",
        document_type: "Automation",
        content: published_automation_runbooks::RESTART_EC2_INSTANCE,
        created_date_millis: 1_511_217_229_956,
    },
    PublishedDocument {
        name: "AWS-TerminateEC2Instance",
        document_type: "Automation",
        content: published_automation_runbooks::TERMINATE_EC2_INSTANCE,
        created_date_millis: 1_514_908_825_638,
    },
    PublishedDocument {
        name: "AWS-CreateImage",
        document_type: "Automation",
        content: published_automation_runbooks::CREATE_IMAGE,
        created_date_millis: 1_533_661_478_787,
    },
    PublishedDocument {
        name: "AWS-CreateSnapshot",
        document_type: "Automation",
        content: published_automation_runbooks::CREATE_SNAPSHOT,
        created_date_millis: 1_514_908_841_805,
    },
    PublishedDocument {
        name: "AWS-DeleteSnapshot",
        document_type: "Automation",
        content: published_automation_runbooks::DELETE_SNAPSHOT,
        created_date_millis: 1_514_908_849_923,
    },
    PublishedDocument {
        name: "AWS-UpdateLinuxAmi",
        document_type: "Automation",
        content: published_automation_runbooks::UPDATE_LINUX_AMI,
        created_date_millis: 1_775_747_623_945,
    },
    PublishedDocument {
        name: "AWS-PatchInstanceWithRollback",
        document_type: "Automation",
        content: published_automation_runbooks::PATCH_INSTANCE_WITH_ROLLBACK,
        created_date_millis: 1_761_232_679_723,
    },
    PublishedDocument {
        name: "AWS-ResizeInstance",
        document_type: "Automation",
        content: published_automation_runbooks::RESIZE_INSTANCE,
        created_date_millis: 1_514_908_874_238,
    },
];

/// The `DocumentType` of a document that OPENS A SESSION on a managed node rather than
/// running a command on it (from the modelled enum,
/// `crate::ops::types::DOCUMENT_TYPE_VALUES`): the type of the session documents Session
/// Manager starts a session with.
pub const DOCUMENT_TYPE_SESSION: &str = "Session";

/// The `DocumentType` of a document that tells the agent on a managed node WHAT TO COLLECT
/// rather than what to run (from the modelled enum,
/// `crate::ops::types::DOCUMENT_TYPE_VALUES`): the type of the inventory policy document
/// Systems Manager publishes ([`inventory::GATHER_SOFTWARE_INVENTORY_DOCUMENT`]).
pub const DOCUMENT_TYPE_POLICY: &str = "Policy";

/// The published document `name` names, or `None` when the catalogue holds none.
///
/// A name is resolved as written: an SSM document name is case-sensitive, so a
/// differently-cased spelling names no document.
pub fn published_document(name: &str) -> Option<&'static PublishedDocument> {
    PUBLISHED_DOCUMENTS
        .iter()
        .find(|published| published.name == name)
}

/// What each catalogue body yields: the members the content reader derives from it, and
/// its hash.
///
/// Derived ONCE for the process rather than per read. The bodies are `const`, so two
/// reads of one published document cannot differ, and no request pays for parsing them.
/// A body this reader cannot read — a mistake in a `const` of this crate, which the test
/// suite reads every entry to catch, never a caller's input — yields nothing derived
/// rather than panicking a live request: the document does exist either way.
pub(super) static PUBLISHED_PROJECTIONS: ::std::sync::LazyLock<Vec<(ParsedDocumentContent, String)>> =
    ::std::sync::LazyLock::new(|| {
        PUBLISHED_DOCUMENTS
            .iter()
            .map(|published| {
                let parsed = parse_document_content(
                    published.content,
                    DEFAULT_DOCUMENT_FORMAT,
                    published.document_type,
                )
                .unwrap_or_default();
                (parsed, document_content_hash(published.content))
            })
            .collect()
    });

/// The published document `name` names together with what its body yields, or `None`
/// when the catalogue holds no document of that name.
pub(super) fn published_document_projection(
    name: &str,
) -> Option<(
    &'static PublishedDocument,
    &'static ParsedDocumentContent,
    &'static str,
)> {
    let index = PUBLISHED_DOCUMENTS
        .iter()
        .position(|published| published.name == name)?;
    let published = PUBLISHED_DOCUMENTS.get(index)?;
    let (parsed, hash) = PUBLISHED_PROJECTIONS.get(index)?;
    Some((published, parsed, hash.as_str()))
}

/// The hash the system takes over a document's content: "The Sha256 or Sha1 hash created
/// by the system when the document was created" (`DocumentDescription$Hash`), with "Sha1
/// hashes have been deprecated", rendered hex the way AWS reports it.
pub fn document_content_hash(content: &str) -> String {
    use ::sha2::Digest;
    ::hex::encode(::sha2::Sha256::digest(content.as_bytes()))
}

/// The attachment names `AttachmentsInformation` reports — "details about the document
/// attachments, including names" — for the attachment sources one call named.
///
/// An `AttachmentsSource` carries "the name of the document attachment file" in `Name`;
/// a source that names none is identified by the file its `Values` URL points at, which
/// is the last segment of that URL. Read by every call that supplies attachments for a
/// version of a document, so a create and an update identify them the same way.
pub fn attachment_names(attachments: Option<&[AttachmentsSource]>) -> Vec<String> {
    attachments
        .unwrap_or_default()
        .iter()
        .filter_map(|source| {
            source
                .name
                .clone()
                .filter(|name| !name.is_empty())
                .or_else(|| {
                    let value = source.values.as_ref()?.first()?;
                    let file = value.rsplit('/').next().unwrap_or(value);
                    (!file.is_empty()).then(|| file.to_string())
                })
        })
        .collect()
}

/// ONE version of a stored SSM document: what the call that minted it supplied, what the
/// service derived from that version's content, and the status move the clock still owes
/// it.
///
/// Everything here is per version because that is what the model says varies between
/// them: `DisplayName` "can differ for each version of the document", a `VersionName` is
/// "unique across all versions of a document", and the content — and so the hash, the
/// schema version, the description, the parameters and the platform types derived from it
/// — is what an update supplies a new one of.
#[derive(Debug, Clone)]
pub struct DocumentVersionRecord {
    /// "The document version", counting from [`FIRST_DOCUMENT_VERSION`].
    pub version: u64,
    pub display_name: Option<String>,
    pub version_name: Option<String>,
    /// A `DocumentFormat` value: the format this version's content is written in.
    pub document_format: String,
    pub target_type: Option<String>,
    /// The content exactly as the call supplied it: what
    /// [`DocumentVersionRecord::hash`] is taken over, and what `GetDocument` reports.
    pub content: String,
    /// Hex-rendered Sha256 of [`DocumentVersionRecord::content`].
    pub hash: String,
    /// Everything derived from the content (see [`parse_document_content`]).
    pub parsed: ParsedDocumentContent,
    pub requires: Option<Vec<DocumentRequires>>,
    /// The attachment names the call named, which is what `AttachmentsInformation`
    /// reports.
    pub attachment_names: Vec<String>,
    pub created_date: SystemTime,
    /// A `DocumentStatus` value.
    pub status: String,
    /// When the clock owes this version's move to `Active`, or `None` once that move has
    /// been materialised.
    pub active_due: Option<SystemTime>,
}

impl DocumentVersionRecord {
    /// Materialise the status move the clock already owes at `now` — the move to
    /// `Active` that was due whether or not anybody called.
    pub fn materialize_due(&mut self, now: SystemTime) {
        if self.active_due.is_some_and(|due| now >= due) {
            self.active_due = None;
            self.status = DOCUMENT_STATUS_ACTIVE.to_string();
        }
    }
}

/// One stored SSM document: the identity and the metadata the whole document carries,
/// and every version of it the store holds.
///
/// A create mints the first version and each `UpdateDocument` appends the next one, so
/// `LatestVersion` is the last version held. The DEFAULT version moves only when
/// `UpdateDocumentDefaultVersion` moves it, which is why an update's new version is the
/// latest without becoming the default.
#[derive(Debug, Clone)]
pub struct DocumentRecord {
    /// The identifier Systems Manager assigned the document when it was created, which is
    /// what a read reports as `DocumentId`: minted once, per document, so two reads of one
    /// document report the same id and no two documents share one
    /// ([`ComAmazonawsSsmAmazonssmState::mint_document_id`]). Nothing of the request is in
    /// it, and it is not the key the document is reached by — a document is addressed by
    /// name.
    pub document_id: String,
    /// The name the create supplied, which is what a read reports and part of the
    /// store key.
    pub name: String,
    /// The Region the document was created in; part of its store key and of its ARN.
    pub region: String,
    /// A `DocumentType` value. One document is of one type: `UpdateDocument` carries no
    /// `DocumentType` member, so a version never changes it.
    pub document_type: String,
    pub tags: BTreeMap<String, String>,
    /// "The Amazon Web Services user that created the document"
    /// (`DocumentDescription$Owner`): the account for a document a caller created here,
    /// and [`PUBLISHED_DOCUMENT_OWNER`] for one Systems Manager publishes.
    pub owner: String,
    /// Every version this document holds, oldest first and the newest last. Never empty
    /// for a record that is in the store.
    pub versions: Vec<DocumentVersionRecord>,
    /// "The default version", the one a read that names no version resolves to.
    pub default_version: u64,
    /// Who this document is SHARED with, as account id (or `all`) to the document version
    /// the share names — what `ModifyDocumentPermission` writes and
    /// `DescribeDocumentPermission` reports ([`super::document_permissions`]). Empty for a
    /// document nobody shared.
    pub shared_with: BTreeMap<String, String>,
}

/// Why a read's `DocumentVersion` / `VersionName` pair names no version of a document,
/// carrying what was asked for so each operation can report it in its own words.
#[derive(Debug, Clone)]
pub enum DocumentVersionRejection {
    /// No version of the document is the one this `DocumentVersion` selector names.
    Version(String),
    /// No version of the document carries this `VersionName`.
    VersionName(String),
}

impl DocumentRecord {
    /// This document's ARN: `arn:aws:ssm:<region>:<account>:document/<name>`.
    pub fn arn(&self) -> String {
        ::engine::emulator_arn(
            ARN_SERVICE,
            &self.region,
            &format!("{ARN_DOCUMENT_RESOURCE_PREFIX}{}", self.name),
        )
    }

    /// Materialise the status move the clock already owes every version at `now` — the
    /// move to `Active` that was due whether or not anybody called.
    pub fn materialize_due(&mut self, now: SystemTime) {
        for version in &mut self.versions {
            version.materialize_due(now);
        }
    }

    /// The newest version, which is "the latest version of the document"
    /// (`DocumentDescription$LatestVersion`).
    pub fn latest(&self) -> Option<&DocumentVersionRecord> {
        self.versions.last()
    }

    /// The number "the latest version of the document" carries.
    pub fn latest_version(&self) -> u64 {
        self.latest()
            .map_or(FIRST_DOCUMENT_VERSION, |latest| latest.version)
    }

    /// The number the next version of this document carries: each update mints the one
    /// after the latest.
    pub fn next_version(&self) -> u64 {
        self.latest_version().saturating_add(1)
    }

    /// The version `version` numbers, or `None` when this document holds no such version.
    pub fn version(&self, version: u64) -> Option<&DocumentVersionRecord> {
        self.versions.iter().find(|held| held.version == version)
    }

    /// The version `version_name` names: "This value is unique across all versions of a
    /// document" (`DocumentVersionInfo$VersionName`), so it names at most one of them.
    pub fn version_named(&self, version_name: &str) -> Option<&DocumentVersionRecord> {
        self.versions
            .iter()
            .find(|held| held.version_name.as_deref() == Some(version_name))
    }

    /// The version a `DocumentVersion` selector names, or `None` when this document
    /// holds none.
    ///
    /// The modelled `DocumentVersion` pattern admits three spellings: `$LATEST` names the
    /// newest version, `$DEFAULT` names "the default version", and a number names the
    /// version it counts.
    pub fn select(&self, selector: &str) -> Option<&DocumentVersionRecord> {
        match selector {
            DOCUMENT_VERSION_LATEST => self.latest(),
            DOCUMENT_VERSION_DEFAULT => self.version(self.default_version),
            number => self.version(number.parse().ok()?),
        }
    }

    /// The version a read's `DocumentVersion` and `VersionName` pair names, or why it
    /// names none of this document's versions.
    ///
    /// A read that names neither resolves to the default version. A read that names only
    /// a version NAME resolves to the version carrying it, because that name is "unique
    /// across all versions of a document"; one that names both has to name the same
    /// version twice, so a name the selected version does not carry names no version.
    pub fn select_requested(
        &self,
        document_version: Option<&str>,
        version_name: Option<&str>,
    ) -> Result<&DocumentVersionRecord, DocumentVersionRejection> {
        let document_version = document_version.filter(|selector| !selector.is_empty());
        let version_name = version_name.filter(|name| !name.is_empty());
        let selected = match (document_version, version_name) {
            (Some(selector), _) => self
                .select(selector)
                .ok_or_else(|| DocumentVersionRejection::Version(selector.to_string()))?,
            (None, Some(name)) => {
                return self
                    .version_named(name)
                    .ok_or_else(|| DocumentVersionRejection::VersionName(name.to_string()));
            }
            (None, None) => self.select(DOCUMENT_VERSION_DEFAULT).ok_or_else(|| {
                DocumentVersionRejection::Version(DOCUMENT_VERSION_DEFAULT.to_string())
            })?,
        };
        match version_name {
            Some(name) if selected.version_name.as_deref() != Some(name) => {
                Err(DocumentVersionRejection::VersionName(name.to_string()))
            }
            _ => Ok(selected),
        }
    }

    /// Whether `requested` names a version name this document carries.
    pub fn holds_version_name(&self, requested: &str) -> bool {
        self.version_named(requested).is_some()
    }

    /// The version whose content hashes to `hash`, or `None` when no version of this
    /// document holds that content.
    ///
    /// The hash is taken over the content ([`document_content_hash`]), so two versions
    /// hash alike exactly when their content is the same — which is what
    /// `DuplicateDocumentContent` is reported for.
    pub fn version_with_hash(&self, hash: &str) -> Option<&DocumentVersionRecord> {
        self.versions.iter().find(|held| held.hash == hash)
    }

    /// "The current review status of a new custom Systems Manager document"
    /// (`DocumentDescription$ReviewStatus`).
    ///
    /// A review is the change-template approval workflow, and `UpdateDocumentMetadata` —
    /// the operation that records one — is still a scaffold, so a change template created
    /// here has not been reviewed and no other document type carries a review at all.
    pub fn review_status(&self) -> Option<String> {
        (self.document_type == CHANGE_TEMPLATE_DOCUMENT_TYPE)
            .then(|| DOCUMENT_REVIEW_STATUS_NOT_REVIEWED.to_string())
    }

    /// The modelled `DocumentDescription` of `version` of this document: the version's
    /// own members beside the whole document's name, owner, type, tags, latest version
    /// and default version.
    pub fn description_of(&self, version: &DocumentVersionRecord) -> DocumentDescription {
        DocumentDescription {
            // The id Systems Manager assigned the document, which every read of it
            // reports.
            document_id: Some(self.document_id.clone()),
            // "Sha1 hashes have been deprecated", so the system hash is the Sha256 in
            // `Hash` and the deprecated member is left unset.
            sha1: None,
            hash: Some(version.hash.clone()),
            hash_type: Some(DOCUMENT_HASH_TYPE.to_string()),
            name: Some(self.name.clone()),
            display_name: version.display_name.clone(),
            version_name: version.version_name.clone(),
            owner: Some(self.owner.clone()),
            created_date: Some(::aws_smithy_types::DateTime::from(version.created_date)),
            status: Some(version.status.clone()),
            // "A message returned by Amazon Web Services Systems Manager that explains
            // the Status value", which it sets for a failure; nothing here failed.
            status_information: None,
            document_version: Some(version.version.to_string()),
            description: version.parsed.description.clone(),
            parameters: (!version.parsed.parameters.is_empty())
                .then(|| version.parsed.parameters.clone()),
            platform_types: (!version.parsed.platform_types.is_empty())
                .then(|| version.parsed.platform_types.clone()),
            document_type: Some(self.document_type.clone()),
            schema_version: version.parsed.schema_version.clone(),
            latest_version: Some(self.latest_version().to_string()),
            default_version: Some(self.default_version.to_string()),
            document_format: Some(version.document_format.clone()),
            target_type: version.target_type.clone(),
            // "The tags, or metadata, that have been applied to the document"
            // (`DocumentDescription$Tags`, a `TagList` of length 0..1000): a document
            // carrying none has an EMPTY tag list rather than no list at all, so a caller
            // iterating `Tags` never has to tell a missing member from an empty one.
            tags: Some(tagging::to_tag_list(&self.tags)),
            attachments_information: (!version.attachment_names.is_empty()).then(|| {
                version
                    .attachment_names
                    .iter()
                    .map(|name| AttachmentInformation {
                        name: Some(name.clone()),
                    })
                    .collect()
            }),
            requires: version.requires.clone(),
            // "The user in your organization who created the document": the emulator
            // authenticates no human author, so there is none to report.
            author: None,
            // A review is the change-template approval workflow, and `UpdateDocument`
            // metadata — the operation that records one — is still a scaffold, so a
            // change template created here has not been reviewed and no other document
            // type carries a review at all.
            review_information: None,
            approved_version: None,
            pending_review_version: None,
            review_status: self.review_status(),
            // "The classification of a document to help you identify and categorize
            // its use", which Systems Manager assigns to the documents it publishes;
            // nothing classifies a document created through this API.
            category: None,
            category_enum: None,
        }
    }

    /// The modelled `DocumentDescription` of the version `selector` names, or `None` when
    /// this document holds no such version.
    pub fn description_of_version(&self, selector: &str) -> Option<DocumentDescription> {
        Some(self.description_of(self.select(selector)?))
    }

    /// The same description as a DESCRIBE of this document reports it: the document's
    /// classification members carried as the empty lists a document nothing classified
    /// has.
    ///
    /// `Category` is "the classification of a document to help you identify and
    /// categorize its use" and `CategoryEnum` "the value that identifies a document's
    /// category" (both `CategoryList`/`CategoryEnumList`, `@length` 0..3): Systems Manager
    /// classifies the documents it publishes, so a document created through this API
    /// carries NO category — which a describe reports as an empty list rather than as a
    /// missing member, the way it reports an untagged document's `Tags`. RECORDED
    /// BEHAVIOUR (corpus `simparity-wire-aws-v1`, fixture `ssm-association-create-and-list`
    /// step `setup#2`): the create's own response carries neither member while the describe
    /// carries both as `[]`, so this shape belongs to the describe rather than to every
    /// read.
    pub fn described_version(&self, version: &DocumentVersionRecord) -> DocumentDescription {
        DocumentDescription {
            category: Some(Vec::new()),
            category_enum: Some(Vec::new()),
            ..self.description_of(version)
        }
    }

    /// Drop the version `version` numbers from this document, reporting whether it held
    /// one. A document whose last version is dropped holds none and is removed whole by
    /// its store ([`ComAmazonawsSsmAmazonssmState::remove_document_version`]).
    pub fn remove_version(&mut self, version: u64) -> bool {
        let held = self.version(version).is_some();
        self.versions.retain(|candidate| candidate.version != version);
        held
    }
}

// SPDX-License-Identifier: CC0-1.0

//! The Docker CLI as a build environment provides it: the login, the image build and the
//! PUSH a build's commands make against this emulator's own container registry.
//!
//! # The rule this module implements
//!
//! The commands a container build runs are the ones AWS documents for pushing an image to
//! Amazon ECR, and each does what the CLI documents it as doing:
//!
//! * `docker login --username AWS --password-stdin <registry>` authenticates to the registry
//!   it names; the password it is given has to be one that registry ISSUED
//!   ([`super::aws_cli`]), so a build that never asked for one — or one that presents
//!   anything else — is not logged in, and the push that follows fails the way a push with no
//!   credential fails;
//! * `docker build` composes an image out of the Dockerfile and the context it names
//!   ([`super::container_image`]) and tags it, so the image exists in the build's own
//!   environment and nowhere else yet;
//! * `docker tag` gives an image another reference, which is what a push then addresses;
//! * `docker push` UPLOADS that image to the repository its reference names: each blob
//!   through the registry's layer-upload calls and then the manifest through `PutImage`, so
//!   the repository really holds the image once the command has exited 0 — and a push the
//!   registry refused, or one no policy in force lets the build's role make (the role's own
//!   documents and the repository's own policy decided together, `super::build_tools`), exits
//!   non-zero with nothing uploaded.
//!
//! A reference is `[<registry>/]<repository>[:<tag>]`, and a push addresses the registry its
//! own reference names: this environment reaches the registries of THIS emulator, and the
//! LOGIN is what says a host is one of them — a login only records a registry whose own
//! authorization token it presented — so a push to a host nothing logged in to is refused the
//! way a push with no credential is.
//!
//! Every other command of the CLI, and every form of these four that carries an option they
//! do not take, is an invocation this program performs nothing of — the session reports it as
//! a command it could not run.

use super::container_image::{self, LocalImage};
use super::{
    answered, service_answered, BuildTools, DOCKER_FAILED, MAX_LOCAL_IMAGES, PROGRAM_SUCCEEDED,
};
use ::service_common::posix_shell_session::{ProgramFiles, ProgramOutcome};

/// The name a command line invokes the Docker CLI by.
pub const DOCKER_PROGRAM: &str = "docker";

/// The tag a reference that names none addresses, which is the CLI's own default.
const DEFAULT_TAG: &str = "latest";

/// The host a registry of this emulator is addressed at: Amazon ECR publishes a private
/// registry as `<account>.dkr.ecr.<region>.amazonaws.com`, so the Region of the registry a
/// push addresses is read out of the host it names.
const ECR_HOST_INFIX: &str = ".dkr.ecr.";

/// See [`ECR_HOST_INFIX`].
const ECR_HOST_SUFFIX: &str = ".amazonaws.com";

/// The IAM actions a push performs, in the order the push performs them ("Actions defined by
/// Amazon Elastic Container Registry", IAM Service Authorization Reference, and the pushing
/// permissions the Amazon ECR User Guide lists for a repository).
const PUSH_ACTIONS: [&str; 4] = [
    "ecr:InitiateLayerUpload",
    "ecr:UploadLayerPart",
    "ecr:CompleteLayerUpload",
    "ecr:PutImage",
];

/// What one `docker` command line asks for — `None` for every invocation this program
/// performs nothing of.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Requested {
    /// `login`: authenticate to a registry.
    Login {
        /// The registry the login names, as the command wrote it.
        registry: String,
        /// The password the command carries, and `None` for one that reads it from standard
        /// input.
        password: Option<String>,
    },
    /// `build`: compose an image from a Dockerfile and a context.
    Build {
        /// The references the built image is tagged with.
        tags: Vec<String>,
        /// The Dockerfile the image is built from.
        dockerfile: Option<String>,
        /// The `--build-arg` values the command carried, keyed by variable name: "set
        /// build-time variables", which take effect where the Dockerfile declares an `ARG`
        /// for them ([`container_image`]).
        build_arguments: ::std::collections::BTreeMap<String, String>,
        /// The context the image is built from.
        context: String,
    },
    /// `tag`: give an image another reference.
    Tag {
        /// The image being tagged.
        source: String,
        /// The reference it is given.
        target: String,
    },
    /// `push`: upload an image to the repository its reference names.
    Push {
        /// The image being pushed.
        reference: String,
    },
}

/// What the words of a `docker` command line ask for.
///
/// The options read are the ones these four commands take and nothing else: a line carrying
/// any other option changes what the command does, so it is an invocation this program
/// performs nothing of.
fn requested(arguments: &[String]) -> Option<Requested> {
    let mut words = arguments.iter().map(String::as_str);
    let command = words.next()?;
    let mut operands: Vec<&str> = Vec::new();
    let mut tags: Vec<String> = Vec::new();
    let mut dockerfile: Option<String> = None;
    let mut build_arguments: ::std::collections::BTreeMap<String, String> =
        ::std::collections::BTreeMap::new();
    let mut password: Option<String> = None;
    let mut password_stdin = false;
    let mut username: Option<String> = None;
    while let Some(word) = words.next() {
        match word {
            "-t" | "--tag" => tags.push(words.next()?.to_string()),
            "-f" | "--file" => dockerfile = Some(words.next()?.to_string()),
            "-u" | "--username" => username = Some(words.next()?.to_string()),
            "-p" | "--password" => password = Some(words.next()?.to_string()),
            "--build-arg" => {
                let (name, value) = build_argument(words.next()?)?;
                build_arguments.insert(name, value);
            }
            "--password-stdin" => password_stdin = true,
            option if option.starts_with('-') => {
                // An option written with its value attached (`--tag=x`), which is the same
                // option; every other option is one these commands do not take.
                let (name, value) = option.split_once('=')?;
                match name {
                    "-t" | "--tag" => tags.push(value.to_string()),
                    "-f" | "--file" => dockerfile = Some(value.to_string()),
                    "-u" | "--username" => username = Some(value.to_string()),
                    "-p" | "--password" => password = Some(value.to_string()),
                    "--build-arg" => {
                        let (name, value) = build_argument(value)?;
                        build_arguments.insert(name, value);
                    }
                    _ => return None,
                }
            }
            operand => operands.push(operand),
        }
    }
    match command {
        "login" => {
            // The login has to name WHO is logging in and WITH WHAT: the documented login
            // takes the user and reads the password from standard input.
            username?;
            if password.is_some() == password_stdin {
                return None;
            }
            let [registry] = operands[..] else {
                return None;
            };
            Some(Requested::Login {
                registry: registry.to_string(),
                password,
            })
        }
        "build" => {
            let [context] = operands[..] else {
                return None;
            };
            Some(Requested::Build {
                tags,
                dockerfile,
                build_arguments,
                context: context.to_string(),
            })
        }
        "tag" => {
            let [source, target] = operands[..] else {
                return None;
            };
            Some(Requested::Tag {
                source: source.to_string(),
                target: target.to_string(),
            })
        }
        "push" => {
            let [reference] = operands[..] else {
                return None;
            };
            Some(Requested::Push {
                reference: reference.to_string(),
            })
        }
        _ => None,
    }
}

/// The variable and the value one `--build-arg` word names: "`--build-arg <varname>=<value>`".
///
/// `None` for a word naming no value, which takes the variable's value from the environment
/// the builder itself runs in — an environment this program is handed none of, so that form is
/// not performed rather than resolved against something else.
fn build_argument(written: &str) -> Option<(String, String)> {
    let (name, value) = written.split_once('=')?;
    (!name.is_empty()).then(|| (name.to_string(), value.to_string()))
}

/// Perform the invocation `arguments` asks for, or answer `None` for one this program
/// performs nothing of.
pub(super) fn performed(
    tools: &BuildTools,
    arguments: &[String],
    standard_input: Option<&str>,
    files: &mut dyn ProgramFiles,
) -> Option<ProgramOutcome> {
    Some(match requested(arguments)? {
        Requested::Login { registry, password } => {
            // The password a login reads from standard input is what the stage before it
            // wrote, without the newline that stage ended it with.
            let presented = password.unwrap_or_else(|| standard_input.unwrap_or("").to_string());
            logged_in(tools, &registry, presented.trim())
        }
        Requested::Build {
            tags,
            dockerfile,
            build_arguments,
            context,
        } => built(
            tools,
            &tags,
            dockerfile.as_deref(),
            &build_arguments,
            &context,
            files,
        ),
        Requested::Tag { source, target } => tagged(tools, &source, &target),
        Requested::Push { reference } => pushed(tools, &reference),
    })
}

/// `docker login`: record this build as logged in to `registry` when `presented` is a
/// password that registry issued it.
fn logged_in(tools: &BuildTools, registry: &str, presented: &str) -> ProgramOutcome {
    if presented.is_empty() {
        return tools.refused(
            DOCKER_PROGRAM,
            "Error: Cannot perform an interactive login from a non TTY device",
            DOCKER_FAILED,
        );
    }
    if !tools.state().issued_passwords.contains(presented) {
        // The registry never issued this credential, so it is not one it honours.
        return tools.refused(
            DOCKER_PROGRAM,
            &format!(
                "Error response from daemon: login attempt to https://{} failed with status: \
                 401 Unauthorized",
                registry_host(registry)
            ),
            DOCKER_FAILED,
        );
    }
    tools
        .state()
        .logins
        .insert(registry_host(registry).to_string());
    answered(PROGRAM_SUCCEEDED, vec!["Login Succeeded".to_string()])
}

/// `docker build`: compose the image the Dockerfile and the context name, and hold it under
/// each reference the command tags it with.
fn built(
    tools: &BuildTools,
    tags: &[String],
    dockerfile: Option<&str>,
    build_arguments: &::std::collections::BTreeMap<String, String>,
    context: &str,
    files: &dyn ProgramFiles,
) -> ProgramOutcome {
    let dockerfile = dockerfile.map_or_else(
        || match context.trim_end_matches('/') {
            "" | "." => container_image::DEFAULT_DOCKERFILE.to_string(),
            held => format!("{held}/{}", container_image::DEFAULT_DOCKERFILE),
        },
        str::to_string,
    );
    let image = match container_image::composed(files, context, &dockerfile, build_arguments) {
        Ok(image) => image,
        Err(detail) => return tools.refused(DOCKER_PROGRAM, &detail, DOCKER_FAILED),
    };
    let digest = container_image::content_digest(image.manifest().as_bytes());
    let mut written = vec![format!(
        "Successfully built {}",
        short_digest(&digest)
    )];
    {
        let mut state = tools.state();
        for tag in tags {
            let Some(reference) = Reference::parse(tag) else {
                return tools.refused(
                    DOCKER_PROGRAM,
                    &format!("invalid reference format: {tag}"),
                    DOCKER_FAILED,
                );
            };
            if state.images.len() >= MAX_LOCAL_IMAGES && !state.images.contains_key(&reference.held())
            {
                return tools.refused(
                    DOCKER_PROGRAM,
                    &format!(
                        "no space left on device: a build composes at most {MAX_LOCAL_IMAGES} \
                         images"
                    ),
                    DOCKER_FAILED,
                );
            }
            state.images.insert(reference.held(), image.clone());
            written.push(format!("Successfully tagged {}", reference.held()));
        }
    }
    answered(PROGRAM_SUCCEEDED, written)
}

/// `docker tag`: give the image `source` names the reference `target`.
fn tagged(tools: &BuildTools, source: &str, target: &str) -> ProgramOutcome {
    let (Some(source), Some(target)) = (Reference::parse(source), Reference::parse(target)) else {
        return tools.refused(
            DOCKER_PROGRAM,
            &format!("invalid reference format: {source} or {target}"),
            DOCKER_FAILED,
        );
    };
    let mut state = tools.state();
    let Some(image) = state.images.get(&source.held()).cloned() else {
        return tools.refused(
            DOCKER_PROGRAM,
            &format!(
                "Error response from daemon: No such image: {}",
                source.held()
            ),
            DOCKER_FAILED,
        );
    };
    if state.images.len() >= MAX_LOCAL_IMAGES && !state.images.contains_key(&target.held()) {
        return tools.refused(
            DOCKER_PROGRAM,
            &format!("no space left on device: a build composes at most {MAX_LOCAL_IMAGES} images"),
            DOCKER_FAILED,
        );
    }
    state.images.insert(target.held(), image);
    answered(PROGRAM_SUCCEEDED, Vec::new())
}

/// `docker push`: upload the image `reference` names to the repository of the registry it
/// names.
fn pushed(tools: &BuildTools, reference: &str) -> ProgramOutcome {
    let Some(reference) = Reference::parse(reference) else {
        return tools.refused(
            DOCKER_PROGRAM,
            &format!("invalid reference format: {reference}"),
            DOCKER_FAILED,
        );
    };
    let Some(image) = tools.state().images.get(&reference.held()).cloned() else {
        return tools.refused(
            DOCKER_PROGRAM,
            &format!(
                "An image does not exist locally with the tag: {}",
                reference.image()
            ),
            DOCKER_FAILED,
        );
    };
    let Some(registry) = reference.registry.clone() else {
        return tools.refused(
            DOCKER_PROGRAM,
            &format!(
                "Error response from daemon: push access denied for {}: this build environment \
                 reaches no registry outside this emulator",
                reference.repository
            ),
            DOCKER_FAILED,
        );
    };
    if !tools.state().logins.contains(&registry) {
        // A push to Amazon ECR presents the credential a login recorded; a daemon holding
        // none for the registry has nothing to present. It is also what says the registry
        // the push addresses is one of THIS emulator's: a login only records a registry
        // whose own authorization token it presented ([`logged_in`]).
        return tools.refused(
            DOCKER_PROGRAM,
            &format!("no basic auth credentials for {registry}"),
            DOCKER_FAILED,
        );
    }
    // WHICH registry: the Region the host names when it is one Amazon ECR publishes, and
    // otherwise the Region the build itself runs in — the endpoint this emulator serves its
    // registries at names no Region, and one registry of this account serves each.
    let region = registry_region(&registry).unwrap_or_else(|| tools.region.clone());
    let repository_arn = BuildTools::repository_arn(&region, &reference.repository);
    // A repository is reached only where a policy in force allows it, and TWO documents can
    // allow one: the role's own identity-based policies and the repository's own
    // resource-based policy, which is what shares a repository with a principal that has no
    // identity-based grant — and what withholds it from one that has, when a statement of it
    // denies. The document is read once, through the service that owns it, and then decides
    // every action the push performs.
    let shared = tools.repository_policy(&region, &reference.repository);
    for action in PUSH_ACTIONS {
        if let Some(denial) = tools.withheld_with_shared(action, &repository_arn, shared.as_ref()) {
            return tools.refused(DOCKER_PROGRAM, &format!("denied: {denial}"), DOCKER_FAILED);
        }
    }
    match uploaded(tools, &region, &reference, &image) {
        Ok(digest) => answered(
            PROGRAM_SUCCEEDED,
            vec![
                format!(
                    "The push refers to repository [{registry}/{}]",
                    reference.repository
                ),
                format!("{}: Pushed", short_digest(&image.layer_blob().1)),
                format!(
                    "{}: digest: {digest} size: {}",
                    reference.tag,
                    image.manifest().len()
                ),
            ],
        ),
        Err(detail) => tools.refused(DOCKER_PROGRAM, &detail, DOCKER_FAILED),
    }
}

/// Upload `image`'s blobs and then its manifest, and answer the digest the registry stored
/// the image under.
fn uploaded(
    tools: &BuildTools,
    region: &str,
    reference: &Reference,
    image: &LocalImage,
) -> Result<String, String> {
    // Every blob the manifest references has to be in the repository before the manifest is
    // pushed: "after you upload the image layers, the PutImage API is called once".
    for (blob, _) in [image.config_blob(), image.layer_blob()] {
        pushed_blob(tools, region, &reference.repository, blob)?;
    }
    tools.made(|| async {
        let answer = tools
            .ecr(region)
            .put_image()
            .repository_name(&reference.repository)
            .image_manifest(image.manifest())
            .image_manifest_media_type(container_image::MANIFEST_MEDIA_TYPE)
            .image_tag(&reference.tag)
            .send()
            .await
            .map_err(|error| service_answered("PutImage", &error))?;
        answer
            .image()
            .and_then(|image| image.image_id())
            .and_then(|id| id.image_digest.clone())
            .ok_or_else(|| "The PutImage operation answered no image digest".to_string())
    })
}

/// Upload ONE blob of an image into `repository`, and answer the digest the registry
/// addresses it by.
///
/// A blob the repository ALREADY holds is one the push does not need to send again, which is
/// what the registry's own refusal of a completed upload says; every other refusal is the
/// push's failure.
fn pushed_blob(
    tools: &BuildTools,
    region: &str,
    repository: &str,
    blob: &[u8],
) -> Result<String, String> {
    let digest = container_image::content_digest(blob);
    tools.made(|| async {
        let client = tools.ecr(region);
        let upload = client
            .initiate_layer_upload()
            .repository_name(repository)
            .send()
            .await
            .map_err(|error| service_answered("InitiateLayerUpload", &error))?;
        let upload_id = upload
            .upload_id()
            .ok_or_else(|| {
                "The InitiateLayerUpload operation answered no upload id".to_string()
            })?
            .to_string();
        let last_byte = i64::try_from(blob.len())
            .map_err(|_| "The blob is larger than one upload carries".to_string())?
            - 1;
        client
            .upload_layer_part()
            .repository_name(repository)
            .upload_id(&upload_id)
            .part_first_byte(0)
            .part_last_byte(last_byte)
            .layer_part_blob(::aws_sdk_ecr::primitives::Blob::new(blob.to_vec()))
            .send()
            .await
            .map_err(|error| service_answered("UploadLayerPart", &error))?;
        match client
            .complete_layer_upload()
            .repository_name(repository)
            .upload_id(&upload_id)
            .layer_digests(&digest)
            .send()
            .await
        {
            Ok(completed) => Ok(completed
                .layer_digest()
                .unwrap_or(digest.as_str())
                .to_string()),
            Err(error) => {
                if error.as_service_error().is_some_and(|service| {
                    service.is_layer_already_exists_exception()
                }) {
                    Ok(digest.clone())
                } else {
                    Err(service_answered("CompleteLayerUpload", &error))
                }
            }
        }
    })
}

/// The first twelve hex characters of `digest`, which is how the CLI shows an image id.
fn short_digest(digest: &str) -> String {
    let hex = digest.split_once(':').map_or(digest, |(_, hex)| hex);
    hex.chars().take(12).collect()
}

/// The HOST half of what a login names: a login takes a registry address, which may be
/// written with a scheme and a trailing path, and the host is what a push then addresses.
fn registry_host(registry: &str) -> &str {
    let held = registry
        .trim()
        .strip_prefix("https://")
        .or_else(|| registry.trim().strip_prefix("http://"))
        .unwrap_or_else(|| registry.trim());
    held.split('/').next().unwrap_or(held)
}

/// The Region of the registry `host` addresses, or `None` when the host names no Region:
/// this emulator's own endpoint, which serves every Region's registry, or a host that is no
/// Amazon ECR registry host at all.
///
/// A private registry's published host is `<account>.dkr.ecr.<region>.amazonaws.com`, so a
/// reference written on one names the Region its repository lives in; a reference written on
/// the endpoint this emulator announces its registries at ([`logged_in`] is what records that
/// the registry there is one of this emulator's) names none, and the build's own Region is
/// then the one its push addresses.
fn registry_region(host: &str) -> Option<String> {
    let (_, after) = host.split_once(ECR_HOST_INFIX)?;
    let region = after.strip_suffix(ECR_HOST_SUFFIX)?;
    (!region.is_empty() && !region.contains('.')).then(|| region.to_string())
}

/// An image REFERENCE: `[<registry>/]<repository>[:<tag>]`, as a command line writes one.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Reference {
    /// The registry the reference names, and `None` for one that names none.
    registry: Option<String>,
    /// The repository path within that registry.
    repository: String,
    /// The tag, which is [`DEFAULT_TAG`] for a reference that names none.
    tag: String,
}

impl Reference {
    /// The reference `written` names, or `None` when it is not a reference at all.
    ///
    /// The first component is the REGISTRY when it looks like a host — "if the first part
    /// contains a `.` or `:`, or is `localhost`, it is interpreted as a registry" (the
    /// distribution reference grammar) — and part of the repository path otherwise.
    fn parse(written: &str) -> Option<Self> {
        let written = written.trim();
        if written.is_empty() || written.contains(char::is_whitespace) {
            return None;
        }
        let (registry, path) = match written.split_once('/') {
            Some((first, rest))
                if first.contains('.') || first.contains(':') || first == "localhost" =>
            {
                (Some(first.to_string()), rest)
            }
            _ => (None, written),
        };
        // A tag is written after the LAST path component, so a colon inside an earlier one
        // (a port in a registry host) is not one.
        let (repository, tag) = match path.rsplit_once(':') {
            Some((repository, tag)) if !tag.contains('/') => (repository, tag),
            _ => (path, DEFAULT_TAG),
        };
        if repository.is_empty() || tag.is_empty() {
            return None;
        }
        Some(Self {
            registry,
            repository: repository.to_string(),
            tag: tag.to_string(),
        })
    }

    /// The reference as the environment HOLDS the image under: the whole reference with its
    /// tag stated, so `<repo>` and `<repo>:latest` are the one image.
    fn held(&self) -> String {
        format!("{}:{}", self.image(), self.tag)
    }

    /// The image half of the reference — the registry and the repository, without the tag.
    fn image(&self) -> String {
        match &self.registry {
            Some(registry) => format!("{registry}/{}", self.repository),
            None => self.repository.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a reference is `[<registry>/]<repository>[:<tag>]`, where the first component is
    /// the registry only when it looks like a host, and a reference that names no tag names
    /// `latest`.
    ///
    /// The rows differ along what the rule generalises over: whether a registry is written,
    /// whether a tag is written, how deep the repository path is, and whether the host
    /// carries a port.
    #[test]
    fn a_reference_names_a_registry_a_repository_and_a_tag() {
        // (case, written, registry, repository, tag)
        let rows: Vec<(&str, &str, Option<&str>, &str, &str)> = vec![
            ("a bare repository", "app", None, "app", "latest"),
            ("a repository and a tag", "app:v1", None, "app", "v1"),
            (
                "a namespaced repository",
                "team/app:v1",
                None,
                "team/app",
                "v1",
            ),
            (
                "an Amazon ECR registry",
                "000000000000.dkr.ecr.us-west-2.amazonaws.com/app:v2",
                Some("000000000000.dkr.ecr.us-west-2.amazonaws.com"),
                "app",
                "v2",
            ),
            (
                "an Amazon ECR registry with no tag",
                "000000000000.dkr.ecr.eu-west-1.amazonaws.com/team/app",
                Some("000000000000.dkr.ecr.eu-west-1.amazonaws.com"),
                "team/app",
                "latest",
            ),
            (
                "a host carrying a port",
                "localhost:5000/app:v3",
                Some("localhost:5000"),
                "app",
                "v3",
            ),
        ];
        for (case, written, registry, repository, tag) in rows {
            let parsed = Reference::parse(written)
                .unwrap_or_else(|| panic!("case {case:?}: expected a reference for {written}"));
            assert_eq!(
                (
                    parsed.registry.as_deref(),
                    parsed.repository.as_str(),
                    parsed.tag.as_str()
                ),
                (registry, repository, tag),
                "case {case:?}: expected {registry:?} {repository} {tag}, got {parsed:?}"
            );
        }
        for (case, written) in [
            ("nothing at all", ""),
            ("a reference carrying a space", "app :v1"),
            ("an empty tag", "app:"),
        ] {
            assert!(
                Reference::parse(written).is_none(),
                "case {case:?}: expected no reference for {written:?}"
            );
        }
    }

    /// RULE: only the four commands this environment performs are performed, in the forms
    /// they take.
    ///
    /// The rows differ along what the rule generalises over: which command is named, how its
    /// options are written, and which forms carry an option or an operand count the command
    /// does not take.
    #[test]
    fn only_the_performed_docker_commands_are_performed() {
        // (case, the words, what they ask for)
        let rows: Vec<(&str, Vec<&str>, Option<Requested>)> = vec![
            (
                "the documented login",
                vec![
                    "login",
                    "--username",
                    "AWS",
                    "--password-stdin",
                    "registry.example",
                ],
                Some(Requested::Login {
                    registry: "registry.example".to_string(),
                    password: None,
                }),
            ),
            (
                "a login carrying its password",
                vec!["login", "-u", "AWS", "-p", "secret", "registry.example"],
                Some(Requested::Login {
                    registry: "registry.example".to_string(),
                    password: Some("secret".to_string()),
                }),
            ),
            (
                "a login naming no user",
                vec!["login", "--password-stdin", "registry.example"],
                None,
            ),
            (
                "a login naming two password sources",
                vec![
                    "login",
                    "-u",
                    "AWS",
                    "-p",
                    "secret",
                    "--password-stdin",
                    "registry.example",
                ],
                None,
            ),
            (
                "a build with one tag",
                vec!["build", "-t", "app:v1", "."],
                Some(Requested::Build {
                    tags: vec!["app:v1".to_string()],
                    dockerfile: None,
                    build_arguments: ::std::collections::BTreeMap::new(),
                    context: ".".to_string(),
                }),
            ),
            (
                "a build with two tags and a Dockerfile",
                vec![
                    "build",
                    "--tag=app:v1",
                    "-t",
                    "app:latest",
                    "-f",
                    "docker/Dockerfile",
                    "src",
                ],
                Some(Requested::Build {
                    tags: vec!["app:v1".to_string(), "app:latest".to_string()],
                    dockerfile: Some("docker/Dockerfile".to_string()),
                    build_arguments: ::std::collections::BTreeMap::new(),
                    context: "src".to_string(),
                }),
            ),
            (
                // "--build-arg <varname>=<value>": the values the Dockerfile's own `ARG`
                // declarations take, in both spellings the option is written in.
                "a build carrying build arguments",
                vec![
                    "build",
                    "--build-arg",
                    "BODY=a b c",
                    "--build-arg=TAG=v2",
                    "-t",
                    "app:v2",
                    ".",
                ],
                Some(Requested::Build {
                    tags: vec!["app:v2".to_string()],
                    dockerfile: None,
                    build_arguments: [("BODY", "a b c"), ("TAG", "v2")]
                        .into_iter()
                        .map(|(name, value)| (name.to_string(), value.to_string()))
                        .collect(),
                    context: ".".to_string(),
                }),
            ),
            (
                // A `--build-arg` naming no value takes it from the environment the builder
                // itself runs in, which this program is handed none of.
                "a build argument with no value",
                vec!["build", "--build-arg", "BODY", "-t", "app:v2", "."],
                None,
            ),
            (
                "a build naming no context",
                vec!["build", "-t", "app:v1"],
                None,
            ),
            (
                "a build carrying an option this program does not take",
                vec!["build", "--no-cache", "-t", "app:v1", "."],
                None,
            ),
            (
                "a tag",
                vec!["tag", "app:v1", "registry.example/app:v1"],
                Some(Requested::Tag {
                    source: "app:v1".to_string(),
                    target: "registry.example/app:v1".to_string(),
                }),
            ),
            (
                "a push",
                vec!["push", "registry.example/app:v1"],
                Some(Requested::Push {
                    reference: "registry.example/app:v1".to_string(),
                }),
            ),
            ("another command of the CLI", vec!["pull", "app:v1"], None),
            ("no command at all", vec![], None),
        ];
        for (case, words, expected) in rows {
            let arguments: Vec<String> = words.iter().map(|word| (*word).to_string()).collect();
            let asked = requested(&arguments);
            assert_eq!(
                asked, expected,
                "case {case:?}: expected {expected:?}, got {asked:?}"
            );
        }
    }

    /// RULE: the Region a push addresses is the one the registry host it names carries, and a
    /// host that names none — the endpoint this emulator announces its registries at — leaves
    /// the push to the build's own Region.
    #[test]
    fn a_push_addresses_the_region_its_registry_host_names() {
        // (case, the host, the Region it names)
        let rows: Vec<(&str, &str, Option<&str>)> = vec![
            (
                "a private registry host",
                "000000000000.dkr.ecr.us-west-2.amazonaws.com",
                Some("us-west-2"),
            ),
            (
                "another Region",
                "123456789012.dkr.ecr.eu-central-1.amazonaws.com",
                Some("eu-central-1"),
            ),
            ("this emulator's own endpoint", "127.0.0.1:9297", None),
            ("a loopback name", "localhost:9297", None),
            ("a public registry", "public.ecr.aws", None),
            ("the Docker Hub", "registry-1.docker.io", None),
            (
                "a host with nothing where the Region goes",
                "000000000000.dkr.ecr..amazonaws.com",
                None,
            ),
        ];
        for (case, host, expected) in rows {
            let region = registry_region(host);
            assert_eq!(
                region.as_deref(),
                expected,
                "case {case:?}: expected {expected:?}, got {region:?}"
            );
        }
    }

    /// RULE: a login takes a registry ADDRESS, and the host it carries is what a push then
    /// addresses.
    #[test]
    fn a_login_records_the_host_a_push_addresses() {
        // (case, what the login named, the host it records)
        let rows: Vec<(&str, &str, &str)> = vec![
            (
                "a bare host",
                "000000000000.dkr.ecr.us-west-2.amazonaws.com",
                "000000000000.dkr.ecr.us-west-2.amazonaws.com",
            ),
            (
                "a host written as a URL",
                "https://000000000000.dkr.ecr.us-west-2.amazonaws.com",
                "000000000000.dkr.ecr.us-west-2.amazonaws.com",
            ),
            (
                "a URL carrying a path",
                "https://000000000000.dkr.ecr.us-west-2.amazonaws.com/v2/",
                "000000000000.dkr.ecr.us-west-2.amazonaws.com",
            ),
        ];
        for (case, named, expected) in rows {
            let host = registry_host(named);
            assert_eq!(
                host, expected,
                "case {case:?}: expected {expected:?}, got {host:?}"
            );
        }
    }
}

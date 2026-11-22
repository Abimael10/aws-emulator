// SPDX-License-Identifier: CC0-1.0

//! How this service builds the modelled exceptions it answers a refused request with:
//! the members every error body carries, and the wording it words the recurring
//! refusals with.
//!
//! Each of this model's exception shapes carries `Message`, `Code` and `Logref`
//! (`smithy_query(query="shape",shape="ResourceNotFoundException")`), and an error body
//! this service sends names the failure TWICE: once in the protocol envelope's `__type`
//! and once in the shape's own `Code` member, both as the exception's shape name. A
//! client that classifies a failure from the body's `Code` has to reach the same class
//! as one that reads the envelope, so neither may be left out. `Logref` is the request
//! identifier the real service correlates its own logs by; this emulator mints no such
//! identifier, so that member stays absent.
//!
//! Every handler constructs its modelled errors through [`ModeledError::with_message`]
//! or one of the named constructors below, so the rule holds for all of them rather
//! than one handler at a time.

use crate::ops::types::*;

/// A modelled exception of this service, constructed with every member the service
/// fills in.
pub trait ModeledError: Sized {
    /// This exception's shape name: the value of its `Code` member, and the value the
    /// error envelope carries in `__type`.
    const CODE: &'static str;

    /// This exception carrying `message`, with `Code` set to [`Self::CODE`].
    fn with_message(message: impl Into<String>) -> Self;
}

/// Implements [`ModeledError`] for each named exception shape. The shape's name is the
/// Rust type's name — the generated types in `ops::types` are one per modelled shape,
/// named after it — and every member beyond `Message` and `Code` is left as the shape's
/// default, i.e. absent.
macro_rules! impl_modeled_error {
    ($($exception:ident),+ $(,)?) => {
        $(
            impl ModeledError for $exception {
                const CODE: &'static str = stringify!($exception);

                fn with_message(message: impl Into<String>) -> Self {
                    Self {
                        message: Some(message.into()),
                        code: Some(Self::CODE.to_string()),
                        ..Self::default()
                    }
                }
            }
        )+
    };
}

// Every exception shape this service models, so an operation implemented later builds
// its own errors the same way the collection and image operations build theirs.
impl_modeled_error!(
    AccessDeniedException,
    ConflictException,
    HumanLoopQuotaExceededException,
    IdempotentParameterMismatchException,
    ImageTooLargeException,
    InternalServerError,
    InvalidImageFormatException,
    InvalidManifestException,
    InvalidPaginationTokenException,
    InvalidParameterException,
    InvalidPolicyRevisionIdException,
    InvalidS3ObjectException,
    LimitExceededException,
    MalformedPolicyDocumentException,
    ProvisionedThroughputExceededException,
    ResourceAlreadyExistsException,
    ResourceInUseException,
    ResourceNotFoundException,
    ResourceNotReadyException,
    ServiceQuotaExceededException,
    SessionNotFoundException,
    ThrottlingException,
    VideoTooLargeException,
);

/// Why a handler could not reach this service's store: a server fault, since nothing
/// about the request caused it.
pub const STORE_UNAVAILABLE_MESSAGE: &str = "the Rekognition collection store is unavailable";

/// The fixed text a request naming a `FaceId` the collection does not hold is refused
/// with. A face id the collection does not hold is a parameter value the service
/// rejects — the collection it names exists — so the class is
/// `InvalidParameterException` rather than `ResourceNotFoundException`, which this API
/// keeps for the collection itself being absent. The text names no id: it interpolates
/// nothing from the request.
pub const FACE_ID_NOT_IN_COLLECTION_MESSAGE: &str = "faceId was not found in the collection.";

/// The pagination token refusal: a `NextToken` this service's lists did not mint names
/// no position in them.
pub const INVALID_PAGINATION_TOKEN_MESSAGE: &str = "Invalid pagination token";

/// AWS's fixed wording for the `InvalidParameterException` class: "Input parameter
/// violated a constraint" is what the exception SHAPE documents, while the `Message` the
/// service puts on the wire for it is this one sentence. It interpolates nothing — neither
/// a request value nor the member at fault — so every refusal of this class carries it
/// byte for byte.
pub const INVALID_PARAMETERS_MESSAGE: &str = "Request has invalid parameters";

/// The fixed text `CompareFaces` refuses an image carrying no comparable face with — it
/// "returns an InvalidParameterException error" when "no faces are detected in the source
/// or target images". That operation answers the condition with the class's generic
/// sentence: it names neither which of the two images it was nor anything else from the
/// request, both of which the caller already holds.
///
/// A search BY IMAGE words the same condition differently
/// ([`NO_FACE_TO_SEARCH_BY_MESSAGE`]), which is why this is a wording per refusal and not
/// one sentence for every operation that needs a face — as with the two wordings of a
/// collection that does not exist above.
pub const NO_FACE_IN_IMAGE_MESSAGE: &str = INVALID_PARAMETERS_MESSAGE;

/// The fixed text a search BY IMAGE is refused with when the submitted image carries no
/// face to search the collection by: "if no faces are detected in the input image,
/// SearchFacesByImage returns an InvalidParameterException error"
/// (`smithy_query(query="operation",operation="SearchFacesByImage")`). A search by image
/// starts from the single largest face of the image, so an image that yields none leaves
/// the operation nothing to search by, and the refusal reports that condition and the
/// number of faces the operation needs. It interpolates nothing — no collection id,
/// threshold, cap or count — so every refusal of this condition carries it byte for byte.
pub const NO_FACE_TO_SEARCH_BY_MESSAGE: &str =
    "There are no faces in the image. Should be at least 1.";

/// The server fault a handler answers with when this service's store cannot be reached.
pub fn store_unavailable() -> InternalServerError {
    InternalServerError::with_message(STORE_UNAVAILABLE_MESSAGE)
}

/// A collection the store does not hold, as the operations ON a collection as a
/// resource — `DescribeCollection` and `DeleteCollection` — word it:
/// `Collection:<CollectionId> not found!`, the request's own id between two pieces of
/// fixed text.
pub fn collection_not_found(collection_id: &str) -> ResourceNotFoundException {
    ResourceNotFoundException::with_message(format!("Collection:{collection_id} not found!"))
}

/// A collection the store does not hold, as the operations on the FACES of a collection
/// — `IndexFaces`, `ListFaces`, `DeleteFaces`, `SearchFaces` — word it. The two paths
/// word the same class differently on the wire, so each operation answers with the
/// wording the service answers it with rather than with one invented for both.
pub fn collection_id_does_not_exist(collection_id: &str) -> ResourceNotFoundException {
    ResourceNotFoundException::with_message(format!(
        "The collection id: {collection_id} does not exist"
    ))
}

/// The fixed text an `Id` naming no celebrity of this service's catalogue is refused with:
/// AWS's own wording for that class, which interpolates nothing — not even the id, which the
/// caller sent. The class is `ResourceNotFoundException`: a celebrity is the resource
/// `GetCelebrityInfo` is given, and the exception the model declares for it describes exactly
/// this — "the resource specified in the request cannot be found".
pub const CELEBRITY_NOT_FOUND_MESSAGE: &str = "No Celebrity found with passed Id";

/// An `Id` this service's celebrity catalogue holds no entry under; see
/// [`CELEBRITY_NOT_FOUND_MESSAGE`].
pub fn celebrity_not_found() -> ResourceNotFoundException {
    ResourceNotFoundException::with_message(CELEBRITY_NOT_FOUND_MESSAGE)
}

/// A `FaceId` the named collection does not hold; see
/// [`FACE_ID_NOT_IN_COLLECTION_MESSAGE`] for why this is a parameter refusal.
pub fn face_id_not_in_collection() -> InvalidParameterException {
    InvalidParameterException::with_message(FACE_ID_NOT_IN_COLLECTION_MESSAGE)
}

/// A `NextToken` no list of this service minted.
pub fn invalid_pagination_token() -> InvalidPaginationTokenException {
    InvalidPaginationTokenException::with_message(INVALID_PAGINATION_TOKEN_MESSAGE)
}

/// A resource ARN no live resource of this store carries, as the tagging operations word
/// it: the request's own ARN between two pieces of fixed text. One wording for the
/// operations that are given a resource by ARN and resolve it the same way — the three
/// tagging ones and the ones that work on a project's policies — since they all report the
/// same thing: the ARN you named is not one of this account's. The operation that works on
/// the PROJECT as a resource words the same class differently
/// ([`project_arn_not_found`]), the way a recorded refusal of it does.
pub fn resource_arn_not_found(resource_arn: &str) -> ResourceNotFoundException {
    ResourceNotFoundException::with_message(format!("The resource {resource_arn} does not exist"))
}

/// A `ProjectArn` naming no project this account has, as `DeleteProject` words it: the
/// request's own ARN between two pieces of fixed text, naming the KIND of resource that is
/// missing.
///
/// It is the `Message` a recorded AWS refusal of a delete of a project that is already gone
/// carries verbatim, which is why this path words the class its own way rather than with the
/// sentence the tagging operations answer ([`resource_arn_not_found`]) — the same split as
/// the two wordings of a collection that does not exist above.
pub fn project_arn_not_found(project_arn: &str) -> ResourceNotFoundException {
    ResourceNotFoundException::with_message(format!("Project {project_arn} does not exist"))
}

/// A submitted image in which a comparison found no face to work on; see
/// [`NO_FACE_IN_IMAGE_MESSAGE`].
pub fn no_face_in_image() -> InvalidParameterException {
    InvalidParameterException::with_message(NO_FACE_IN_IMAGE_MESSAGE)
}

/// A submitted image a search by image is left no face to search by from; see
/// [`NO_FACE_TO_SEARCH_BY_MESSAGE`].
pub fn no_face_to_search_by() -> InvalidParameterException {
    InvalidParameterException::with_message(NO_FACE_TO_SEARCH_BY_MESSAGE)
}

/// A request whose parameters do not identify what the operation is to work on; see
/// [`INVALID_PARAMETERS_MESSAGE`].
///
/// A `UserId` the named collection does not hold is one such parameter: the collection
/// the request named exists, so what the service rejects is the VALUE it was given and
/// not a resource it could not find — the same reasoning, and the same class, as a
/// `FaceId` the collection does not hold ([`face_id_not_in_collection`]), with
/// `ResourceNotFoundException` kept for the collection itself being absent.
pub fn invalid_parameters() -> InvalidParameterException {
    InvalidParameterException::with_message(INVALID_PARAMETERS_MESSAGE)
}

/// A `UserId` the named collection already holds, as `CreateUser` refuses it: the
/// request's own id between two pieces of fixed text, worded the way this service words
/// the same class for a collection id a create repeats ("The collection id: … already
/// exists"), since both say "the id you asked to create is taken".
///
/// The class is `ConflictException`, which this API models for exactly this: "A User
/// with the same Id already exists within the collection"
/// (`smithy_query(query="doc",shape="ConflictException")`).
pub fn user_id_already_exists(user_id: &str) -> ConflictException {
    ConflictException::with_message(format!("The user id: {user_id} already exists"))
}

/// A `UserId` the named collection holds no user under, as the operations ON a user of a
/// collection word it: the request's own id between two pieces of fixed text, worded the
/// way this service words the same class for a collection id that names nothing ("The
/// collection id: … does not exist") and as the mirror of the create's own refusal
/// ([`user_id_already_exists`]), since all three say "the id you named is not one this
/// store holds".
///
/// The class is `ResourceNotFoundException`: a user is the resource the request is given,
/// and the exception the model declares for it describes exactly this — "the resource
/// specified in the request cannot be found".
pub fn user_id_does_not_exist(user_id: &str) -> ResourceNotFoundException {
    ResourceNotFoundException::with_message(format!("The user id: {user_id} does not exist"))
}

/// The fixed text an idempotency-token reuse is refused with.
///
/// It describes none of the request, and it does not restate the rule — "a
/// `ClientRequestToken` input parameter was reused with an operation, but at least one
/// of the other input parameters is different from the previous call" is what the
/// exception SHAPE documents
/// (`smithy_query(query="doc",shape="IdempotentParameterMismatchException")`), not what
/// the service puts in `Message`. This is the `Message` a recorded AWS `CreateUser`
/// refusal of a reused token carries verbatim (the recorded exchange this refusal was
/// implemented against, `rekognition.create-user-idempotent-token-mismatch-rejected`),
/// and the wire message is part of the contract, so the service answers what AWS
/// answers rather than a better-worded sentence of its own. It interpolates nothing:
/// the caller sent both requests and so already holds every value that differs.
pub const IDEMPOTENT_PARAMETER_MISMATCH_MESSAGE: &str = "Service Unavailable";

/// A `ClientRequestToken` reused for a request that differs from the one it already
/// committed; one wording for every operation that takes one, since the rule is one.
pub fn idempotent_parameter_mismatch() -> IdempotentParameterMismatchException {
    IdempotentParameterMismatchException::with_message(IDEMPOTENT_PARAMETER_MISMATCH_MESSAGE)
}

/// The fixed text a start refused because too many video analyses are already running is
/// worded with, with the ceiling it names taken from
/// [`super::video_jobs::MAX_CONCURRENT_JOBS`] rather than written into the sentence.
///
/// `LimitExceededException` documents the class — "if you start too many jobs concurrently,
/// subsequent calls to start operations … will raise a LimitExceededException exception …
/// until the number of concurrently running jobs is below the Amazon Rekognition service
/// limit" (`smithy_query(query="doc",shape="LimitExceededException")`) — but no refusal of it
/// is recorded, so unlike [`IDEMPOTENT_PARAMETER_MISMATCH_MESSAGE`] this wording is not taken
/// from an exchange. It states the quota that was reached and nothing of the request, and it
/// is one wording for every `Start…` operation of the family, since the ceiling is one.
pub fn video_job_concurrency_limit_message() -> String {
    format!(
        "The maximum number of concurrently running video analysis jobs ({}) has been reached",
        super::video_jobs::MAX_CONCURRENT_JOBS
    )
}

/// The fixed text a project policy document that is not a policy at all is refused with.
///
/// It is the `Message` a recorded AWS `PutProjectPolicy` refusal of an unparseable document
/// carries verbatim, and it interpolates nothing: neither the document nor where it stopped
/// being a policy, both of which the caller sent.
pub const MALFORMED_POLICY_MESSAGE: &str = "Request contains a malformed policy.";

/// The fixed text a project policy document whose resources do not resolve is refused with
/// — recorded, like [`MALFORMED_POLICY_MESSAGE`], and interpolating nothing either: it names
/// neither which resource nor what it should have been.
pub const INVALID_POLICY_RESOURCE_MESSAGE: &str =
    "Request contains an invalid resource in the provided policy.";

/// A project policy document that is not a policy: it does not parse as JSON, or what it
/// parses to is not a statement naming the resources it grants on.
pub fn malformed_policy_document() -> MalformedPolicyDocumentException {
    MalformedPolicyDocumentException::with_message(MALFORMED_POLICY_MESSAGE)
}

/// A project policy document that is a policy, but names a resource that is not a model
/// version of the project the policy would be attached to.
pub fn invalid_policy_resource() -> MalformedPolicyDocumentException {
    MalformedPolicyDocumentException::with_message(INVALID_POLICY_RESOURCE_MESSAGE)
}

/// The fixed text a write that does not name the policy's current revision is refused with:
/// the sentence the exception shape itself words that class with
/// (`smithy_query(query="doc",shape="InvalidPolicyRevisionIdException")`). It interpolates
/// nothing — in particular not the current revision, which a caller holding a stale one is
/// not told by the refusal.
pub const INVALID_POLICY_REVISION_ID_MESSAGE: &str =
    "The supplied revision id for the project policy is invalid.";

/// A `PolicyRevisionId` that is not the revision the named policy currently carries.
pub fn invalid_policy_revision_id() -> InvalidPolicyRevisionIdException {
    InvalidPolicyRevisionIdException::with_message(INVALID_POLICY_REVISION_ID_MESSAGE)
}

/// The fixed text an operation given a `PolicyName` the named project has no policy under is
/// refused with: the two request values between three pieces of fixed text. A policy is
/// identified by its name WITHIN a project, so the refusal names both halves of that identity
/// — the name asked for and the project it was looked for in — and takes each from the
/// request and the store rather than from any one exchange.
pub fn project_policy_not_found(
    policy_name: &str,
    project_name: &str,
) -> ResourceNotFoundException {
    ResourceNotFoundException::with_message(format!(
        "The specified policy {policy_name} does not exist for project {project_name}."
    ))
}

/// The fixed text a DELETE that does not name the policy's current revision is refused with.
///
/// `DeleteProjectPolicy` words this class as what the caller has to do about it — re-read the
/// policy's latest revision and try again — rather than as the shape's own sentence
/// ([`INVALID_POLICY_REVISION_ID_MESSAGE`], which is how a WRITE of a policy words it), so the
/// two refusals of the same modelled exception keep the wording of the operation that answers
/// them. The policy's name comes from the request; the revision the policy actually carries is
/// not disclosed, which is why the caller is told to go and read it.
pub fn project_policy_revision_is_not_latest(
    policy_name: &str,
) -> InvalidPolicyRevisionIdException {
    InvalidPolicyRevisionIdException::with_message(format!(
        "The policy cannot be deleted. Please enter the latest policy revision ID associated \
         with the policy {policy_name} and try again."
    ))
}

/// A `PolicyName` the project already has a policy under, on a write that did not name a
/// revision to replace: attaching under a name that is taken is a collision, and updating
/// the policy that holds the name is what naming its revision asks for.
///
/// Worded the way this service words the same class for a name a create repeats ("The
/// project name: … already exists", `CreateProject`), with the request's own policy name
/// between two pieces of fixed text.
pub fn project_policy_already_exists(policy_name: &str) -> ResourceAlreadyExistsException {
    ResourceAlreadyExistsException::with_message(format!(
        "The project policy: {policy_name} already exists"
    ))
}

/// The fixed text a project that still manages model versions is refused the DELETE of
/// with, with the project's own name taken from the store.
///
/// The precondition is documented on the operation itself: "to delete a project you must
/// first delete all models or adapters associated with the project"
/// (`smithy_query(query="operation",operation="DeleteProject")`), and
/// `ResourceInUseException` — "the specified resource is already being used" — is the class
/// the operation declares for it. No refusal of this condition is recorded, so the sentence
/// is this service's own statement of the precondition rather than one taken from an
/// exchange; it is worded the way this service words the other refusal a caller has to go
/// and do something about first ([`project_policy_revision_is_not_latest`]).
pub fn project_has_associated_versions(project_name: &str) -> ResourceInUseException {
    ResourceInUseException::with_message(format!(
        "The project cannot be deleted. Please delete all models and adapters associated with \
         the project {project_name} and try again."
    ))
}

/// A `DatasetArn` naming no dataset this account has, as the operations ON a dataset word it:
/// the request's own ARN between two pieces of fixed text, naming the KIND of resource that is
/// missing and what to check.
///
/// It is the `Message` a recorded AWS refusal of a read of a dataset that is already gone
/// carries verbatim, which is why this path words the class its own way rather than with the
/// sentence a project ARN that names nothing is refused with ([`project_arn_not_found`]) — the
/// same split as the two wordings of a collection that does not exist above.
///
/// `ResourceNotFoundException` is the class the operations given a dataset by ARN declare for
/// it — "the resource specified in the request cannot be found".
pub fn dataset_arn_not_found(dataset_arn: &str) -> ResourceNotFoundException {
    ResourceNotFoundException::with_message(format!(
        "The dataset {dataset_arn} doesn't exist. Check that the dataset exists."
    ))
}

/// A `DatasetType` the named project already holds a dataset of: a project has at most one
/// training and one test dataset, so a second create of the same half is a collision rather
/// than a second dataset.
///
/// Worded the way this service words the same class for an identifier a create repeats ("The
/// project name: … already exists", "The collection id: … already exists"), with the SCOPE the
/// type is unique in named as well, since a dataset type identifies a dataset only within its
/// project.
pub fn dataset_type_already_exists(
    dataset_type: &str,
    project_name: &str,
) -> ResourceAlreadyExistsException {
    ResourceAlreadyExistsException::with_message(format!(
        "The dataset type: {dataset_type} already exists for the project: {project_name}"
    ))
}

/// The fixed text a dataset that is not in a state it can be deleted from is refused the
/// DELETE of, with the status it really holds taken from the store.
///
/// The precondition is documented on the operation itself: "you can't delete a dataset while
/// it is creating (`Status` = `CREATE_IN_PROGRESS`) or if the dataset is updating (`Status` =
/// `UPDATE_IN_PROGRESS`)" (`smithy_query(query="doc",shape="DeleteDataset")`), and
/// `ResourceInUseException` — "the specified resource is already being used" — is the class the
/// operation declares for it. It is worded the way this service words the other refusal a
/// caller has to wait out ([`project_has_associated_versions`]).
pub fn dataset_is_not_ready_to_delete(status: &str) -> ResourceInUseException {
    ResourceInUseException::with_message(format!(
        "The dataset cannot be deleted while its status is {status}. Please try again once the \
         dataset operation has completed."
    ))
}

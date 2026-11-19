// SPDX-License-Identifier: CC0-1.0

//! An OTA UPDATE, as this service persists it: its identity, the job it deploys through,
//! the job document a targeted device downloads its files by, and the record the account
//! keeps of it.
//!
//! An OTA update is a roll-out of package-version files to a set of targets, so it lives
//! beside the package versions ([`super::RegisteredPackageVersion`]) whose files it hands
//! out and the job registry ([`ota_update_job_id`]'s job) that delivers it. This is the
//! half of the record the OTA operations write and read; the state struct that holds them
//! is the parent module's.

use super::*;

/// The ARN of an OTA update:
/// `arn:aws:iot:<region>:<account>:otaupdate/<otaUpdateId>`.
///
/// `otaupdate` (one word, lowercase) is the resource type AWS IoT's OTA-update ARNs
/// use — the resource an `iot:GetOTAUpdate` policy statement names — beside `thing/`
/// for a thing (`OTAUpdateArn` is an opaque string in the model,
/// `smithy_query(query="shape",shape="OTAUpdateArn")`, so the form comes from the
/// service's documented ARN layout); the namespace and account segments are the ones
/// [`thing_arn`] documents.
pub fn ota_update_arn(region: &str, ota_update_id: &str) -> String {
    ::emulator_server::emulator_arn("iot", region, &format!("otaupdate/{ota_update_id}"))
}

/// The prefix of the AWS IoT job id an OTA update deploys through.
///
/// AWS documents that the OTA Update Manager creates the update's job under
/// `AFR_OTA-<otaUpdateId>`, which is the id a targeted device sees on the job it is
/// handed; `CreateOTAUpdate` reports it as `awsIotJobId`.
pub const OTA_UPDATE_JOB_ID_PREFIX: &str = "AFR_OTA-";

/// The id of the AWS IoT job the OTA update `ota_update_id` deploys through.
///
/// It is DERIVED from the update's own id, so the job an update owns is found from the
/// update and never recorded twice. `OTAUpdateId` is `len 1..128, ^[a-zA-Z0-9_-]+$`
/// and `JobId` is `len 1..64, ^[a-zA-Z0-9_-]+$`, so the prefix keeps the character
/// class; an id long enough to overflow `JobId` is refused before this is called (see
/// [`MAX_OTA_UPDATE_ID_LEN`]).
pub fn ota_update_job_id(ota_update_id: &str) -> String {
    format!("{OTA_UPDATE_JOB_ID_PREFIX}{ota_update_id}")
}

/// The longest `otaUpdateId` whose derived job id still fits the model's `JobId`.
///
/// `OTAUpdateId` is modelled `len 1..128` while `JobId` — the shape the derived
/// `awsIotJobId` has to satisfy — is `len 1..64`, so an id past this length could not
/// be deployed by a job that any `/jobs/{jobId}` request can address, and
/// `CreateOTAUpdate` refuses it with the modelled `InvalidRequestException` instead of
/// recording an update whose job is unreachable.
pub const MAX_OTA_UPDATE_ID_LEN: usize = 64 - OTA_UPDATE_JOB_ID_PREFIX.len();

/// How many `files` ONE `CreateOTAUpdate` may carry.
///
/// `OTAUpdateFiles` is modelled `len 1..` — a minimum and NO maximum — so without a
/// ceiling the request chooses how much the stored record costs (each file carries its
/// own location, code-signing material and attribute map). The figure is the
/// emulator's own, for the same reason [`MAX_JOB_TARGETS`] exists, and a request past
/// it answers the modelled `LimitExceededException`.
pub const MAX_OTA_UPDATE_FILES: usize = 50;

/// The one member of an OTA job document every other member hangs off.
///
/// The OTA Update Manager writes the document it generates as a single `afr_ota`
/// object, which is the member a device's OTA agent reads the update out of.
pub const OTA_JOB_DOCUMENT_ROOT: &str = "afr_ota";

/// The job document the AWS IoT job an OTA update deploys through carries.
///
/// `CreateOTAUpdate` models no `document`, `documentSource` or `documentParameters`
/// member at all: the OTA service GENERATES the document of the job it deploys out of
/// the request, which is why a caller never supplies one. This is that generation, and
/// everything it states comes from the request:
///
/// * `protocols` — "The protocol used to transfer the OTA update image" — as the
///   request named them, so a device learns which transfers the update allows; a
///   request that named none states none rather than an invented default.
/// * `streamname` — the `streamId` of the stream the update's files are served from,
///   when a file's `fileLocation` names a stream.
/// * one `files` entry per requested file, in request order, carrying that file's
///   `fileName` as the path the device writes (`filepath`), its id (`fileid`: the
///   `fileLocation.stream.fileId` of a streamed file, otherwise the file's position in
///   `files`), the `fileType` "you can include in the job document to allow your
///   devices to identify the type of file received", the download location its
///   `s3Location` names ([`ota_file_download_url`]) and the code-signing material the
///   request supplied for it ([`insert_ota_file_code_signing`]).
///
/// The download location is an address a DEVICE then fetches, so it is minted against
/// the endpoint this emulator is reached on ([`ota_file_download_url`]) — `ctx` is what
/// carries that endpoint and the signing key.
///
/// A file's `attributes` and the update's `additionalParameters` are documented as NOT
/// sent to devices as part of the job document, so neither appears. No `filesize` is
/// stated either: nothing here holds the object a file names, so its size is not known
/// and a measurement nothing took would be invented.
///
/// `Err` carries the length in bytes the document reached when `JobDocument` could not
/// hold it ([`MAX_JOB_DOCUMENT_BYTES`]), so a request whose files would deploy a job
/// whose document no reader can report is refused rather than recorded.
pub fn ota_update_job_document(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    protocols: Option<&[String]>,
    presigned_url_config: Option<&AwsJobPresignedUrlConfig>,
    files: &[OTAUpdateFile],
) -> Result<String, usize> {
    let expires_in_sec = presigned_url_config.and_then(|config| config.expires_in_sec);
    let mut entries: Vec<::serde_json::Value> = Vec::with_capacity(files.len());
    let mut stream_name: Option<String> = None;
    for (index, file) in files.iter().enumerate() {
        let location = file.file_location.as_ref();
        let stream = location.and_then(|location| location.stream.as_ref());
        let s3_location = location.and_then(|location| location.s3_location.as_ref());
        if stream_name.is_none() {
            stream_name = stream.and_then(|stream| stream.stream_id.clone());
        }

        let mut entry = ::serde_json::Map::new();
        if let Some(file_name) = &file.file_name {
            entry.insert("filepath".to_string(), ::serde_json::json!(file_name));
        }
        // A streamed file is addressed by the id it has IN that stream; an S3 object is
        // not addressed by an id at all, so the file's own position in `files` is what
        // tells one entry of the document from another.
        let file_id = stream.and_then(|stream| stream.file_id).map_or_else(
            || i64::try_from(index).unwrap_or(i64::MAX),
            i64::from,
        );
        entry.insert("fileid".to_string(), ::serde_json::json!(file_id));
        if let Some(file_type) = file.file_type {
            entry.insert("fileType".to_string(), ::serde_json::json!(file_type));
        }
        if let Some(url) = s3_location
            .and_then(|location| ota_file_download_url(ctx, region, location, expires_in_sec))
        {
            entry.insert("update_data_url".to_string(), ::serde_json::json!(url));
        }
        if let Some(code_signing) = file.code_signing.as_ref() {
            insert_ota_file_code_signing(&mut entry, code_signing);
        }
        entries.push(::serde_json::Value::Object(entry));
    }

    let mut update = ::serde_json::Map::new();
    if let Some(protocols) = protocols {
        update.insert("protocols".to_string(), ::serde_json::json!(protocols));
    }
    if let Some(stream_name) = stream_name {
        update.insert("streamname".to_string(), ::serde_json::json!(stream_name));
    }
    update.insert("files".to_string(), ::serde_json::Value::Array(entries));

    let document = ::serde_json::json!({ OTA_JOB_DOCUMENT_ROOT: update }).to_string();
    if document.len() > MAX_JOB_DOCUMENT_BYTES {
        return Err(document.len());
    }
    Ok(document)
}

/// The lifetime a pre-signed download URL is minted with when the request asked for
/// none: 1800 seconds.
///
/// `AwsJobPresignedUrlConfig$expiresInSec` states it — "How long (in seconds) pre-signed
/// URLs are valid. Valid values are 60 - 3600, the default value is 1800 seconds" — so it
/// is the service's documented default rather than a span chosen here.
pub const DEFAULT_PRESIGNED_URL_EXPIRY_SEC: u64 = 1800;

/// The shortest lifetime that member documents for a pre-signed URL ("Valid values are
/// 60 - 3600").
pub const MIN_PRESIGNED_URL_EXPIRY_SEC: u64 = 60;

/// The longest lifetime that member documents for a pre-signed URL ("Valid values are
/// 60 - 3600").
pub const MAX_PRESIGNED_URL_EXPIRY_SEC: u64 = 3600;

/// How long the pre-signed download URLs of one update stay valid: the lifetime the
/// request asked for, held inside the range the member documents, and the documented
/// default for a request that asked for none.
///
/// `ExpiresInSeconds` carries no modelled range, so a value outside the documented one
/// reaches here; it is held at the nearer published bound rather than minting a URL that
/// has already expired or outlives what the member promises.
fn presigned_url_lifetime(expires_in_sec: Option<i64>) -> Duration {
    let seconds = expires_in_sec
        .map(|value| u64::try_from(value).unwrap_or(MIN_PRESIGNED_URL_EXPIRY_SEC))
        .unwrap_or(DEFAULT_PRESIGNED_URL_EXPIRY_SEC)
        .clamp(MIN_PRESIGNED_URL_EXPIRY_SEC, MAX_PRESIGNED_URL_EXPIRY_SEC);
    Duration::from_secs(seconds)
}

/// The location a DEVICE downloads the S3 object a requested file names from: that
/// object's own address on the Amazon S3 THIS EMULATOR serves, pre-signed.
///
/// `update_data_url` is not a label an agent reads — it is the address it then FETCHES,
/// and the member that governs it is about pre-signing: "How long (in seconds) pre-signed
/// URLs are valid" (`AwsJobPresignedUrlConfig$expiresInSec`). So the value has to be a URL
/// a client of this emulator can follow, which the host AWS serves its own buckets at is
/// not — a device sent there leaves the emulator and downloads nothing — and it has to
/// carry a signature this emulator verifies, which a hand-written `X-Amz-Expires` is not.
/// Both come from the framework's URL signer: it signs for the base URL this emulator is
/// reached on, and the object's bucket and key travel in the PATH, the way a path-style
/// Amazon S3 URL carries them. The lifetime is [`presigned_url_lifetime`] and the Region
/// signed for is the one the update was created in, so the link's credential scope names
/// the same Region as the update's own ARN.
///
/// "The S3 bucket version" is carried as the `versionId` query Amazon S3 reads a pinned
/// version from, and only when the location pins one.
///
/// `None` for a location missing its bucket or its key: that is no location a device
/// could fetch from, so the entry states none rather than a URL with a hole in it.
fn ota_file_download_url(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    location: &S3Location,
    expires_in_sec: Option<i64>,
) -> Option<String> {
    let bucket = location.bucket.as_deref()?;
    let key = location.key.as_deref()?;
    let path = format!("/{bucket}/{key}");
    let mut request = ::smithy_server_runtime::SignedUrlRequest::new(
        ::smithy_server_runtime::http::Method::GET,
        super::thing_registration_reports::S3_SIGNING_NAME,
        &path,
    )
    .region(region);
    if let Some(version) = location.version.as_deref() {
        request = request.query("versionId", version);
    }
    Some(
        match ctx
            .url_signer()
            .sign(request, presigned_url_lifetime(expires_in_sec))
        {
            Ok(signed) => signed.as_str().to_string(),
            // The lifetime is held inside the signer's own range and the Region is
            // narrowed before it gets here, so this is unreachable in practice; the
            // object is still served at that address, so the address is what the entry
            // states rather than no location at all. The endpoint is `scheme://host[:port]`
            // with no path of its own, and a trailing separator is dropped so the object's
            // path is the only one the URL carries.
            Err(_) => format!("{}{path}", ctx.endpoint().as_str().trim_end_matches('/')),
        },
    )
}

/// Write the code-signing material a requested file carries into its document entry.
///
/// A device verifies the image it downloaded before it installs it, so what the
/// request said about signing that file is what its entry states: the path the signing
/// certificate has on the device (`certfile` — the `certificatePathOnDevice` of the
/// signing profile when the request named one, otherwise the name of the certificate
/// chain it supplied), and a custom signature under the key naming the algorithms it
/// was made with (`sig-<hashAlgorithm>-<signatureAlgorithm>`, lowercased, which is how
/// a device looks the signature up).
///
/// A signature whose algorithms the request did not name is NOT written: nothing here
/// knows which key a device would read it under. An `awsSignerJobId` or a signing
/// profile names a signing job that produced a signature elsewhere, and no signature
/// is held here, so neither puts one in the document.
fn insert_ota_file_code_signing(
    entry: &mut ::serde_json::Map<String, ::serde_json::Value>,
    code_signing: &CodeSigning,
) {
    let custom = code_signing.custom_code_signing.as_ref();
    let certificate_path = code_signing
        .start_signing_job_parameter
        .as_ref()
        .and_then(|parameter| parameter.signing_profile_parameter.as_ref())
        .and_then(|profile| profile.certificate_path_on_device.clone())
        .or_else(|| {
            custom
                .and_then(|custom| custom.certificate_chain.as_ref())
                .and_then(|chain| chain.certificate_name.clone())
        });
    if let Some(certificate_path) = certificate_path {
        entry.insert("certfile".to_string(), ::serde_json::json!(certificate_path));
    }

    let Some(custom) = custom else {
        return;
    };
    let signature = custom
        .signature
        .as_ref()
        .and_then(|signature| signature.inline_document.as_deref());
    if let (Some(signature), Some(hash), Some(algorithm)) = (
        signature,
        custom.hash_algorithm.as_deref(),
        custom.signature_algorithm.as_deref(),
    ) {
        entry.insert(
            format!("sig-{}-{}", hash.to_lowercase(), algorithm.to_lowercase()),
            ::serde_json::json!(::aws_smithy_types::base64::encode(signature)),
        );
    }
}

/// How this service states that an `otaUpdateId` names no OTA update.
///
/// AWS IoT calls the resource an OTA update is addressed through an `otaUpdateJob` —
/// an update IS its deployment — and the refusal names the id the request asked for,
/// so every reader of an update and the delete refuse one unknown id in one wording.
pub fn missing_ota_update_message(ota_update_id: &str) -> String {
    format!("Requested otaUpdateJob {ota_update_id} does not exist")
}

/// The status an OTA update is created in.
///
/// `OTAUpdateStatus` is `CREATE_PENDING | CREATE_IN_PROGRESS | CREATE_COMPLETE |
/// CREATE_FAILED | DELETE_IN_PROGRESS | DELETE_FAILED`
/// ([`crate::ops::types::O_T_A_UPDATE_STATUS_VALUES`]) and the creation lifecycle is
/// entered at `CREATE_PENDING`: that is what `CreateOTAUpdate` answers with, and the
/// clock is what moves it on.
pub const NEW_OTA_UPDATE_STATUS: &str = "CREATE_PENDING";

/// The status an OTA update holds once its creation has completed.
pub const CREATED_OTA_UPDATE_STATUS: &str = "CREATE_COMPLETE";

/// How long after `CreateOTAUpdate` the update's creation completes.
///
/// The update is answered in a NON-TERMINAL status ([`NEW_OTA_UPDATE_STATUS`]), so the
/// clock owes its move to [`CREATED_OTA_UPDATE_STATUS`]; the span is the emulator's
/// own, chosen so a caller reading straight back still observes the transient status,
/// and the due instant is recorded rather than slept for.
pub const OTA_UPDATE_CREATION_SPAN: Duration = Duration::from_millis(500);

/// One OTA update in the control plane, as this service persists it.
///
/// It holds what `GetOTAUpdate` reports member for member — the id and ARN are its
/// identity, and the targets, protocols, files, configs and parameters are what the
/// create supplied — so a reader clones this record and the writer and the reader
/// cannot drift.
///
/// `role_arn`, `aws_job_abort_config`, `aws_job_timeout_config` and `tags` are NOT
/// kept: no reader of an OTA update reports them (`OTAUpdateInfo` declares none of
/// them), so storing them would promise a reader data this record does not hand back.
/// The timeout the request named IS carried — onto the executions of the job the
/// update deploys through, where a device reads it.
#[derive(Debug, Clone)]
pub struct RegisteredOtaUpdate {
    /// The id the update was created under; also its key in the store.
    pub ota_update_id: String,
    /// `arn:aws:iot:<region>:<account>:otaupdate/<otaUpdateId>`, minted on create.
    pub ota_update_arn: String,
    /// "The description of the OTA update", when the create carried one.
    pub description: Option<String>,
    /// "The devices targeted to receive OTA updates", verbatim.
    pub targets: Vec<String>,
    /// "The protocol used to transfer the OTA update image", when the create named
    /// one. Absent is distinguishable from a value, so a create that named no
    /// protocol reports none rather than an invented default.
    pub protocols: Option<Vec<String>>,
    /// `targetSelection` as supplied (`CONTINUOUS` / `SNAPSHOT`), or `None` when the
    /// request left it out.
    pub target_selection: Option<String>,
    /// "Configuration for the rollout of OTA updates", when the create carried one.
    pub aws_job_executions_rollout_config: Option<AwsJobExecutionsRolloutConfig>,
    /// "Configuration information for pre-signed URLs", when the create carried one.
    pub aws_job_presigned_url_config: Option<AwsJobPresignedUrlConfig>,
    /// "The files to be streamed by the OTA update", verbatim. Bounded by
    /// [`MAX_OTA_UPDATE_FILES`].
    pub files: Vec<OTAUpdateFile>,
    /// "A list of additional OTA update parameters, which are name-value pairs",
    /// keyed by name — `AdditionalParameterMap` is a map, so the key is the identity
    /// and a `BTreeMap` keeps one stable order for every reader.
    pub additional_parameters: BTreeMap<String, String>,
    /// The id of the AWS IoT job this update deploys through
    /// ([`ota_update_job_id`]), reported as `awsIotJobId`.
    pub aws_iot_job_id: String,
    /// That job's ARN ([`job_arn`]), reported as `awsIotJobArn`.
    pub aws_iot_job_arn: String,
    /// Where the update stands in its creation lifecycle: one of
    /// [`crate::ops::types::O_T_A_UPDATE_STATUS_VALUES`].
    pub status: String,
    /// When the creation this update was answered in the middle of completes — the
    /// transition the clock already owes, whether or not anybody calls. `None` once it
    /// has been materialised.
    pub create_complete_due_at: Option<SystemTime>,
    /// "The date when the OTA update was created" (`OTAUpdateInfo$creationDate`).
    pub creation_date: SystemTime,
    /// "The date when the OTA update was last updated"
    /// (`OTAUpdateInfo$lastModifiedDate`) — the instant of the last transition it
    /// made, which on a freshly created update is the create itself.
    pub last_modified_date: SystemTime,
}

impl RegisteredOtaUpdate {
    /// Record an OTA update created now: the id, ARN and the job it deploys through
    /// are its identity, the rest is what the create supplied, it enters the lifecycle
    /// at [`NEW_OTA_UPDATE_STATUS`] with its completion due
    /// [`OTA_UPDATE_CREATION_SPAN`] later, and both dates are the instant of the
    /// create.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        ota_update_id: String,
        ota_update_arn: String,
        aws_iot_job_id: String,
        aws_iot_job_arn: String,
        description: Option<String>,
        targets: Vec<String>,
        protocols: Option<Vec<String>>,
        target_selection: Option<String>,
        aws_job_executions_rollout_config: Option<AwsJobExecutionsRolloutConfig>,
        aws_job_presigned_url_config: Option<AwsJobPresignedUrlConfig>,
        files: Vec<OTAUpdateFile>,
        additional_parameters: BTreeMap<String, String>,
        created_at: SystemTime,
    ) -> Self {
        Self {
            ota_update_id,
            ota_update_arn,
            description,
            targets,
            protocols,
            target_selection,
            aws_job_executions_rollout_config,
            aws_job_presigned_url_config,
            files,
            additional_parameters,
            aws_iot_job_id,
            aws_iot_job_arn,
            status: NEW_OTA_UPDATE_STATUS.to_string(),
            create_complete_due_at: Some(created_at + OTA_UPDATE_CREATION_SPAN),
            creation_date: created_at,
            last_modified_date: created_at,
        }
    }

    /// Materialise the completion the clock already owes at `now`, and answer whether
    /// anything changed.
    ///
    /// The update was answered `CREATE_PENDING` with its completion due at a recorded
    /// instant, so by `now` it is `CREATE_COMPLETE` whether or not anybody called: a
    /// reader states that, and stamps the transition's own instant as the update's
    /// `lastModifiedDate` rather than the moment of the read.
    pub fn settle(&mut self, now: SystemTime) -> bool {
        let Some(due) = self.create_complete_due_at.filter(|due| now >= *due) else {
            return false;
        };
        self.status = CREATED_OTA_UPDATE_STATUS.to_string();
        self.create_complete_due_at = None;
        self.last_modified_date = due;
        true
    }

    /// This update as the modeled `OTAUpdateInfo` a reader hands back.
    ///
    /// `errorInfo` is "A collection of error information on this OTA update" and
    /// nothing here fails a creation, so there is no stored error to report and the
    /// member is left out rather than reported empty. `additionalParameters` is
    /// reported only while the update carries some, so a create that named none is not
    /// answered with an empty map.
    pub fn info(&self) -> OTAUpdateInfo {
        OTAUpdateInfo {
            ota_update_id: Some(self.ota_update_id.clone()),
            ota_update_arn: Some(self.ota_update_arn.clone()),
            creation_date: Some(::aws_smithy_types::DateTime::from(self.creation_date)),
            last_modified_date: Some(::aws_smithy_types::DateTime::from(self.last_modified_date)),
            description: self.description.clone(),
            targets: Some(self.targets.clone()),
            protocols: self.protocols.clone(),
            aws_job_executions_rollout_config: self.aws_job_executions_rollout_config.clone(),
            aws_job_presigned_url_config: self.aws_job_presigned_url_config.clone(),
            target_selection: self.target_selection.clone(),
            ota_update_files: Some(self.files.clone()),
            ota_update_status: Some(self.status.clone()),
            aws_iot_job_id: Some(self.aws_iot_job_id.clone()),
            aws_iot_job_arn: Some(self.aws_iot_job_arn.clone()),
            error_info: None,
            additional_parameters: (!self.additional_parameters.is_empty()).then(|| {
                self.additional_parameters
                    .iter()
                    .map(|(name, value)| (name.clone(), value.clone()))
                    .collect()
            }),
        }
    }
}

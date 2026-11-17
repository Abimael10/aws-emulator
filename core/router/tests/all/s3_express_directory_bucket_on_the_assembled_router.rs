// SPDX-License-Identifier: CC0-1.0

//! S3 Express One Zone is REACHABLE on the whole assembled emulator, with request
//! authentication on: a directory bucket created through the control surface is the same
//! bucket every Zonal call then addresses.
//!
//! The rule under test: **a `CreateBucket` that places a directory bucket records that
//! bucket, so `CreateSession` on the same name serves it** — whichever Region the request is
//! signed for, whichever host it is addressed to, and whichever of the two signing names
//! (`s3` for the general purpose surface, `s3express` for the Zonal and `s3express-control`
//! surfaces) its credential scope carries. `CreateSession`'s one modelled error,
//! `NoSuchBucket` (404), is reserved for a name nothing holds, and the session it vends is
//! what every later Express data-plane call is signed with — so a bucket that cannot be
//! sessioned takes the whole Express surface with it.
//!
//! It lives here, over the router the production entrypoint assembles and with signatures
//! verified, because what it pins is reachability across the whole service set rather than
//! one crate's handlers: the S3 Express surface shares a signing-name family with the rest of
//! S3 and a path space with S3 Control, so a claim, a signature family or a route resolved
//! elsewhere is invisible to the owning crate's own tests.
//!
//! The rows differ along what the rule generalises over: the Region and its zone, the signing
//! name the credential scope carries, and whether the request is addressed to a regional
//! endpoint host or to the loopback authority a client of this emulator reaches it on.
//!
//! Assembling the whole router costs stack (see `boot_serve.rs`), so it is built on an
//! explicitly sized thread.

use std::sync::Arc;
use std::time::SystemTime;

use aws_credential_types::Credentials;
use aws_sigv4::http_request::{sign, SignableBody, SignableRequest, SigningSettings};
use aws_sigv4::sign::v4::SigningParams;
use smithy_server_runtime::MultiServiceRouter;

/// Keep this at or above the size `boot_serve.rs` uses: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The credential every call signs with: the pair this process publishes once request
/// authentication is on.
struct Signer {
    key_id: String,
    secret: String,
}

impl Signer {
    /// Sign `request` for `signing_name` in `region`, in the `Authorization` header an SDK
    /// request carries.
    fn sign(
        &self,
        mut request: http::Request<Vec<u8>>,
        signing_name: &str,
        region: &str,
    ) -> http::Request<Vec<u8>> {
        let identity = Credentials::new(&self.key_id, &self.secret, None, None, "test").into();
        let params = SigningParams::builder()
            .identity(&identity)
            .region(region)
            .name(signing_name)
            .time(SystemTime::now())
            .settings(SigningSettings::default())
            .build()
            .expect("the signing parameters are complete")
            .into();
        let (method, uri) = (request.method().to_string(), request.uri().to_string());
        let headers: Vec<(String, String)> = request
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_string(),
                    value.to_str().expect("a text header").to_string(),
                )
            })
            .collect();
        let signable = SignableRequest::new(
            &method,
            &uri,
            headers
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str())),
            SignableBody::Bytes(request.body()),
        )
        .expect("the request is signable");
        let (instructions, _) = sign(signable, &params)
            .expect("the request signs")
            .into_parts();
        instructions.apply_to_request_http1x(&mut request);
        request
    }
}

/// The `CreateBucketConfiguration` a directory-bucket create carries: the zone it is placed
/// in ("the name of the location is the Zone ID of the Availability Zone", `LocationInfo`)
/// and the kind of bucket it is.
fn directory_configuration(zone_id: &str) -> String {
    format!(
        "<CreateBucketConfiguration xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
         <Location><Type>AvailabilityZone</Type><Name>{zone_id}</Name></Location>\
         <Bucket><DataRedundancy>SingleAvailabilityZone</DataRedundancy><Type>Directory</Type>\
         </Bucket></CreateBucketConfiguration>"
    )
}

/// Where a request is addressed and what it is signed for: the signing name its credential
/// scope carries, the Region of that scope, and the host it is sent to.
#[derive(Debug, Clone, Copy)]
struct Endpoint {
    signing_name: &'static str,
    region: &'static str,
    host: &'static str,
}

/// One S3 request, path-style, signed for and addressed to `endpoint`.
async fn s3(
    router: &MultiServiceRouter,
    signer: &Signer,
    endpoint: Endpoint,
    method: &str,
    path: &str,
    body: &str,
) -> (u16, String) {
    let Endpoint {
        signing_name,
        region,
        host,
    } = endpoint;
    let built = http::Request::builder()
        .method(method)
        .uri(format!("http://{host}{path}"))
        .header("host", host)
        .body(body.as_bytes().to_vec())
        .expect("a well-formed request");
    let signed = signer.sign(built, signing_name, region);
    let (parts, payload) = signed.into_parts();
    let response = router
        .dispatch(http::Request::from_parts(
            parts,
            bytes::Bytes::from(payload),
        ))
        .await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).into_owned(),
    )
}

/// The production router with signature verification ON, and the credential its own
/// authenticator publishes for this process.
fn assembled_router() -> (MultiServiceRouter, Signer) {
    std::thread::Builder::new()
        .name("s3-express-router".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine = aws_emulator_router::new_engine();
            let mut router = aws_emulator_router::build_router(Arc::clone(&engine));
            router.set_request_authentication(true);
            let published = ::emulator_server::SigV4Authenticator::minted_credentials()
                .expect("turning authentication on publishes this process's keys");
            let signer = Signer {
                key_id: published.access_key_id.clone(),
                secret: published.secret_access_key.clone(),
            };
            (router, signer)
        })
        .expect("spawn the router-assembly thread")
        .join()
        .expect("the production router must assemble")
}

#[tokio::test]
async fn a_directory_bucket_created_on_the_assembled_router_serves_its_zonal_calls() {
    let (router, signer) = assembled_router();

    // (case, where the request is addressed and signed, the zone the bucket is placed in)
    let rows: [(&str, Endpoint, &str); 4] = [
        (
            "the regional endpoint signed for s3express",
            Endpoint {
                signing_name: "s3express",
                region: "us-west-2",
                host: "s3.us-west-2.amazonaws.com",
            },
            "usw2-az1",
        ),
        (
            "the regional endpoint signed for s3",
            Endpoint {
                signing_name: "s3",
                region: "us-west-2",
                host: "s3.us-west-2.amazonaws.com",
            },
            "usw2-az2",
        ),
        (
            "a loopback endpoint signed for s3express",
            Endpoint {
                signing_name: "s3express",
                region: "us-west-2",
                host: "127.0.0.1:34567",
            },
            "usw2-az3",
        ),
        (
            "a loopback endpoint signed for s3",
            Endpoint {
                signing_name: "s3",
                region: "us-east-1",
                host: "127.0.0.1:34567",
            },
            "use1-az4",
        ),
    ];

    for (case, endpoint, zone) in rows {
        let bucket = format!("xsvc-express-{zone}--{zone}--x-s3");
        let (status, answered) = s3(
            &router,
            &signer,
            endpoint,
            "PUT",
            &format!("/{bucket}"),
            &directory_configuration(zone),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateBucket({bucket:?}) must succeed, got {status} with {answered:?}"
        );

        let (status, answered) = s3(
            &router,
            &signer,
            endpoint,
            "GET",
            &format!("/{bucket}?session"),
            "",
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateSession({bucket:?}) must serve the bucket the create \
             recorded, got {status} with {answered:?}"
        );
        assert!(
            answered.contains("<AccessKeyId>"),
            "case {case:?}: the session must vend credentials, got {answered:?}"
        );
    }
}

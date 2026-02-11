use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::sync::Mutex;

use async_trait::async_trait;
use google_photoslibrary1::PhotosLibrary;
use google_photoslibrary1::api::Album;
use google_photoslibrary1::api::BatchCreateMediaItemsRequest;
use google_photoslibrary1::api::CreateAlbumRequest;
use google_photoslibrary1::api::NewMediaItem;
use google_photoslibrary1::api::Scope;
use google_photoslibrary1::api::SimpleMediaItem;
use google_photoslibrary1::common;
use google_photoslibrary1::hyper;
use google_photoslibrary1::hyper_rustls;
use google_photoslibrary1::hyper_rustls::HttpsConnector;
use google_photoslibrary1::hyper_util;
use google_photoslibrary1::hyper_util::client::legacy::connect::HttpConnector;
use google_photoslibrary1::yup_oauth2;
use google_photoslibrary1::yup_oauth2::error::TokenStorageError;
use google_photoslibrary1::yup_oauth2::storage::TokenInfo;
use google_photoslibrary1::yup_oauth2::storage::TokenStorage;
use http_body_util::BodyExt;
use http_body_util::Full;
use http_body_util::combinators::BoxBody;
use hyper::body::Bytes;
use itertools::Itertools;
use reqwest::header;

/// Other google SDKs:
///     * service-authenticator
///       The Photos API does not support "service accounts" or "API keys", it only supports OAuth2 flows for user accounts,
///       so this library is not compatible.
///       Requires using actix instead of tokio.
///     * google-api-rust-client-unoffical
///       Same as above, this library only supports service accounts.
///
/// TODOs:
///     * parameterize the path to the token storage
///     * parameterize the path to the client secret file
pub async fn load() {
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .unwrap();

    let token_storage = match try_read_tokens() {
        Ok(tokens) => FileTokenStorage {
            tokens: Mutex::new(tokens),
        },
        Err(err) => {
            println!(">>>>> FAIL_CI err: {:#?}", err);
            FileTokenStorage {
                tokens: Mutex::new(HashMap::new()),
            }
        }
    };

    let secret_file = "/home/dc/Dropbox/dotfiles/client_secret_caminhar_crawler.json";

    let secret: yup_oauth2::ApplicationSecret = yup_oauth2::read_application_secret(secret_file)
        .await
        .expect("client secret could not be read");

    // Instantiate the authenticator. It will choose a suitable authentication flow for you,
    // unless you replace  `None` with the desired Flow.
    // Provide your own `AuthenticatorDelegate` to adjust the way it operates and get feedback about
    // what's going on. You probably want to bring in your own `TokenStorage` to persist tokens and
    // retrieve them from storage.
    let connector = hyper_rustls::HttpsConnectorBuilder::new()
        .with_native_roots()
        .unwrap()
        .https_only()
        .enable_http2()
        .build();

    let executor = hyper_util::rt::TokioExecutor::new();
    let auth = yup_oauth2::InstalledFlowAuthenticator::with_client(
        secret,
        yup_oauth2::InstalledFlowReturnMethod::HTTPRedirect,
        yup_oauth2::client::CustomHyperClientBuilder::from(
            hyper_util::client::legacy::Client::builder(executor).build(connector),
        ),
    )
    .with_storage(Box::new(token_storage))
    .build()
    .await
    .unwrap();

    let client = hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
        .build(
            hyper_rustls::HttpsConnectorBuilder::new()
                .with_native_roots()
                .unwrap()
                .https_or_http()
                .enable_http2()
                .build(),
        );
    let hub: PhotosLibrary<HttpsConnector<HttpConnector>> = PhotosLibrary::new(client, auth);

    list_albums(&hub).await;
}

async fn list_albums(hub: &PhotosLibrary<HttpsConnector<HttpConnector>>) {
    // TODO: cycle through all the page tokens
    let (_, resp) = hub
        .albums()
        .list()
        .page_token("CkQKPnR5cGUuZ29vZ2xlYXBpcy5jb20vZ29vZ2xlLnBob3Rvcy5saWJyYXJ5LnYxLkxpc3RBbGJ1bXNSZXF1ZXN0EgIIChKMA0FIX3VRNDNELXdhSmNBUzZDTUlJd0VHanRxSEJNcmM5SnlySzBLNjBsNnBrMGd3OEMtOVRGRFE3QVFkOUFSVmpaWGJ0cDVEdndaVjJ0dmxtSFFjZUdRQVRFYTdLUjBuTEt0UFNuVE83RkE1RUl6eW9FRjlTX3RNZ2V1OTN2TnV6SEJSR2ZHMjBXRkYyOHZHYVZaRHQ4Y20xMjM4Y2NHV1RXVGNNWTdUVy1qX3VXSFpkclc0RWdtcExrOGhYV1JWRVEyemN2aGoxNko0RFdxR2l1bzVPNjdPMGc1S1JnNUVBRHdncEZ3OGhnQldnSTh4OFJ4dWQ4dEhjR3I1YjlVYnBYd1BEV2Y4dTZ2VDROZ2t0SHlxcTRGRl9hS3MwdWdKUjFXNVU3SVZGbldoSlY4U3ZCNzhtQUhQRWFBaXJLNHB2d1ZBSmRha1ljR0N4WkZyVUtsUXAxbzNtRjhEY0hmWFBLdTB4MDJFM1MyYjZsT0ZMak5hMUJRd0dGVWlNOWFPcW5XR25xRERDa0NHURoA")
        .page_size(10)
        .exclude_non_app_created_data(false)
        .add_scopes(SCOPES)
        .doit()
        .await
        .unwrap();

    println!(">>>>> FAIL_CI resp: {:#?}", resp);
}

/// Uploads a single photo using a raw binary upload request.
///
/// https://developers.google.com/photos/library/guides/upload-media#uploading-bytes
async fn upload_photo(hub: &PhotosLibrary<HttpsConnector<HttpConnector>>) {
    let scopes: Vec<&str> = SCOPES.iter().map(|s| s.as_ref()).collect_vec();

    let token = hub.auth.get_token(scopes.as_ref()).await.unwrap().unwrap();

    let client = &hub.client;

    let mut file = File::open("/home/dc/Downloads/cais-de-gaia3.jpg").unwrap();
    // Read the file into memory to send as a raw upload body.
    let mut buffer = Vec::new();
    file.read_to_end(&mut buffer).unwrap();

    let url = "https://photoslibrary.googleapis.com/v1/uploads";
    let body = Full::new(Bytes::from(buffer)).map_err(|err| match err {});
    let request = hyper::Request::builder()
        .method(hyper::Method::POST)
        .uri(url)
        .header(header::AUTHORIZATION, format!("Bearer {}", token))
        // .header("X-Goog-Upload-File-Name", "cais-de-gaia3.jpg")
        .header("X-Goog-Upload-Protocol", "raw")
        .header(header::CONTENT_TYPE, "application/octet-stream")
        // TODO: try `common::to_body`, the same thing they use in the googlephotos lib
        .body(BoxBody::new(body))
        .unwrap();

    // Fire the upload request and wait for completion.
    let response = client.request(request).await.unwrap();

    let (parts, body) = response.into_parts();

    println!(">>>>> FAIL_CI status: {:#?}", parts.status);

    let body = common::Body::new(body);
    let bytes = common::to_bytes(body).await.unwrap_or_default();
    let upload_token = common::to_string(&bytes);

    println!(">>>>> FAIL_CI upload_token: {:#?}", upload_token);
}

async fn add_to_library_and_album(hub: &PhotosLibrary<HttpsConnector<HttpConnector>>) {
    let (_, resp) = hub
        .media_items()
        .batch_create(BatchCreateMediaItemsRequest {
            album_id: Some(
                "AMN8MgqxOXhPCGV9GyJIDQLODkpxpZoUxuKpr3MlWQ66j7WPVTN7uq46ScQBP87FC5n9ccsKYvGg"
                    .to_string(),
            ),
            album_position: None,
            new_media_items: Some(vec![NewMediaItem {
                description: Some("Test Photo My description".to_string()),
                simple_media_item: Some(SimpleMediaItem {
                    file_name: Some("my-test-filename.jpg".to_string()),
                    upload_token: Some("CAIS6QIAJoFQihcsU5cX8sgx5DqgeRAvGtzZ0TnlvTTF7hzRxO0ITNkVHh3zLK5793W/rGnMvhpcvtbsxRMtvRlC9nEWTVzAUXHUQG/PYEswd90x82BlFGanyx22TWIjhJOYqWKe6zjsnsgza7OjvU69pg+GQSl67e2iDNPCNh0XtI9fTp5Dz5gUKhcmKL777x/wiauw+FT8SQZPNGK9G/Os+HlZcSTbH0VOiGXndGG7bMIZWTj8b+khR5peE0Df9/H6mj0FzvBDW6ikgKfXpYyXXWv8qjXEgMVrXeSzayUcMd/eKSobHLni3h6eN4c6rhssXInUV/2QOVyucGn+ld6btba6s8JHeRkt7AN7qxWW6uWS6LzFaX2LJ6836KS6b5f4Y/fEGGzrF94nt6bvuD80alkO7Kt4uOzRMlN7XhGnM79PEhXt/DW5P+xuvRFR24r7mZmDOOi+wHfr6HcgnxMbGBQA4/BqpaZUOLWh".to_string())
                    ,
                }),
            }]),
        }) // .page_token("voluptua.")
        // .page_size(10)
        // .exclude_non_app_created_data(false)
        .add_scopes(SCOPES)
        .doit()
        .await
        .unwrap();

    println!(">>>>> FAIL_CI resp: {:#?}", resp);
}

async fn create_album(hub: &PhotosLibrary<HttpsConnector<HttpConnector>>) {
    let (_, resp) = hub
        .albums()
        .create(CreateAlbumRequest {
            album: Some(Album {
                cover_photo_base_url: None,
                cover_photo_media_item_id: None,
                id: None,
                is_writeable: Some(true),
                media_items_count: None,
                product_url: None,
                share_info: None,
                title: Some("Test Album".to_string()),
            }),
        })
        .add_scopes(SCOPES)
        .doit()
        .await
        .unwrap();

    println!(">>>>> FAIL_CI resp: {:#?}", resp);
}

struct FileTokenStorage {
    tokens: Mutex<HashMap<Vec<String>, TokenInfo>>,
}

const TOKENS_FILE: &str = "tokens.json";

const SCOPES: &[Scope] = &[
    Scope::Appendonly,
    Scope::EditAppcreateddata,
    Scope::Readonly,
    Scope::ReadonlyAppcreateddata,
    Scope::Sharing,
];

#[async_trait]
impl TokenStorage for FileTokenStorage {
    async fn set(&self, scopes: &[&str], token: TokenInfo) -> Result<(), TokenStorageError> {
        let mut tokens = self.tokens.lock().unwrap();
        tokens.insert(scopes.iter().map(|s| s.to_string()).collect(), token);

        let file = File::create(TOKENS_FILE).unwrap();
        serde_json::to_writer_pretty(file, &tokens.iter().collect_vec()).unwrap();
        Ok(())
    }

    async fn get(&self, scopes: &[&str]) -> Option<TokenInfo> {
        let scopes: Vec<_> = scopes.iter().map(|s| s.to_string()).collect();
        self.tokens.lock().unwrap().get(&scopes).cloned()
    }
}

fn try_read_tokens() -> Result<HashMap<Vec<String>, TokenInfo>, std::io::Error> {
    let file = File::open(TOKENS_FILE)?;
    let tokens: Vec<(Vec<String>, TokenInfo)> = serde_json::from_reader(file)?;
    Ok(tokens.into_iter().collect())
}

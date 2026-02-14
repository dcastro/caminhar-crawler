use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;
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
use google_photoslibrary1::common::Connector;
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

use crate::FileType;
use crate::PictureFile;
use crate::State;

/// Other google SDKs:
///     * service-authenticator
///       The Photos API does not support "service accounts" or "API keys", it only supports OAuth2 flows for user accounts,
///       so this library is not compatible.
///       Requires using actix instead of tokio.
///     * google-api-rust-client-unoffical
///       Same as above, this library only supports service accounts.
///
/// CLI tools:
///     * https://github.com/gphotosuploader/gphotos-uploader-cli
///     * https://github.com/int128/gpup
///     * https://docs.rs/crate/google-photoslibrary1-cli/latest
///
/// TODOs:
///     * parameterize the path to the token storage
///     * parameterize the path to the client secret file
pub async fn upload(
    pics: Vec<PictureFile>,
    album_title: &str,
    state: &mut State,
    state_path: &Path,
) {
    let hub = setup().await;
    let album_id = match get_album_id(&hub, album_title).await {
        Some(album_id) => album_id,
        None => {
            println!("Google Photos: creating album '{album_title}'");
            create_album(&hub, album_title.to_owned()).await
        }
    };

    let count = pics.len();
    for (index, pic) in pics.into_iter().rev().enumerate() {
        println!(
            "Google Photos: uploading media {}/{}: {}",
            index + 1,
            count,
            pic.file_path.display()
        );

        match pic.file_type {
            FileType::Image => {}
            FileType::Video => {}
            FileType::Pdf => {
                println!("Google Photos: skipping PDF file");
                continue;
            }
        }

        let upload_token = upload_photo(&hub, &pic.file_path).await;
        add_to_library_and_album(&hub, &pic, album_id.clone(), upload_token).await;

        state.latest_uploaded_img_id = Some(pic.img_large_id);
        state.save_to_file(state_path);
    }
}

async fn setup() -> PhotosLibrary<HttpsConnector<HttpConnector>> {
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .unwrap();

    let token_storage = match try_read_tokens() {
        Ok(tokens) => FileTokenStorage {
            tokens: Mutex::new(tokens),
        },
        Err(err) => {
            eprintln!("{err}");
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

    PhotosLibrary::new(client, auth)
}

#[allow(clippy::question_mark)]
async fn get_album_id<C: Connector>(hub: &PhotosLibrary<C>, album_title: &str) -> Option<String> {
    let mut page_token: Option<String> = None;

    loop {
        let request = hub.albums().list().add_scopes(SCOPES);
        let request = match page_token {
            Some(token) => request.page_token(&token),
            None => request,
        };

        let (_, resp) = request.doit().await.unwrap();

        if let Some(album) = resp
            .albums
            .iter()
            .flatten()
            .find(|album| album.title.as_deref() == Some(album_title))
        {
            return album.id.clone();
        }

        if resp.next_page_token.is_none() {
            return None;
        }

        page_token = resp.next_page_token;
    }
}

/// Uploads a single photo using a raw binary upload request.
///
/// https://developers.google.com/photos/library/guides/upload-media#uploading-bytes
async fn upload_photo<C: Connector>(hub: &PhotosLibrary<C>, file_path: &Path) -> String {
    let scopes: Vec<&str> = SCOPES.iter().map(|s| s.as_ref()).collect_vec();

    // Read the file into memory to send as a raw upload body.
    let mut file = File::open(file_path).unwrap();
    let mut buffer = Vec::new();
    file.read_to_end(&mut buffer).unwrap();

    let url = "https://photoslibrary.googleapis.com/v1/uploads";
    let body = Full::new(Bytes::from(buffer)).map_err(|err| match err {});
    let token = hub.auth.get_token(scopes.as_ref()).await.unwrap().unwrap();
    let request = hyper::Request::builder()
        .method(hyper::Method::POST)
        .uri(url)
        .header(header::AUTHORIZATION, format!("Bearer {}", token))
        .header("X-Goog-Upload-Protocol", "raw")
        .header(header::CONTENT_TYPE, "application/octet-stream")
        // TODO: try `common::to_body`, the same thing they use in the googlephotos lib
        .body(BoxBody::new(body))
        .unwrap();

    // Fire the upload request and wait for completion.
    let client = &hub.client;
    let response = client.request(request).await.unwrap();
    let (parts, body) = response.into_parts();

    let body = common::Body::new(body);
    let bytes = common::to_bytes(body).await.unwrap_or_default();
    let body_str = common::to_string(&bytes);

    if !parts.status.is_success() {
        panic!("Upload failed with status '{}': {}", parts.status, body_str);
    }

    body_str.into_owned()
}

async fn add_to_library_and_album<C: Connector>(
    hub: &PhotosLibrary<C>,
    pic: &PictureFile,
    album_id: String,
    upload_token: String,
) {
    let label_desc = merge_label_desc(pic);

    hub.media_items()
        .batch_create(BatchCreateMediaItemsRequest {
            album_id: Some(album_id),
            album_position: None,
            new_media_items: Some(vec![NewMediaItem {
                description: Some(label_desc),
                simple_media_item: Some(SimpleMediaItem {
                    file_name: Some(pic.file_path.file_name().unwrap().display().to_string()),
                    upload_token: Some(upload_token),
                }),
            }]),
        }) // .page_token("voluptua.")
        .add_scopes(SCOPES)
        .doit()
        .await
        .unwrap();
}

fn merge_label_desc(pic: &PictureFile) -> String {
    if pic.label.is_empty() {
        pic.description.to_owned()
    } else if pic.description.is_empty() {
        pic.label.to_owned()
    } else {
        format!("{}\n\n{}", pic.label, pic.description)
    }
}

async fn create_album<C: Connector>(hub: &PhotosLibrary<C>, album_title: String) -> String {
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
                title: Some(album_title),
            }),
        })
        .add_scopes(SCOPES)
        .doit()
        .await
        .unwrap();

    resp.id.unwrap()
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

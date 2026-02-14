use std::collections::BTreeSet;
use std::fs;
use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

use chrono::NaiveDate;
use clap::Parser;
use itertools::Itertools;
use reqwest::header;
use serde::Deserialize;
use serde::Serialize;

mod gphotos;

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct Pictures {
    pictures: Vec<Picture>,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
struct Picture {
    label: String,
    description: String,
    short_date: String,
    img_large: String,
    img_large_id: u32,

    // video/mp4 or image/jpeg
    #[serde(rename = "type")]
    type_: String,
}

#[derive(Debug, Clone, Serialize)]
struct PictureFile {
    file_path: PathBuf,
    file_type: FileType,

    label: String,
    description: String,
    short_date: NaiveDate,
    img_large: String,
    img_large_id: u32,
    type_: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, Copy)]
pub enum FileType {
    Image,
    Video,
    Pdf,
}

impl PictureFile {
    fn new(pic: Picture, pics_dir: &Path) -> Self {
        let Picture {
            label,
            description,
            short_date,
            img_large,
            img_large_id,
            type_,
        } = pic;

        let label = label.trim().to_owned();
        let description = description.trim().to_owned();

        let short_date = NaiveDate::parse_from_str(&short_date, "%d-%m-%Y").unwrap();

        let (extension, file_type) = match type_.as_str() {
            "video/mp4" => ("mp4", FileType::Video),
            "image/jpeg" => ("jpg", FileType::Image),
            "application/pdf" => ("pdf", FileType::Pdf),
            other => panic!("unknown media type: {other}"),
        };

        let file_path = pics_dir.join(make_filename(
            &short_date,
            img_large_id,
            label.trim(),
            extension,
        ));

        Self {
            label,
            description,
            short_date,
            img_large,
            img_large_id,
            file_path,
            type_,
            file_type,
        }
    }
}

#[derive(Parser, Debug)]
struct Args {
    /// Where to save the pictures.
    #[arg(short, long)]
    pics_dir: PathBuf,

    /// Where to save the JSON file with the application's state.
    #[arg(short, long)]
    state_path: PathBuf,

    /// The name of the album to upload the media files to.
    #[arg(short, long)]
    album_title: String,
}

#[derive(Serialize, Deserialize, Debug)]
struct State {
    latest_saved_img_id: Option<u32>,
    latest_uploaded_img_id: Option<u32>,
}

impl State {
    fn load_from_file(path: &Path) -> Self {
        if path.exists() {
            let file = File::open(path).unwrap();
            serde_json::from_reader(file).unwrap()
        } else {
            State {
                latest_saved_img_id: None,
                latest_uploaded_img_id: None,
            }
        }
    }

    fn save_to_file(&self, path: &Path) {
        let file = File::create(path).unwrap();
        serde_json::to_writer_pretty(file, self).unwrap();
    }

    fn get_latest_img_ids(&self) -> Option<BTreeSet<u32>> {
        let mut ids = BTreeSet::new();
        if let Some(id) = self.latest_saved_img_id {
            ids.insert(id);
        }
        if let Some(id) = self.latest_uploaded_img_id {
            ids.insert(id);
        }
        if ids.is_empty() { None } else { Some(ids) }
    }
}

#[tokio::main]
async fn main() {
    let cookie =
        std::env::var("CAMINHAR_COOKIE").expect("CAMINHAR_COOKIE environment variable not set");

    let args = Args::parse();
    let mut state = State::load_from_file(&args.state_path);

    let pics = fetch_pics(&cookie, state.get_latest_img_ids()).await;

    let pics = pics
        .pictures
        .into_iter()
        .map(|pic| PictureFile::new(pic, &args.pics_dir))
        .collect_vec();

    let pics_to_download = match state.latest_saved_img_id {
        Some(latest_saved_img_id) => pics
            .iter()
            .take_while(|pic| pic.img_large_id != latest_saved_img_id)
            .cloned()
            .collect_vec(),
        None => pics.clone(),
    };

    let pics_to_upload = match state.latest_uploaded_img_id {
        Some(latest_uploaded_img_id) => pics
            .iter()
            .take_while(|pic| pic.img_large_id != latest_uploaded_img_id)
            .cloned()
            .collect_vec(),
        None => pics.clone(),
    };

    // TODO: remove this
    let file = File::create("pics.json").unwrap();
    serde_json::to_writer_pretty(file, &pics).unwrap();

    download_all_media(&pics_to_download, &cookie, &args, &mut state).await;
    gphotos::upload(
        pics_to_upload,
        &args.album_title,
        &mut state,
        &args.state_path,
    )
    .await;
}

async fn fetch_pics(cookie: &str, mut latest_saved_ids: Option<BTreeSet<u32>>) -> Pictures {
    let url = "https://ocaminhar.educabiz.com/childctrl/childgalleryloadmore";

    let mut all_pics = Pictures { pictures: vec![] };
    let mut page = 1;

    loop {
        println!("Fetching page {page}...");

        let pics = reqwest::Client::builder()
            .build()
            .unwrap()
            .get(url)
            .header(header::COOKIE, cookie)
            .form(&[("page", page.to_string().as_ref()), ("childId", "3408743")])
            .send()
            .await
            .unwrap()
            .json::<Pictures>()
            .await
            .unwrap();

        // If there are no more pictures, stop fetching.
        if pics.pictures.is_empty() {
            break;
        }

        // We break when we've reached either the last saved picture or the last updloaded picture, whichever is oldest.
        for pic in pics.pictures {
            if let Some(latest_saved_ids) = &mut latest_saved_ids {
                latest_saved_ids.remove(&pic.img_large_id);
                if latest_saved_ids.is_empty() {
                    return all_pics;
                }
            }
            all_pics.pictures.push(pic);
        }

        page += 1;
    }

    all_pics
}

async fn download_all_media(pics: &[PictureFile], cookie: &str, args: &Args, state: &mut State) {
    let count = pics.len();
    println!("Downloading {} media files.", count);

    for (index, pic) in pics.iter().rev().enumerate() {
        download_media(pic, cookie, args, index, count, state).await;
    }
}

#[allow(clippy::match_like_matches_macro)]
async fn download_media(
    pic: &PictureFile,
    cookie: &str,
    args: &Args,
    index: usize,
    count: usize,
    state: &mut State,
) {
    let PictureFile {
        file_path,
        label,
        description,
        short_date,
        img_large,
        img_large_id,
        type_,
        file_type,
    } = pic;

    println!(
        "Downloading media {}/{}: {img_large_id} - {label} - {img_large}",
        index + 1,
        count
    );

    let resp = reqwest::Client::builder()
        .build()
        .unwrap()
        .get(img_large)
        .header(header::COOKIE, cookie)
        .send()
        .await
        .unwrap();

    if !resp.status().is_success() {
        panic!(
            "request failed with status: {}, url: {img_large}",
            resp.status()
        );
    }

    let bytes = resp.bytes().await.unwrap();

    // Save file
    File::create(file_path).unwrap().write_all(&bytes).unwrap();

    let file_path = fix_extension(file_path, type_);

    add_metadata_tags(
        &file_path,
        *file_type,
        short_date,
        label,
        description,
        *img_large_id,
    );

    state.latest_saved_img_id = Some(*img_large_id);
    state.save_to_file(&args.state_path);
}

fn make_filename(
    short_date: &NaiveDate,
    img_large_id: u32,
    label: &str,
    extension: &str,
) -> String {
    let short_date = short_date.format("%Y-%m-%d").to_string();
    let filename = format!("[{short_date}] {img_large_id} {label}.{extension}");

    // Some descriptions have invalid characters, e.g. '/', so we have to sanitize them.
    // WARNING: this will also truncate filenames to 255 chars.
    let options = sanitize_filename::Options {
        replacement: "-",
        ..sanitize_filename::Options::default() // default options, for reference
    };

    sanitize_filename::sanitize_with_options(filename, options)
}

fn add_metadata_tags(
    path: &Path,
    file_type: FileType,
    short_date: &NaiveDate,
    label: &str,
    desc: &str,
    img_large_id: u32,
) {
    let short_date = convert_date_to_exif_format(short_date);

    // https://exiftool.org/TagNames/QuickTime.html
    // https://exiftool.org/TagNames/XMP.html
    // https://exiftool.org/TagNames/EXIF.html
    // https://exiftool.org/faq.html
    // https://exiftool.org/TagNames/JPEG.html
    // https://exiftool.org/TagNames/PDF.html
    call_exiftool(
        path,
        &[
            ("CreateDate", &short_date),
            ("Title", label),
            // Google Photos displays the `Description` tag in the image's properties sidebar.
            ("Description", desc),
            // We set the nanoseconds to the image ID, so that if there are multiple images with the same date, they will be sorted by their ID.
            ("SubSecTime", &img_large_id.to_string()),
        ],
    );

    if file_type == FileType::Image {
        call_exiftool(
            path,
            &[
                ("DateTimeOriginal", &short_date),
                ("ImageDescription", desc),
                ("SubSecTimeOriginal", &img_large_id.to_string()),
            ],
        );
    }
}

fn call_exiftool(path: &Path, tags: &[(&str, &str)]) {
    let tags = tags.iter().map(|(k, v)| format!("-{k}={v}"));

    // Example usage:
    // exiftool <file> -DateTimeOriginal="2026:01:13 00:00:00+00:00" -CreateDate="2026:01:13 00:00:00+00:00" -ImageDescription="desc"

    // https://exiftool.org/exiftool_pod.html
    let status = std::process::Command::new("exiftool")
        .arg("-s")
        .arg("-overwrite_original")
        .arg(path)
        .args(tags)
        .status()
        .expect("failed to run exiftool");
    if !status.success() {
        panic!("exiftool failed with status: {status}");
    }
}

/// Converts `13-01-2026` to `2026:01:13 13:00:00+00:00`
fn convert_date_to_exif_format(short_date: &NaiveDate) -> String {
    // If we set the hour to 00:00, then Google Photos will display this image in the previous day.
    // So we set it to 13:00 instead.
    let datetime = short_date.and_hms_opt(13, 0, 0).unwrap();
    let datetime_utc = datetime.and_utc();
    datetime_utc.format("%Y:%m:%d %H:%M:%S%:z").to_string()
}

/// Some files are incorrectly tagged.
/// E.g. the image with ID `26296938` says it's a `image/jpeg`, but it's actually a PNG file.
///
/// This function fixes the file extension based on the actual file type.
/// It returns the new filepath.
fn fix_extension(path: &Path, declared_mime_type: &str) -> PathBuf {
    let kind = infer::get_from_path(path)
        .expect("file read successfully")
        .expect("file type is known");

    if declared_mime_type != kind.mime_type() {
        eprintln!(
            "*** WARNING: file {path:?} has declared mime type {declared_mime_type}, but actual mime type is {}",
            kind.mime_type()
        );

        let new_path = path.with_extension(kind.extension());
        fs::rename(path, &new_path).unwrap();
        new_path
    } else {
        path.to_owned()
    }
}

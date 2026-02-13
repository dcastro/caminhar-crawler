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

#[allow(dead_code)]
mod gphotos;

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct Pictures {
    pictures: Vec<Picture>,
}

#[derive(Serialize, Deserialize, Debug)]
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

#[derive(Parser, Debug)]
struct Args {
    /// Where to save the pictures.
    #[arg(short, long)]
    pics_dir: PathBuf,

    /// Where to save the JSON file with the application's state.
    #[arg(short, long)]
    state_path: PathBuf,
}

#[derive(Serialize, Deserialize, Debug)]
struct State {
    latest_saved_img_id: Option<u32>,
}

impl State {
    fn load_from_file(path: &Path) -> Self {
        if path.exists() {
            let file = File::open(path).unwrap();
            serde_json::from_reader(file).unwrap()
        } else {
            State {
                latest_saved_img_id: None,
            }
        }
    }

    fn save_to_file(&self, path: &Path) {
        let file = File::create(path).unwrap();
        serde_json::to_writer_pretty(file, self).unwrap();
    }
}

#[tokio::main]
async fn main() {
    let cookie =
        std::env::var("CAMINHAR_COOKIE").expect("CAMINHAR_COOKIE environment variable not set");

    let args = Args::parse();
    let mut state = State::load_from_file(&args.state_path);

    let pics = fetch_pics(&cookie, state.latest_saved_img_id).await;

    download_all_media(&pics, &cookie, &args, &mut state).await;
}

async fn fetch_pics(cookie: &str, latest_saved_img_id: Option<u32>) -> Pictures {
    let url = "https://ocaminhar.educabiz.com/childctrl/childgalleryloadmore";

    let mut all_pics = Pictures { pictures: vec![] };
    let mut page = 1;

    loop {
        println!("Fetching page {page}...");

        let mut pics = reqwest::Client::builder()
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

        // If we have previously saved some pictures, we can stop fetching when we reach the latest saved picture.
        if let Some(latest_saved_img_id) = latest_saved_img_id
            && let Some((idx, _)) = pics
                .pictures
                .iter()
                .find_position(|pic| pic.img_large_id == latest_saved_img_id)
        {
            pics.pictures.truncate(idx);
            all_pics.pictures.extend(pics.pictures);
            break;
        }

        all_pics.pictures.extend(pics.pictures);
        page += 1;
    }

    all_pics
}

async fn download_all_media(pics: &Pictures, cookie: &str, args: &Args, state: &mut State) {
    let count = pics.pictures.len();
    println!("Downloading {} media files.", count);

    // TODO: remove this
    let file = File::create("pics.json").unwrap();
    serde_json::to_writer_pretty(file, pics).unwrap();

    for (index, pic) in pics.pictures.iter().rev().enumerate() {
        download_media(pic, cookie, args, index, count, state).await;
    }
}

#[allow(clippy::match_like_matches_macro)]
async fn download_media(
    pic: &Picture,
    cookie: &str,
    args: &Args,
    index: usize,
    count: usize,
    state: &mut State,
) {
    let Picture {
        label,
        description,
        short_date,
        img_large,
        img_large_id,
        type_,
    } = pic;

    let label = label.trim();
    let description = description.trim();
    let short_date = NaiveDate::parse_from_str(short_date, "%d-%m-%Y").unwrap();

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

    let extension = match type_.as_str() {
        "video/mp4" => "mp4",
        "image/jpeg" => "jpg",
        "application/pdf" => "pdf",
        other => panic!("unknown media type: {other}"),
    };

    let is_image = match type_.as_str() {
        "image/jpeg" => true,
        _ => false,
    };

    // Save file
    let file_path = args
        .pics_dir
        .join(make_filename(&short_date, *img_large_id, label, extension));
    File::create(&file_path).unwrap().write_all(&bytes).unwrap();

    let file_path = fix_extension(&file_path, type_);

    add_metadata_tags(&file_path, is_image, &short_date, label, description);

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

fn add_metadata_tags(path: &Path, is_image: bool, short_date: &NaiveDate, label: &str, desc: &str) {
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
        ],
    );

    if is_image {
        call_exiftool(
            path,
            &[
                ("DateTimeOriginal", &short_date),
                ("ImageDescription", desc),
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

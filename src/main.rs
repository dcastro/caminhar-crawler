use std::fs;
use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

use chrono::NaiveDate;
use clap::Parser;
use little_exif::exif_tag::ExifTag;
use little_exif::metadata::Metadata;
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

    is_video: bool,
}

#[derive(Parser, Debug)]
struct Args {
    /// Where to save the pictures.
    #[arg(short, long)]
    dir: PathBuf,
}

#[tokio::main]
async fn main() {
    let cookie =
        std::env::var("CAMINHAR_COOKIE").expect("CAMINHAR_COOKIE environment variable not set");

    let args = Args::parse();

    let pics = fetch_pics(&cookie).await;

    download_all_media(&pics, &cookie, &args.dir).await;
}

async fn fetch_pics(cookie: &str) -> Pictures {
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

        if pics.pictures.is_empty() {
            break;
        }

        all_pics.pictures.extend(pics.pictures);
        page += 1;
    }

    all_pics
}

async fn download_all_media(pics: &Pictures, cookie: &str, dir: &Path) {
    let count = pics.pictures.len();
    println!("Downloading {} media files.", count);

    // TODO: remove this
    let file = File::create("pics.json").unwrap();
    serde_json::to_writer_pretty(file, pics).unwrap();

    for (index, pic) in pics.pictures.iter().enumerate() {
        download_media(pic, cookie, dir, index, count).await;
    }
}

async fn download_media(pic: &Picture, cookie: &str, dir: &Path, index: usize, count: usize) {
    let Picture {
        label,
        description,
        short_date,
        img_large,
        img_large_id,
        type_,
        is_video,
    } = pic;

    let label = label.trim();
    let description = description.trim();

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
        other => panic!("unknown media type: {other}"),
    };

    // Save file
    let file_path = dir.join(make_filename(short_date, *img_large_id, label, extension));
    File::create(&file_path).unwrap().write_all(&bytes).unwrap();

    let file_path = fix_extension(&file_path, type_);

    let label_desc = merge_label_desc(label, description);
    edit_exif_tags(&file_path, *is_video, short_date, label_desc);
}

fn make_filename(short_date: &str, img_large_id: u32, label: &str, extension: &str) -> String {
    let filename = format!("[{short_date}] {img_large_id} {label}.{extension}");

    // Some descriptions have invalid characters, e.g. '/', so we have to sanitize them.
    // WARNING: this will also truncate filenames to 255 chars.
    let options = sanitize_filename::Options {
        replacement: "-",
        ..sanitize_filename::Options::default() // default options, for reference
    };

    sanitize_filename::sanitize_with_options(filename, options)
}

fn edit_exif_tags(path: &Path, is_video: bool, short_date: &str, label_desc: String) {
    let short_date = convert_date_to_exif_format(short_date);

    if !is_video {
        // exiftool <file> -DateTimeOriginal="2026:01:13 00:00:00+00:00" -CreateDate="2026:01:13 00:00:00+00:00" -ImageDescription="desc"
        //
        // Note: Unfortunately, Google Photos does not display the `ImageDescription` exif tag
        // (though it does display the `Description` tag, which is not an exif tag).
        let mut metadata: Metadata = Metadata::new();
        metadata.set_tag(ExifTag::CreateDate(short_date.clone()));
        metadata.set_tag(ExifTag::DateTimeOriginal(short_date.clone()));
        metadata.set_tag(ExifTag::ImageDescription(label_desc));
        metadata.write_to_file(path).unwrap();
    } else {
        // The `little-exif` crate will fail if we try to edit the exif tags of an mp4 file:
        // > Custom { kind: Unsupported, error: "Unsupported file type: mp4 - Unknown file type: mp4" }
        //
        // But the `exiftool` command works fine.
        let status = std::process::Command::new("exiftool")
            .arg("-s")
            .arg("-overwrite_original")
            .arg(path.as_os_str())
            .arg(format!("-CreateDate={short_date}"))
            .status()
            .expect("failed to run exiftool");
        if !status.success() {
            panic!("exiftool failed with status: {status}");
        }

        // Set mp4 comment, similar to how you'd use `ffmpeg` (or `ffprobe` to read it).
        let mut tag = mp4ameta::Tag::read_from_path(path).unwrap();
        tag.set_comment(label_desc);
        tag.write_to_path(path).unwrap();
    }
}

/// Converts `13-01-2026` to `2026:01:13 13:00:00+00:00`
fn convert_date_to_exif_format(short_date: &str) -> String {
    // Convert "13-01-2026" to "2026:01:13 00:00:00+00:00"
    let date = NaiveDate::parse_from_str(short_date, "%d-%m-%Y").unwrap();
    let datetime = date.and_hms_opt(13, 0, 0).unwrap();
    let datetime_utc = datetime.and_utc();
    datetime_utc.format("%Y:%m:%d %H:%M:%S%:z").to_string()
}

fn merge_label_desc(label: &str, description: &str) -> String {
    if label.is_empty() {
        description.to_owned()
    } else if description.is_empty() {
        label.to_owned()
    } else {
        format!("{label}\n---\n{description}")
    }
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

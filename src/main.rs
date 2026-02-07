use std::collections::HashMap;
use std::path::PathBuf;

use clap::Parser;
use reqwest::header;
use serde::Deserialize;

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct Pictures {
    pictures: Vec<Picture>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct Picture {
    label: String,
    description: String,
    short_date: String,
    img_large: String,
    img_large_id: u32,
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

    let url = "https://ocaminhar.educabiz.com/childctrl/childgalleryloadmore";

    let pics = reqwest::Client::builder()
        .build()
        .unwrap()
        .get(url)
        .header(header::COOKIE, cookie)
        .form(&[("page", "1"), ("childId", "3408743")])
        .send()
        .await
        .unwrap()
        .json::<Pictures>()
        .await
        .unwrap();

    println!(">>>>> FAIL_CI pics: {:#?}", pics);
}

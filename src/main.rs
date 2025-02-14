use std::{
    collections::HashSet,
    env,
    ffi::OsStr,
    fmt::Debug,
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Command,
};

use clap::{command, Parser, Subcommand};
use eyre::{bail, Context, Result};
use jsonpath_rust::JsonPath;
use rusty_tesseract::Image;
use serde::Deserialize;
use serde_json::Value;

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    #[command(subcommand)]
    mode: Mode,
}

#[derive(Debug, Subcommand)]
enum Mode {
    Scan,
    Search { query: String },
    Hydrate,
}

const CONFIG_PATH: &str = ".config/kartka.toml";

#[derive(Debug, Deserialize)]
struct Kartka {
    scan_dir: PathBuf,
    index_dir: PathBuf,
}

#[derive(Debug)]
struct UploadContent {
    name: String,
    content: String,
}

impl Kartka {
    fn index(&self) -> &Path {
        &self.index_dir
    }

    fn scans(&self) -> &Path {
        &self.scan_dir
    }

    fn search(&self, search_str: &str) -> Result<()> {
        let output = Command::new("rg")
            .arg("--json")
            .arg("-i")
            .arg(search_str)
            .current_dir(self.index())
            .output()
            .context("running ripgrep")?;
        let stdout_str = String::from_utf8_lossy(&output.stdout);
        let match_type_path = JsonPath::try_from("$.type")?;
        let match_file_path = JsonPath::try_from("$.data.path.text")?;
        let ids: HashSet<String> = stdout_str
            .lines()
            .map(|it| serde_json::from_str(it).unwrap())
            .filter(|it| &extract_path(it, &match_type_path) == "match")
            .map(|it| extract_path(&it, &match_file_path))
            .flat_map(|it| {
                Path::new(&it)
                    .file_name()
                    .and_then(OsStr::to_str)
                    .map(str::to_string)
            })
            .collect();

        let links: Vec<_> = ids
            .into_iter()
            .map(|it| format!("https://www.dropbox.com/home/Apps/kartka?preview={it}"))
            .collect();

        println!("{links:?}");
        Ok(())
    }

    fn upload(&self, content: &UploadContent) -> Result<()> {
        let content_path = self.index().join(&content.name);

        if let Ok(mut out) = File::create_new(&content_path) {
            out.write_all(&content.content.clone().into_bytes())?;
        } else {
            bail!("could not create file at {content_path:?}");
        }

        Ok(())
    }

    fn read_and_index(&self, dir: &Path, output_name: &str) -> Result<()> {
        let mut content = String::new();

        let mut entries: Vec<_> = dir
            .read_dir()
            .context(format!("reading dir: {:?}", dir))?
            .collect::<Result<_, _>>()?;

        entries.sort_by_key(|it| it.file_name());

        for dir_entry in entries.iter() {
            // skip if can't read name or is hidden
            if dir_entry
                .file_name()
                .to_str()
                .map(|it| it.starts_with("."))
                .unwrap_or(true)
            {
                continue;
            }

            println!("processing {:?}..", dir_entry.path());
            let contents = Image::from_path(dir_entry.path()).context("open file for OCR")?;
            let tsrt_args = rusty_tesseract::Args::default();
            let output =
                rusty_tesseract::image_to_string(&contents, &tsrt_args).context("running OCR")?;

            content.push_str(&output);
            content.push('\n');
        }

        self.upload(&UploadContent {
            name: output_name.to_string(),
            content,
        })
        .context("uploading content")?;

        Ok(())
    }

    fn scan(&self) -> Result<()> {
        let timestamp = jiff::Zoned::now().timestamp().strftime("%Y_%m_%d_%H_%M_%S");
        let pdf_name = format!("{timestamp}.pdf");
        self.read_and_index(self.scans(), &pdf_name)?;

        println!("converting to PDF..");
        let temp_dir = tempfile::tempdir()?;
        Command::new("magick")
            .arg(self.scans().join("*.png"))
            .arg(temp_dir.path().join(&pdf_name))
            .output()?;

        upload_to_dropbox(temp_dir.path(), &pdf_name)?;

        if inquire::Confirm::new("Delete files in scan dir?")
            .with_default(false)
            .prompt()?
        {
            for entry in self.scans().read_dir()? {
                fs::remove_file(entry?.path())?;
            }
        }

        println!("done!");
        Ok(())
    }

    fn rehydrate(&self) -> Result<()> {
        // want to download all files that I don't have in my index
        let remote_files: HashSet<_> = String::from_utf8(
            Command::new("rclone")
                .arg("lsf")
                .arg("dropbox:")
                .output()?
                .stdout,
        )?
        .lines()
        .map(|it| it.to_string())
        .collect();

        let local_files: HashSet<_> = self
            .index()
            .read_dir()?
            .into_iter()
            .map(|res| {
                res.map_err(|e| eyre::eyre!("{e:?}")).and_then(|it| {
                    it.file_name()
                        .into_string()
                        .map_err(|e| eyre::eyre!("{e:?}"))
                })
            })
            .collect::<Result<_>>()?;

        let missing_files = remote_files.difference(&local_files);
        let num_missing = missing_files.clone().count();
        for (i, missing) in missing_files.enumerate() {
            let temp_dir = tempfile::tempdir()?;
            let dest = temp_dir.path().join(missing);

            println!(
                "({} / {}) pulling, converting, and processing: {missing}..",
                i + 1,
                num_missing
            );
            Command::new("rclone")
                .arg("copyto")
                .arg(format!("dropbox:{missing}"))
                .arg(&dest)
                .output()?;

            Command::new("magick")
                .arg(&dest)
                .arg(temp_dir.path().join(format!("{missing}-%d.png")))
                .output()?;

            fs::remove_file(dest)?;

            self.read_and_index(temp_dir.path(), missing)?;
        }

        println!("done!");
        Ok(())
    }
}

fn extract_path(value: &Value, path: &JsonPath) -> String {
    let value: Value = path.find_slice(value)[0].clone().to_data();
    value.as_str().unwrap().to_string()
}

fn upload_to_dropbox(dir: &Path, target: &str) -> Result<()> {
    println!("Copying to Dropbox..");
    Command::new("rclone")
        .arg("copy")
        .arg("--exclude")
        .arg(".DS_Store")
        .arg("--include")
        .arg(target)
        .arg(dir)
        .arg("dropbox:")
        .output()?;

    Ok(())
}

fn main() {
    let args = Args::parse();

    let config_path =
        Path::new(&env::var("HOME").expect("no home env variable set")).join(CONFIG_PATH);

    if !config_path.exists() {
        panic!("no kartka config found at {config_path:?}");
    }

    let mut contents = String::new();
    File::open(config_path)
        .expect("open config")
        .read_to_string(&mut contents)
        .expect("could not read string contents");

    let kartka: Kartka = toml::from_str(&contents).expect("could not parse config");

    match args.mode {
        Mode::Scan => {
            kartka.scan().unwrap();
        }
        Mode::Search { query } => {
            kartka.search(&query).unwrap();
        }
        Mode::Hydrate => {
            kartka.rehydrate().unwrap();
        }
    };
}

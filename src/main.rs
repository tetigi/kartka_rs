use std::{
    collections::{HashMap, HashSet},
    env,
    ffi::OsStr,
    fmt::Debug,
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};

use axum::{
    extract::{Query, State},
    routing::{get, put},
    Json, Router,
};
use clap::{command, Parser, Subcommand};
use eyre::{Context, Result};
use jsonpath_rust::JsonPath;
use rusty_tesseract::Image;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    #[command(subcommand)]
    mode: Mode,
}

#[derive(Debug, Subcommand)]
enum Mode {
    Check,
    // Ingest,
    Scan,
    Server { root: PathBuf },
    // Search,
    // Hydrate
}

const CONFIG_PATH: &str = ".config/kartka.toml";

#[derive(Debug, Deserialize)]
struct Config {
    kartka_dir: PathBuf,
}

#[derive(Debug, Serialize, Deserialize)]
struct UploadContent {
    name: String,
    content: String,
}

async fn search(
    Query(params): Query<HashMap<String, String>>,
    State(state): State<Arc<Server>>,
) -> Json<Value> {
    if let Some(query) = params.get("query") {
        if let Ok(res) = state.search(query) {
            let links: Vec<_> = res
                .into_iter()
                .map(|it| format!("https://www.dropbox.com/home/Apps/kartka?preview={it}"))
                .collect();
            Json(json!({"links": links}))
        } else {
            Json(json!({"error": ":("}))
        }
    } else {
        Json(json!({}))
    }
}

async fn upload(
    State(state): State<Arc<Server>>,
    Json(content): Json<UploadContent>,
) -> Json<Value> {
    let content_path = state.content().join(content.name);

    if let Some(err) = write_out(&content.content.into_bytes(), &content_path) {
        return err;
    }

    Json(json!({"success": true}))
}

fn write_out(data: &[u8], path: &Path) -> Option<Json<Value>> {
    if let Ok(mut out) = File::create_new(path) {
        if let Err(e) = out.write_all(data) {
            return Some(Json(json!({"success": false, "error": e.to_string()})));
        }
    } else {
        return Some(Json(
            json!({"success": false, "error": format!("could not create file at {path:?}")}),
        ));
    }

    None
}

struct Server {
    root: PathBuf,
    content_dir: PathBuf,
}

impl Server {
    fn content(&self) -> PathBuf {
        self.root.join(&self.content_dir)
    }

    fn search(&self, search_str: &str) -> Result<Vec<String>> {
        let output = Command::new("rg")
            .arg("--json")
            .arg("-i")
            .arg(search_str)
            .current_dir(self.content())
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

        Ok(ids.into_iter().collect())
    }
}

fn extract_path(value: &Value, path: &JsonPath) -> String {
    let value: Value = path.find_slice(value)[0].clone().to_data();
    value.as_str().unwrap().to_string()
}

async fn scan(config: Config) -> Result<()> {
    let mut content = String::new();

    let mut entries: Vec<_> = config
        .kartka_dir
        .read_dir()
        .context(format!("reading kartka dir: {:?}", &config.kartka_dir))?
        .collect::<Result<_, _>>()?;

    entries.sort_by_key(|it| it.file_name());

    for dir_entry in entries.into_iter() {
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

    let timestamp = jiff::Zoned::now().timestamp().strftime("%Y_%m_%d_%H_%M_%S");
    let pdf_name = format!("{timestamp}.pdf");

    println!("converting to PDF..");
    Command::new("magick")
        .arg(config.kartka_dir.join("*.png"))
        .arg(config.kartka_dir.join(&pdf_name))
        .output()?;

    let client = reqwest::Client::new();

    client
        .put("http://localhost:3000/upload")
        .json(&UploadContent {
            name: format!("{timestamp}.pdf"),
            content,
        })
        .send()
        .await
        .context("uploading content")?;

    upload_to_dropbox(&config.kartka_dir, &pdf_name)?;
    println!("done!");
    println!("https://www.dropbox.com/home/Apps/kartka?preview={timestamp}.pdf");
    Ok(())
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

#[tokio::main]
async fn main() {
    let args = Args::parse();

    let config_path =
        Path::new(&env::var("HOME").expect("no home env variable set")).join(CONFIG_PATH);

    match args.mode {
        Mode::Check => todo!(),
        Mode::Scan => {
            if config_path.exists() {
                let mut contents = String::new();
                File::open(config_path)
                    .expect("open config")
                    .read_to_string(&mut contents)
                    .expect("could not read string contents");
                let config = toml::from_str(&contents).expect("could not parse config");
                scan(config).await.expect("error while scanning");
            } else {
                panic!("no kartka config found at {config_path:?}");
            }
        }
        Mode::Server { root } => {
            let server = Server {
                root,
                content_dir: Path::new("content").to_path_buf(),
            };

            if !&server.content().exists() {
                panic!(
                    "content directory at {:?} does not exist",
                    &server.content()
                );
            }

            let app = Router::new()
                .route("/search", get(search))
                .route("/upload", put(upload))
                .with_state(Arc::new(server));

            let listener = tokio::net::TcpListener::bind("localhost:3000")
                .await
                .unwrap();
            axum::serve(listener, app).await.unwrap();
        }
    };
}

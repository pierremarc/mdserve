#[macro_use]
extern crate lazy_static;
use ammonia;
use clap::{App, Arg};
use comrak::{markdown_to_html, ComrakOptions};
use std::collections::HashMap;
use std::convert::TryInto;
use std::path::PathBuf;
use tokio::{self, io::AsyncReadExt, sync::Mutex};
use warp::{self, Filter, Rejection};

#[derive(Debug)]
enum MarkdownError {
    NotMarkdown,
    // Decoding,
}

impl warp::reject::Reject for MarkdownError {}

const HTML_HEAD_STR: &'static str = include_str!("html/head.html");
const HTML_TAIL_STR: &'static str = include_str!("html/tail.html");

struct Rendered(String);

impl warp::Reply for Rendered {
    fn into_response(self) -> warp::reply::Response {
        let body: String = [
            String::from(HTML_HEAD_STR),
            self.0,
            String::from(HTML_TAIL_STR),
        ]
        .join("");
        let mut response = warp::reply::Response::new(body.into());
        *response.status_mut() = http::StatusCode::OK;
        response.headers_mut().insert(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static("text/html; charset=UTF-8"),
        );
        response
    }
}

#[derive(PartialEq, Eq, Hash, Clone)]
struct CacheKey {
    path: PathBuf,
    modified: ::std::time::SystemTime,
}

type Cache = ::std::sync::Arc<Mutex<HashMap<CacheKey, String>>>;

#[derive(Clone)]
struct Context {
    base_dir: PathBuf,
    cache: Cache,
}

lazy_static! {
    static ref CLEANER: ammonia::Builder<'static> = {
        let mut d = ammonia::Builder::default();
        d.add_generic_attributes(&["id", "class"]);
        d
    };
    static ref CM_OPTIONS: ComrakOptions = ComrakOptions {
        smart: true,
        unsafe_: true,
        ext_superscript: true,
        ext_autolink: true,
        ext_table: true,
        ext_header_ids: Some(String::new()),
        ..ComrakOptions::default()
    };
}

fn process(input: &str) -> String {
    CLEANER
        .clean(&markdown_to_html(input, &CM_OPTIONS))
        .to_string()
}

async fn file_metadata(f: &tokio::fs::File) -> Result<::std::fs::Metadata, Rejection> {
    match f.metadata().await {
        Ok(meta) => Ok(meta),
        Err(_) => Err(warp::reject::not_found()),
    }
}

async fn read_file(f: &mut tokio::fs::File, size: u64) -> Result<String, Rejection> {
    let mut buf = String::with_capacity(size.try_into().unwrap());
    match f.read_to_string(&mut buf).await {
        Ok(_) => Ok(buf),
        Err(_) => Err(warp::reject()),
    }
}

fn evict(path: &PathBuf, cache: &mut HashMap<CacheKey, String>) {
    let keys: Vec<CacheKey> = cache
        .keys()
        .filter(|k| &k.path == path)
        .map(|k| k.clone())
        .collect();

    for k in keys {
        cache.remove(&k);
    }
}

async fn process_file(path: &PathBuf, cache: Cache) -> Result<Rendered, Rejection> {
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|_| warp::reject())?;
    let meta = file_metadata(&file).await?;
    let ck = CacheKey {
        modified: meta.modified().expect("We want to run on a platform where https://doc.rust-lang.org/std/fs/struct.Metadata.html#method.modified is available"),
        path: path.clone(),
    };

    let mut cache = cache.lock().await;

    match cache.get(&ck) {
        Some(s) => Ok(Rendered(s.clone())),
        None => {
            let input = read_file(&mut file, meta.len()).await?;
            let output = process(&input);
            evict(path, &mut cache);
            cache.insert(ck, output.clone());
            Ok(Rendered(output))
        }
    }
}

async fn convert(
    path: warp::filters::path::FullPath,
    context: Context,
) -> Result<impl warp::Reply, Rejection> {
    let req_path_str = path.as_str();
    let req_path = PathBuf::from(req_path_str.get(1..).unwrap_or("index.md"));
    let maybe_full_path = context.base_dir.clone().join(req_path.clone());
    let full_path = if maybe_full_path.is_dir() {
        maybe_full_path.clone().join("index.md")
    } else {
        maybe_full_path.clone()
    };

    match full_path.extension() {
        Some(ext) if ext == "md" => process_file(&full_path, context.cache).await,
        Some(_) => Err(warp::reject::custom(MarkdownError::NotMarkdown)),
        None => {
            let full_path_ext = full_path.with_extension("md");
            if full_path_ext.exists() {
                process_file(&full_path_ext, context.cache).await
            } else {
                Err(warp::reject::not_found())
            }
        }
    }
}

fn inject_context(ctx: Context) -> warp::filters::BoxedFilter<(Context,)> {
    warp::any().map(move || ctx.clone()).boxed()
}

fn print_log(info: warp::filters::log::Info) {
    use chrono::Utc;
    eprintln!(
        "{} {} {} {} {} {}",
        Utc::now().to_rfc3339(),
        info.remote_addr()
            .map(|a| format!("{}", a.ip()))
            .unwrap_or("-".into()),
        info.method(),
        info.path(),
        info.status(),
        info.elapsed().as_millis(),
    );
}

// #[tokio::main]
async fn serve(argv0: String, argv1: String) {
    let base_dir = PathBuf::from(&argv0);
    let dir = warp::fs::dir(base_dir.clone());
    let cache: Cache = ::std::sync::Arc::new(Mutex::new(HashMap::new()));
    let ctx = Context {
        base_dir: base_dir.clone(),
        cache: cache,
    };
    let get = warp::get()
        .and(warp::path::full())
        .and(inject_context(ctx.clone()))
        .and_then(convert)
        .or(dir)
        .with(warp::log::custom(print_log));
    let service = warp::serve(get);
    let addr: std::net::SocketAddr = argv1.parse().expect("not a valid address");
    println!("running on http://{}", addr);
    service.run(addr).await;
}

fn main() {
    let base_dir = Arg::with_name("base_dir")
        .short("d")
        .long("dir")
        .value_name("base_dir")
        .help("Directory to serve")
        .takes_value(true);

    let addr = Arg::with_name("address")
        .short("a")
        .long("address")
        .value_name("address")
        .help("address to listen to")
        .takes_value(true);

    let matches = App::new("mdserve")
        .version("0.1")
        .about("Serve you some markdown")
        .arg(base_dir)
        .arg(addr)
        .get_matches();

    let argv0 = matches.value_of("base_dir");
    let argv1 = matches.value_of("address");

    match (argv0, argv1) {
        (Some(base_dir), Some(addr)) => {
            let mut rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(serve(String::from(base_dir), String::from(addr)));
        }
        _ => {
            println!("args didnt work {:?}, {:?}", argv0, argv1);
            ()
        }
    }
}

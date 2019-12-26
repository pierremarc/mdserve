use clap::{App, Arg};
use comrak::{markdown_to_html, ComrakOptions};
use std::path::PathBuf;
use tokio::{self};
use warp::{self, Filter, Rejection};

#[derive(Debug)]
enum MarkdownError {
    NotMarkdown,
    Decoding,
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

fn options() -> ComrakOptions {
    ComrakOptions {
        smart: true,
        ext_superscript: true,
        ext_autolink: true,
        ext_table: true,
        ext_header_ids: Some(String::new()),
        ..ComrakOptions::default()
    }
}

fn process(input: &str) -> String {
    let options = options();
    markdown_to_html(input, &options)
}

async fn process_file(path: &PathBuf) -> Result<Rendered, Rejection> {
    if let Ok(data) = tokio::fs::read(path).await {
        match String::from_utf8(data) {
            Ok(input) => Ok(Rendered(process(&input))),
            Err(_) => Err(warp::reject::custom(MarkdownError::Decoding)),
        }
    } else {
        Err(warp::reject())
    }
}

async fn convert(
    root_path: PathBuf,
    path: warp::filters::path::FullPath,
) -> Result<impl warp::Reply, Rejection> {
    let req_path_str = path.as_str();
    let req_path = dbg!(PathBuf::from(req_path_str.get(1..).unwrap_or("index.md")));
    let maybe_full_path = dbg!(root_path.clone().join(req_path.clone()));
    let full_path = if maybe_full_path.is_dir() {
        maybe_full_path.clone().join("index.md")
    } else {
        maybe_full_path.clone()
    };

    match dbg!(full_path.extension()) {
        Some(ext) if ext == "md" => process_file(&full_path).await,
        Some(_) => Err(warp::reject::custom(MarkdownError::NotMarkdown)),
        None => {
            let full_path_ext = dbg!(full_path.with_extension("md"));
            if dbg!(full_path_ext.exists()) {
                process_file(&full_path_ext).await
            } else {
                Err(warp::reject())
            }
        }
    }
}

// #[tokio::main]
async fn serve(argv0: String, argv1: String) {
    let base_dir = PathBuf::from(&argv0);
    let dir = warp::fs::dir(base_dir.clone());
    let get = warp::get()
        .map(move || base_dir.clone())
        .and(warp::path::full())
        .and_then(convert)
        .or(dir);
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

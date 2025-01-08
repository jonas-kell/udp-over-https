use actix_web::{web, App, HttpResponse, HttpServer, Responder};
use clap::{Parser, ValueEnum};
use reqwest::Client;

/// Simple program demonstrating HTTP server and client modes with Actix
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Mode of operation: 'server' or 'client'
    #[arg(value_enum)]
    mode: Mode,
}

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, ValueEnum, Debug)]
enum Mode {
    Server,
    Client,
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    let args = Args::parse();

    match args.mode {
        Mode::Server => run_server().await,
        Mode::Client => run_client().await,
    }
}

/// Start the HTTP server
async fn run_server() -> std::io::Result<()> {
    let addr = "127.0.0.1:8080";

    println!("Starting server at http://{}", addr);

    HttpServer::new(|| App::new().route("/", web::get().to(hello_world)))
        .bind(addr)?
        .run()
        .await
}

/// Handler for the server's `/` endpoint
async fn hello_world() -> impl Responder {
    HttpResponse::Ok().json(serde_json::json!({ "message": "Hello, Actix!" }))
}

/// Run the client
async fn run_client() -> std::io::Result<()> {
    let url = "http://127.0.0.1:8080";

    let client = Client::new();
    let res = client
        .get(url)
        .send()
        .await
        .expect("Failed to send request");

    let status = res.status();
    let body = res.text().await.expect("Failed to read response body");

    println!("Response Status: {}", status);
    println!("Response Body: {}", body);

    Ok(())
}

mod handlers;
mod service;

use axum::{routing::get, Router};

fn app() -> Router {
    Router::new()
        .route("/articles", get(handlers::list_articles).post(handlers::create_article))
}

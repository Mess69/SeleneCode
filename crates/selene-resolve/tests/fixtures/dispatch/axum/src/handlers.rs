use crate::service;

pub async fn list_articles() -> String {
    service::list()
}

pub async fn create_article() -> String {
    service::create()
}

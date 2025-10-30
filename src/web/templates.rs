use askama::Template;
use crate::storage::Worker;
use crate::job::StratumJob;

// -------- Askama filters --------

pub fn format_rfc3339(dt: &chrono::DateTime<chrono::Utc>) -> askama::Result<String> {
    Ok(dt.to_rfc3339())
}

// Register filters in this module (Askama finds functions in the same module)

// -------- Templates --------

#[derive(Template)]
#[template(path = "dashboard.html")]
pub struct DashboardTemplate {
    pub workers: Vec<Worker>,
    pub job: Option<StratumJob>,
}

#[derive(Template)]
#[template(path = "workers.html")]
pub struct WorkersTemplate {
    pub workers: Vec<Worker>,
}

// We convert Options to display-friendly Strings in the handler to keep the template simple
#[derive(Clone)]
pub struct BlockRow {
    pub height: String,
    pub hash: String,
    pub status: String,
    pub timestamp: String,
}

#[derive(Template)]
#[template(path = "blocks.html")]
pub struct BlocksTemplate {
    pub blocks: Vec<BlockRow>,
}
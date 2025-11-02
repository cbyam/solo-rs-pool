use askama::Template;
use crate::{job::StratumJob, storage::Worker};

/// Askama expects custom filters under a module named `filters`
pub mod filters {
    use askama::Result;

    pub fn format_rfc3339(dt: &chrono::DateTime<chrono::Utc>) -> Result<String> {
        Ok(dt.to_rfc3339())
    }
}

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
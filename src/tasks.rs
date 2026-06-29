use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TaskRecord {
    pub id: String,
    pub title: String,
    pub body: String,
    pub created_by: String,
    pub assigned_to: Option<String>,
    pub status: String,
    pub lease_owner: Option<String>,
    pub lease_expires_at: Option<i64>,
    pub result_summary: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub completed_at: Option<i64>,
}

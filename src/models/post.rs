//! LinkedIn post model (port of `models/post.py`).

use std::fmt;

use serde::{Deserialize, Serialize};

use super::person::opt;
use crate::error::Result;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Post {
    pub linkedin_url: Option<String>,
    pub urn: Option<String>,
    pub text: Option<String>,
    pub posted_date: Option<String>,
    pub reactions_count: Option<i64>,
    pub comments_count: Option<i64>,
    pub reposts_count: Option<i64>,
    #[serde(default)]
    pub image_urls: Vec<String>,
    pub video_url: Option<String>,
    pub article_url: Option<String>,
}

impl Post {
    pub fn to_dict(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("Post is always serializable")
    }

    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string(self)?)
    }

    pub fn to_json_pretty(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }
}

fn opt_i64(v: Option<i64>) -> String {
    v.map_or_else(|| "None".to_string(), |n| n.to_string())
}

impl fmt::Display for Post {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let preview = match &self.text {
            Some(t) if t.chars().count() > 80 => {
                format!("{}...", t.chars().take(80).collect::<String>())
            }
            other => opt(other).to_string(),
        };
        write!(
            f,
            "<Post\n  Text: {}\n  Posted: {}\n  Reactions: {}\n  Comments: {}>",
            preview,
            opt(&self.posted_date),
            opt_i64(self.reactions_count),
            opt_i64(self.comments_count)
        )
    }
}

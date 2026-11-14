//! LinkedIn job posting model (port of `models/job.py`).

use std::fmt;

use serde::{Deserialize, Serialize};

use super::person::opt;
use crate::error::{Error, Result};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub linkedin_url: String,
    pub job_title: Option<String>,
    pub company: Option<String>,
    pub company_linkedin_url: Option<String>,
    pub location: Option<String>,
    pub posted_date: Option<String>,
    pub applicant_count: Option<String>,
    pub job_description: Option<String>,
    pub benefits: Option<String>,
}

impl Job {
    pub fn validate(&self) -> Result<()> {
        if !self.linkedin_url.contains("linkedin.com/jobs") {
            return Err(Error::Validation(
                "Must be a valid LinkedIn job URL (contains /jobs)".into(),
            ));
        }
        Ok(())
    }

    pub fn to_dict(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("Job is always serializable")
    }

    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string(self)?)
    }

    pub fn to_json_pretty(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }
}

impl fmt::Display for Job {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "<Job {} at {}\n  Location: {}\n  Posted: {}\n  Applicants: {}>",
            opt(&self.job_title),
            opt(&self.company),
            opt(&self.location),
            opt(&self.posted_date),
            opt(&self.applicant_count)
        )
    }
}

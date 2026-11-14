//! LinkedIn person/profile models (port of `models/person.py`).

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Interest {
    pub name: String,
    pub category: String,
    pub linkedin_url: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Contact {
    #[serde(rename = "type")]
    pub contact_type: String,
    pub value: String,
    pub label: Option<String>,
}

/// Work experience.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Experience {
    pub position_title: Option<String>,
    pub institution_name: Option<String>,
    /// "CDI", "Stage", "Full-time"… (new LinkedIn UI only).
    #[serde(default)]
    pub employment_type: Option<String>,
    pub linkedin_url: Option<String>,
    pub from_date: Option<String>,
    pub to_date: Option<String>,
    pub duration: Option<String>,
    pub location: Option<String>,
    pub description: Option<String>,
}

/// Education entry.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Education {
    pub institution_name: Option<String>,
    pub degree: Option<String>,
    pub linkedin_url: Option<String>,
    pub from_date: Option<String>,
    pub to_date: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Accomplishment {
    pub category: String,
    pub title: String,
    pub issuer: Option<String>,
    pub issued_date: Option<String>,
    pub credential_id: Option<String>,
    pub credential_url: Option<String>,
    pub description: Option<String>,
}

/// A complete LinkedIn profile.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Person {
    pub linkedin_url: String,
    pub name: Option<String>,
    /// Headline under the name ("Consultante", "CEO at Acme"…).
    #[serde(default)]
    pub headline: Option<String>,
    pub location: Option<String>,
    pub about: Option<String>,
    #[serde(default)]
    pub open_to_work: bool,
    #[serde(default)]
    pub experiences: Vec<Experience>,
    #[serde(default)]
    pub educations: Vec<Education>,
    #[serde(default)]
    pub interests: Vec<Interest>,
    #[serde(default)]
    pub accomplishments: Vec<Accomplishment>,
    #[serde(default)]
    pub contacts: Vec<Contact>,
}

impl Person {
    /// Validates the model (Pydantic `field_validator` equivalent).
    pub fn validate(&self) -> Result<()> {
        if !self.linkedin_url.contains("linkedin.com/in/") {
            return Err(Error::Validation(
                "Must be a valid LinkedIn profile URL (contains /in/)".into(),
            ));
        }
        Ok(())
    }

    /// Company name from the most recent experience.
    pub fn company(&self) -> Option<&str> {
        self.experiences.first()?.institution_name.as_deref()
    }

    /// Job title from the most recent experience.
    pub fn job_title(&self) -> Option<&str> {
        self.experiences.first()?.position_title.as_deref()
    }

    pub fn to_dict(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("Person is always serializable")
    }

    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string(self)?)
    }

    pub fn to_json_pretty(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }
}

impl fmt::Display for Person {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "<Person {}\n  Company: {}\n  Title: {}\n  Location: {}\n  Experiences: {}\n  Education: {}>",
            opt(&self.name),
            opt_str(self.company()),
            opt_str(self.job_title()),
            opt(&self.location),
            self.experiences.len(),
            self.educations.len()
        )
    }
}

/// Formats an `Option` like Python formats `None`.
pub(crate) fn opt(v: &Option<String>) -> &str {
    v.as_deref().unwrap_or("None")
}

pub(crate) fn opt_str(v: Option<&str>) -> &str {
    v.unwrap_or("None")
}

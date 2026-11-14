//! LinkedIn company models (port of `models/company.py`).

use std::fmt;

use serde::{Deserialize, Serialize};

use super::person::opt;
use crate::error::{Error, Result};

/// Summary information for affiliated/showcase companies.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CompanySummary {
    pub linkedin_url: Option<String>,
    pub name: Option<String>,
    pub followers: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Employee {
    pub name: String,
    /// LinkedIn headline (usually "Job title @Company").
    pub designation: Option<String>,
    pub linkedin_url: Option<String>,
    /// Seniority level of the designation (see [`crate::seniority`]).
    #[serde(default)]
    pub seniority: Option<crate::seniority::Seniority>,
    /// 0 (unknown) … 9 (executive); employees are sorted by it, highest first.
    #[serde(default)]
    pub seniority_rank: u8,
}

impl Employee {
    /// Builds an employee and classifies its designation.
    pub fn new(
        name: impl Into<String>,
        designation: Option<String>,
        linkedin_url: Option<String>,
    ) -> Self {
        // "--" / "·" placeholders are not designations.
        let designation = designation.filter(|d| d.chars().any(char::is_alphanumeric));
        let level = designation.as_deref().map_or(
            crate::seniority::Seniority::Unknown,
            crate::seniority::classify,
        );
        Employee {
            name: name.into(),
            designation,
            linkedin_url,
            seniority: Some(level),
            seniority_rank: level.rank(),
        }
    }
}

/// Sorts by seniority (highest first), then by name.
pub fn sort_by_seniority(employees: &mut [Employee]) {
    employees.sort_by(|a, b| {
        b.seniority_rank
            .cmp(&a.seniority_rank)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
}

/// A LinkedIn company page.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Company {
    pub linkedin_url: String,
    pub name: Option<String>,
    pub about_us: Option<String>,
    pub website: Option<String>,
    pub phone: Option<String>,
    pub headquarters: Option<String>,
    pub founded: Option<String>,
    pub industry: Option<String>,
    pub company_type: Option<String>,
    pub company_size: Option<String>,
    pub specialties: Option<String>,
    pub headcount: Option<i64>,
    #[serde(default)]
    pub showcase_pages: Vec<CompanySummary>,
    #[serde(default)]
    pub affiliated_companies: Vec<CompanySummary>,
    #[serde(default)]
    pub employees: Vec<Employee>,
}

impl Company {
    pub fn validate(&self) -> Result<()> {
        if !self.linkedin_url.contains("linkedin.com/company/") {
            return Err(Error::Validation(
                "Must be a valid LinkedIn company URL (contains /company/)".into(),
            ));
        }
        Ok(())
    }

    pub fn to_dict(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("Company is always serializable")
    }

    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string(self)?)
    }

    pub fn to_json_pretty(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }
}

impl fmt::Display for Company {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "<Company {}\n  Industry: {}\n  Size: {}\n  Headquarters: {}\n  Employees: {}>",
            opt(&self.name),
            opt(&self.industry),
            opt(&self.company_size),
            opt(&self.headquarters),
            self.employees.len()
        )
    }
}

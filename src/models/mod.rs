//! Serde data models (port of the Pydantic models).

mod company;
mod job;
mod person;
mod post;

pub use company::{Company, CompanySummary, Employee, sort_by_seniority};
pub use job::Job;
pub use person::{Accomplishment, Contact, Education, Experience, Interest, Person};
pub use post::Post;

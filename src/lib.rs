//! Measure a folder of stems and write a delivery sheet.
//!
//! The binary is the product. Library entry points exist so the sheet, the
//! receipt, and the fixture generator can be tested without parsing stdout.

#![forbid(unsafe_code)]

mod aiff;
mod error;
mod fixture;
mod measure;
mod render;
mod session;
mod wav;

pub use error::Error;
pub use fixture::write_fixtures;
pub use measure::Rules;
pub use render::{render_html, render_json, render_table};
pub use session::{measure, Delivery, FileReport, SessionReport};

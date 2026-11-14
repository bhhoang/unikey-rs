//! UniKey Vietnamese input engine, ported to Rust.
//!
//! The conversion logic is a port of Pham Kim Long's UniKey engine (`ukengine`
//! from the ibus-unikey 0.6.1 sources, http://unikey.org). Output is always
//! Unicode; the legacy-charset converters of the original (TCVN3, VNI, ...) are
//! not ported.

#![allow(non_upper_case_globals)]

pub mod charset;
pub mod engine;
pub mod input;
pub mod keymap;
pub mod macros;
pub mod settings;
pub mod tables;

pub use engine::{Engine, Options, Output};
pub use input::InputMethod;

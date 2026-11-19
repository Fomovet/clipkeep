#![allow(dead_code)]

// Import production code directly so tests cannot drift into a duplicated implementation.
#[path = "../../../src/db.rs"]
mod db;

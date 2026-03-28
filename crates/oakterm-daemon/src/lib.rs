// PTY fd requires BorrowedFd::borrow_raw for async I/O setup and reads.
#![allow(unsafe_code)]

pub mod server;
pub mod socket;

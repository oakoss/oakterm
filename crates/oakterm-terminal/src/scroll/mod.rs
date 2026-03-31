//! Scrollback buffer: bounded ring buffer for recent terminal output.

pub mod archive;
pub mod archive_manager;
mod hot_buffer;
pub mod row_codec;

pub use hot_buffer::HotBuffer;

//! Scrollback buffer: bounded ring buffer for recent terminal output.

mod hot_buffer;
pub mod row_codec;

pub use hot_buffer::HotBuffer;

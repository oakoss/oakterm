/// Parsed numeric parameters from CSI/DCS sequences.
///
/// Parameters are separated by `;` (semicolon) or `:` (colon, for
/// sub-parameters like SGR `38:2:R:G:B`). Missing parameters default to 0.
#[derive(Debug)]
#[non_exhaustive]
pub struct Params {
    params: Vec<Vec<u16>>,
}

impl Params {
    #[expect(dead_code, reason = "used when VT parser constructs params")]
    pub(crate) fn new(params: Vec<Vec<u16>>) -> Self {
        Self { params }
    }

    pub fn iter(&self) -> impl Iterator<Item = &[u16]> {
        self.params.iter().map(Vec::as_slice)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.params.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.params.is_empty()
    }
}

/// Low-level interface between the VT parser state machine and the handler.
///
/// The parser calls these methods as it recognizes sequence boundaries.
/// `print` is required; all other methods default to no-ops for incremental
/// development.
#[allow(unused_variables)]
pub trait Perform {
    /// Printable character in ground state. Required — a handler that drops
    /// printable characters is always a bug.
    fn print(&mut self, c: char);

    /// C0 or C1 control byte in ground state.
    fn execute(&mut self, byte: u8) {}

    /// CSI sequence complete. `action` is the final byte (0x40-0x7E).
    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {}

    /// ESC sequence complete. `byte` is the final byte.
    fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8) {}

    /// OSC sequence complete. `params` are semicolon-delimited byte slices.
    fn osc_dispatch(&mut self, params: &[&[u8]], bell_terminated: bool) {}

    /// DCS sequence started.
    fn hook(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {}

    /// DCS data byte.
    fn put(&mut self, byte: u8) {}

    /// DCS sequence ended.
    fn unhook(&mut self) {}

    /// APC sequence started (Kitty graphics).
    fn apc_start(&mut self) {}

    /// APC data byte.
    fn apc_put(&mut self, byte: u8) {}

    /// APC sequence ended. Handler processes buffered APC content.
    fn apc_end(&mut self) {}
}

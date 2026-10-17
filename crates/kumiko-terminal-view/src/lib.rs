//! The terminal view (L1, spec section 19): VT emulation by alacritty_terminal, drawn on GPUI.
//! The shell's bytes come from the core's terminal (the Process service's PTY) straight into
//! the emulator; keys go back as xterm sequences.

pub mod colors;
pub mod keys;
pub mod view;

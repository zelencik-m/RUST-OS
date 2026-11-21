//! PS/2 keyboard: scancodes in, decoded keys to the TTY.

use pc_keyboard::{layouts, DecodedKey, HandleControl, Keyboard, ScancodeSet1};

use crate::sync::IrqSafeMutex;

/// Scancode set 1 → keys, US layout.
///
/// Ctrl+letter is mapped to the ASCII control characters (Ctrl-C = 0x03,
/// Ctrl-D = 0x04, Ctrl-L = 0x0C, ...), which is what the TTY expects.
pub struct KeyDecoder {
    keyboard: Keyboard<layouts::Us104Key, ScancodeSet1>,
}

impl KeyDecoder {
    pub const fn new() -> Self {
        Self {
            keyboard: Keyboard::new(
                ScancodeSet1::new(),
                layouts::Us104Key,
                HandleControl::MapLettersToUnicode,
            ),
        }
    }

    /// Feed one scancode byte. Returns a key once a complete key press has
    /// arrived. Releases, modifier changes and prefix bytes (`0xE0`) give
    /// `None`.
    pub fn feed(&mut self, scancode: u8) -> Option<DecodedKey> {
        match self.keyboard.add_byte(scancode) {
            Ok(Some(event)) => self.keyboard.process_keyevent(event),
            _ => None,
        }
    }
}

impl Default for KeyDecoder {
    fn default() -> Self {
        Self::new()
    }
}

static DECODER: IrqSafeMutex<KeyDecoder> = IrqSafeMutex::new(KeyDecoder::new());

/// Called by the keyboard IRQ handler with each byte read from port 0x60.
pub fn handle_scancode(scancode: u8) {
    // Drop the decoder lock before the TTY runs: it may echo and wake threads.
    let key = DECODER.lock().feed(scancode);
    if let Some(key) = key {
        crate::tty::push_key(key);
    }
}

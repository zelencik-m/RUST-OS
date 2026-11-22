//! The console terminal: turns key presses into the bytes `read(0)` returns.
//!
//! The pieces:
//! - [`LineDiscipline`]: a pure state machine (keys in; echo and readable
//!   bytes out). It has no locks or I/O, so tests can drive it directly.
//! - [`push_key`]: called from the keyboard IRQ. It feeds the line
//!   discipline, echoes, and wakes blocked readers.
//! - [`read`]: called by `read(0)` (via the console inode). It blocks until
//!   the line discipline has something to return.
//!
//! **Nothing on the `push_key` path may allocate.** It runs inside the
//! keyboard interrupt, and the heap allocator's lock is not IRQ-safe, so an
//! interrupted allocation would deadlock. Use fixed-size buffers.
//!
//! Lock order: TTY → SCHEDULER → PROCESS_MANAGER → MEMORY_MANAGER.
//!
//! ## Contract (what `tests/tty.rs` checks)
//!
//! | Input            | Cooked (default)                         | Raw                  |
//! |------------------|------------------------------------------|----------------------|
//! | printable ASCII  | appended to the edit line, echoed        | readable, no echo    |
//! | Backspace (0x08) | drop last char, echo `\b \b`; no-op if empty | readable `0x08`  |
//! | Enter (`\n`)     | edit line + `\n` becomes readable, echo `\n` | readable `\n`    |
//! | Ctrl-D (0x04)    | empty line: one EOF; else line readable without `\n` | readable `0x04` |
//! | Ctrl-C (0x03)    | discard edit line, echo `^C\n`           | readable `0x03`      |
//! | arrow keys       | ignored                                  | readable `ESC [ A/B/C/D` |
//! | other ASCII (Tab, ESC, Ctrl-L, ...) | ignored               | readable as is       |
//! | non-ASCII, other raw keys (F1, Home, ...) | ignored         | ignored              |
//!
//! - "Printable ASCII" is `0x20..=0x7e`. Raw mode passes every ASCII
//!   character (`0x00..=0x7f`) through as its byte.
//! - Cooked `read` returns at most one line (the rest of a too-long line
//!   comes on the next read). An EOF makes one `read` return 0, in order
//!   with the lines around it: `a\n`, Ctrl-D, Ctrl-D reads as `a\n`, then 0,
//!   then 0.
//! - Raw `read` returns every available byte that fits.
//! - The edit line holds at most [`LINE_MAX`] characters. Further printable
//!   keys are dropped without echo, but Enter/Backspace/Ctrl keys still work.
//! - Readable bytes are capped at [`INPUT_CAPACITY`]. Raw bytes beyond that
//!   are dropped. A cooked line is committed only if it fits completely
//!   (line + `\n`); otherwise the whole line is dropped (the echo of `\n`
//!   still happens).
//! - Switching mode discards the cooked edit line; bytes that are already
//!   readable stay readable.

use core::sync::atomic::{AtomicU64, Ordering};

use pc_keyboard::{DecodedKey, KeyCode};

use crate::process::process_manager;
use crate::process::{EXIT_INTERRUPTED, PID};
use crate::scheduler::wait_queue::{Interrupted, WaitQueue};
use crate::sync::IrqSafeMutex;

/// Longest cooked edit line, excluding the `\n` added on Enter.
pub const LINE_MAX: usize = 255;
/// Most bytes that can be readable (committed lines, EOF markers aside).
pub const INPUT_CAPACITY: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TtyMode {
    /// Line editing and echo; `read` returns whole lines.
    Cooked,
    /// No editing or echo; `read` returns bytes as they arrive.
    Raw,
}

impl TtyMode {
    /// Decode the `SYS_TTY_MODE` argument: `0` = cooked, `1` = raw.
    pub fn from_raw(raw: u64) -> Option<Self> {
        match raw {
            0 => Some(Self::Cooked),
            1 => Some(Self::Raw),
            _ => None,
        }
    }

    pub fn as_raw(self) -> u64 {
        match self {
            Self::Cooked => 0,
            Self::Raw => 1,
        }
    }
}

/// Bytes to echo after one key press. Fixed-size, because it is produced
/// inside the keyboard IRQ, which must not allocate.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Echo {
    bytes: [u8; Echo::CAPACITY],
    len: usize,
}

impl Echo {
    pub const CAPACITY: usize = 8;

    pub const fn none() -> Self {
        Self {
            bytes: [0; Self::CAPACITY],
            len: 0,
        }
    }

    /// Panics if `bytes` is longer than [`Echo::CAPACITY`].
    pub fn from_bytes(bytes: &[u8]) -> Self {
        assert!(bytes.len() <= Self::CAPACITY, "echo too long");
        let mut echo = Self::none();
        echo.bytes[..bytes.len()].copy_from_slice(bytes);
        echo.len = bytes.len();
        echo
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

impl core::fmt::Debug for Echo {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Echo({:?})", self.as_bytes())
    }
}

const BACKSPACE: u8 = 0x08;
const CTRL_C: u8 = 0x03;
const CTRL_D: u8 = 0x04;

/// Most queued records (lines, raw runs, EOFs). Every record except an EOF
/// holds at least one byte, so this only limits Ctrl-D spam.
const MAX_RECORDS: usize = INPUT_CAPACITY;

/// Fixed-capacity FIFO. Never allocates, and `new` is `const`.
struct Ring<T: Copy, const N: usize> {
    items: [T; N],
    head: usize,
    len: usize,
}

impl<T: Copy, const N: usize> Ring<T, N> {
    const fn new(fill: T) -> Self {
        Self {
            items: [fill; N],
            head: 0,
            len: 0,
        }
    }

    fn len(&self) -> usize {
        self.len
    }

    fn free(&self) -> usize {
        N - self.len
    }

    fn push_back(&mut self, item: T) -> bool {
        if self.len == N {
            return false;
        }
        self.items[(self.head + self.len) % N] = item;
        self.len += 1;
        true
    }

    fn pop_front(&mut self) -> Option<T> {
        if self.len == 0 {
            return None;
        }
        let item = self.items[self.head];
        self.head = (self.head + 1) % N;
        self.len -= 1;
        Some(item)
    }

    fn front_mut(&mut self) -> Option<&mut T> {
        if self.len == 0 {
            return None;
        }
        Some(&mut self.items[self.head])
    }

    fn back_mut(&mut self) -> Option<&mut T> {
        if self.len == 0 {
            return None;
        }
        Some(&mut self.items[(self.head + self.len - 1) % N])
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecordKind {
    /// A cooked line: ends with `\n`, or not if Ctrl-D flushed it.
    Line,
    /// Bytes that arrived in raw mode. Consecutive raw bytes share one record.
    Raw,
    /// Ctrl-D on an empty cooked line. Holds no bytes.
    Eof,
}

/// Describes the next `len` readable bytes. A cooked read stops at the end
/// of a record, which is how it returns one line at a time.
///
/// 4 bytes, so the 1024-entry record ring stays small.
#[derive(Debug, Clone, Copy)]
struct Record {
    len: u16,
    kind: RecordKind,
}

const _: () = assert!(INPUT_CAPACITY <= u16::MAX as usize, "Record::len is a u16");

/// Bytes that `read` can return, split into records.
///
/// Invariant: the record lengths add up to `bytes.len()`.
struct ReadQueue {
    bytes: Ring<u8, INPUT_CAPACITY>,
    records: Ring<Record, MAX_RECORDS>,
}

impl ReadQueue {
    const fn new() -> Self {
        Self {
            bytes: Ring::new(0),
            records: Ring::new(Record {
                len: 0,
                kind: RecordKind::Eof,
            }),
        }
    }

    /// Append `parts` (concatenated) as one record of `kind`. All or nothing:
    /// returns false and changes nothing if the bytes or the record don't fit.
    fn commit(&mut self, parts: &[&[u8]], kind: RecordKind) -> bool {
        let len: usize = parts.iter().map(|part| part.len()).sum();
        if len > self.bytes.free() {
            return false;
        }
        let extends_raw_run = kind == RecordKind::Raw
            && matches!(self.records.back_mut(), Some(last) if last.kind == RecordKind::Raw);
        if extends_raw_run {
            if let Some(last) = self.records.back_mut() {
                last.len += len as u16;
            }
        } else if !self.records.push_back(Record {
            len: len as u16,
            kind,
        }) {
            return false;
        }
        for &byte in parts.iter().flat_map(|part| part.iter()) {
            self.bytes.push_back(byte);
        }
        true
    }

    /// Cooked: one EOF, or bytes from the front record only. Raw: bytes
    /// across records, skipping EOFs (raw mode has no EOF).
    fn read(&mut self, buf: &mut [u8], mode: TtyMode) -> usize {
        let mut n = 0;
        while n < buf.len() {
            let Some(record) = self.records.front_mut() else {
                break;
            };
            if record.kind == RecordKind::Eof {
                self.records.pop_front();
                match mode {
                    TtyMode::Cooked => return 0,
                    TtyMode::Raw => continue,
                }
            }

            let take = core::cmp::min(record.len as usize, buf.len() - n);
            for slot in &mut buf[n..n + take] {
                *slot = self.bytes.pop_front().expect("record longer than its bytes");
            }
            record.len -= take as u16;
            if record.len == 0 {
                self.records.pop_front();
            }
            n += take;

            if mode == TtyMode::Cooked {
                break;
            }
        }
        n
    }
}

/// The terminal's input state machine. See the module docs for the contract.
///
/// Fixed-size throughout (it runs inside the keyboard IRQ), about 5.3 KiB.
pub struct LineDiscipline {
    mode: TtyMode,
    /// The cooked line being edited, not readable until Enter or Ctrl-D.
    line: [u8; LINE_MAX],
    line_len: usize,
    queue: ReadQueue,
}

impl LineDiscipline {
    /// A cooked, empty terminal.
    ///
    /// Must stay `const`: the console's state lives in a `static` built at
    /// compile time. Built at runtime (e.g. by `lazy_static`), the
    /// multi-KB struct would be constructed and copied on the stack of
    /// whichever thread touched the TTY first. Kernel stacks are only
    /// 16 KiB with no guard page, so that silently overflows them (seen as
    /// a jump to address 0).
    pub const fn new() -> Self {
        Self {
            mode: TtyMode::Cooked,
            line: [0; LINE_MAX],
            line_len: 0,
            queue: ReadQueue::new(),
        }
    }

    pub fn mode(&self) -> TtyMode {
        self.mode
    }

    /// Switch mode. Discards the cooked edit line; readable bytes stay.
    pub fn set_mode(&mut self, mode: TtyMode) {
        self.mode = mode;
        self.line_len = 0;
    }

    /// Process one key press. Returns the bytes to echo (empty in raw mode).
    pub fn input(&mut self, key: DecodedKey) -> Echo {
        match self.mode {
            TtyMode::Cooked => self.input_cooked(key),
            TtyMode::Raw => {
                self.input_raw(key);
                Echo::none()
            }
        }
    }

    fn input_raw(&mut self, key: DecodedKey) {
        match key {
            DecodedKey::Unicode(c) if c.is_ascii() => {
                self.queue.commit(&[&[c as u8]], RecordKind::Raw);
            }
            DecodedKey::RawKey(code) => {
                if let Some(sequence) = arrow_sequence(code) {
                    self.queue.commit(&[sequence], RecordKind::Raw);
                }
            }
            DecodedKey::Unicode(_) => {}
        }
    }

    fn input_cooked(&mut self, key: DecodedKey) -> Echo {
        let byte = match key {
            DecodedKey::Unicode(c) if c.is_ascii() => c as u8,
            _ => return Echo::none(),
        };
        match byte {
            0x20..=0x7e => {
                if self.line_len == LINE_MAX {
                    return Echo::none();
                }
                self.line[self.line_len] = byte;
                self.line_len += 1;
                Echo::from_bytes(&[byte])
            }
            BACKSPACE => {
                if self.line_len == 0 {
                    return Echo::none();
                }
                self.line_len -= 1;
                // VGA backspace doesn't erase: back, blank, back.
                Echo::from_bytes(b"\x08 \x08")
            }
            b'\n' => {
                // Dropped as a whole if it doesn't fit; never truncated.
                self.queue
                    .commit(&[&self.line[..self.line_len], b"\n"], RecordKind::Line);
                self.line_len = 0;
                Echo::from_bytes(b"\n")
            }
            CTRL_D => {
                if self.line_len == 0 {
                    self.queue.commit(&[], RecordKind::Eof);
                } else {
                    self.queue
                        .commit(&[&self.line[..self.line_len]], RecordKind::Line);
                    self.line_len = 0;
                }
                Echo::none()
            }
            CTRL_C => {
                self.line_len = 0;
                Echo::from_bytes(b"^C\n")
            }
            _ => Echo::none(),
        }
    }

    /// Whether `read` would return right now (data, or a pending EOF).
    pub fn readable(&self) -> bool {
        match self.mode {
            TtyMode::Cooked => self.queue.records.len() > 0,
            // EOF records don't count: raw mode has no EOF.
            TtyMode::Raw => self.queue.bytes.len() > 0,
        }
    }

    /// Take readable bytes into `buf` without blocking.
    ///
    /// Call only when `readable()` is true. Returns 0 when it consumed an EOF.
    /// Cooked: never reads past the end of the first line. Raw: as much as
    /// fits.
    pub fn read(&mut self, buf: &mut [u8]) -> usize {
        self.queue.read(buf, self.mode)
    }
}

/// The bytes a VT100 terminal sends for an arrow key.
fn arrow_sequence(code: KeyCode) -> Option<&'static [u8]> {
    match code {
        KeyCode::ArrowUp => Some(b"\x1b[A"),
        KeyCode::ArrowDown => Some(b"\x1b[B"),
        KeyCode::ArrowRight => Some(b"\x1b[C"),
        KeyCode::ArrowLeft => Some(b"\x1b[D"),
        _ => None,
    }
}

impl Default for LineDiscipline {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// The global console terminal
// ---------------------------------------------------------------------------

/// Built at compile time; see [`LineDiscipline::new`].
static TTY: IrqSafeMutex<LineDiscipline> = IrqSafeMutex::new(LineDiscipline::new());

/// Threads blocked in [`read`].
static READERS: WaitQueue = WaitQueue::new();

/// The process Ctrl-C kills, as a raw PID (0 = none; PID 0 is the kernel,
/// which can't be the foreground). A shell sets it to the child it's
/// waiting for (`SYS_TTY_FOREGROUND`). An atomic, so the keyboard IRQ can
/// read it without a lock.
static FOREGROUND: AtomicU64 = AtomicU64::new(0);

/// Feed one decoded key into the console. Called from the keyboard IRQ, and
/// directly by tests. Allocation-free.
///
/// A cooked-mode Ctrl-C also asks the foreground process (if any) to exit
/// with `EXIT_INTERRUPTED`. In raw mode, Ctrl-C is just byte 0x03 for the
/// reader (Linux: `ISIG` off).
pub fn push_key(key: DecodedKey) {
    let (echoed, readable, interrupt) = {
        let mut tty = TTY.lock();
        let interrupt =
            tty.mode() == TtyMode::Cooked && key == DecodedKey::Unicode(CTRL_C as char);
        let echoed = tty.input(key);
        (echoed, tty.readable(), interrupt)
    };
    // Echo outside the TTY lock: printing takes the VGA/serial locks.
    echo(echoed.as_bytes());
    if readable {
        READERS.wake_all();
    }
    if interrupt {
        if let Some(pid) = foreground() {
            process_manager::request_kill(pid, EXIT_INTERRUPTED);
        }
    }
}

/// Block until the console has input, then copy it into `buf`. Returns the
/// number of bytes read; 0 means EOF (Ctrl-D in cooked mode).
///
/// `Err(Interrupted)` if the reading process gets a kill request (Ctrl-C)
/// while it waits.
pub fn read(buf: &mut [u8]) -> Result<usize, Interrupted> {
    if buf.is_empty() {
        return Ok(0);
    }
    // Checking and reading under one TTY lock is what stops two readers
    // from both taking the same line: the loser finds nothing readable and
    // goes back to sleep. The kill check comes first, with no lock held
    // (it takes SCHEDULER and PROCESS_MANAGER).
    READERS.wait_until(|| {
        if process_manager::current_kill_requested() {
            return Some(Err(Interrupted));
        }
        let mut tty = TTY.lock();
        tty.readable().then(|| Ok(tty.read(buf)))
    })
}

/// The process Ctrl-C kills, if any.
pub fn foreground() -> Option<PID> {
    match FOREGROUND.load(Ordering::SeqCst) {
        0 => None,
        pid => Some(PID(pid)),
    }
}

/// Set the process Ctrl-C kills (`None`: nobody), returning the previous one.
pub fn set_foreground(pid: Option<PID>) -> Option<PID> {
    let raw = pid.map(|pid| pid.0).unwrap_or(0);
    match FOREGROUND.swap(raw, Ordering::SeqCst) {
        0 => None,
        previous => Some(PID(previous)),
    }
}

/// Clear the foreground if it is `pid`. Used once `pid` has been collected,
/// so a later Ctrl-C can't hit an unrelated process that reuses the PID.
pub fn clear_foreground_if(pid: PID) {
    let _ = FOREGROUND.compare_exchange(pid.0, 0, Ordering::SeqCst, Ordering::SeqCst);
}

pub fn mode() -> TtyMode {
    TTY.lock().mode()
}

/// Switch the console's mode and return the previous one.
pub fn set_mode(mode: TtyMode) -> TtyMode {
    let previous = {
        let mut tty = TTY.lock();
        let previous = tty.mode();
        tty.set_mode(mode);
        previous
    };
    // Waiters re-check their condition, so an unnecessary wakeup is
    // harmless, and this keeps readers correct whatever a mode switch did.
    READERS.wake_all();
    previous
}

/// Number of threads currently queued in [`read`] (may include stale
/// entries). For tests.
pub fn waiting_readers() -> usize {
    READERS.waiter_count()
}

/// Write echo bytes to the screen and the serial port, like console output.
/// Allocation-free and IRQ-safe (both printers disable interrupts).
pub fn echo(bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
    crate::serial::write_bytes(bytes);
    crate::vga_buffer::write_bytes(bytes);
}

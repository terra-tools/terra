//! terra patch: a tee on the child→terminal byte stream.
//!
//! Everything a program prints reaches terra as bytes read from the PTY inside
//! `alacritty_terminal`'s event loop, which hands them straight to the parser
//! and keeps no copy. terra wants one (see terra-app's `transcript.rs`: a
//! full-screen program's output exists nowhere else once the screen is
//! cleared), and the event loop is generic over `EventedPty` — so the copy is
//! taken by wrapping the PTY rather than by forking alacritty.
//!
//! The only awkward part is `EventedReadWrite::reader(&mut self) -> &mut
//! Self::Reader`: the wrapper must hand back a reader it *owns*, while the
//! bytes come from a reader owned by the PTY it wraps. [`Tap`] resolves that
//! by holding a pointer to the inner reader, refreshed on every `reader()`
//! call — see the safety note there.

use std::io::{self, Read};
use std::sync::Arc;

use alacritty_terminal::event::{OnResize, WindowSize};
use alacritty_terminal::tty::{ChildEvent, EventedPty, EventedReadWrite};
use polling::{Event, PollMode, Poller};

/// Called with every chunk of bytes the child writes, on the PTY reader
/// thread. Must be cheap: it runs in the read loop, before the parser.
pub type OutputTap = Arc<dyn Fn(&[u8]) + Send + Sync>;

/// A PTY that copies everything read from it to an [`OutputTap`].
///
/// With no tap installed this is a pure delegation — one pointer store and one
/// `Option` check per read — so the wrapper is applied unconditionally and
/// `[tabs] transcript_kb = 0` costs nothing but that.
pub struct TappedPty<P: EventedReadWrite> {
    inner: P,
    tap: Tap<P::Reader>,
}

impl<P: EventedReadWrite> TappedPty<P> {
    pub fn new(inner: P, sink: Option<OutputTap>) -> Self {
        Self {
            inner,
            tap: Tap {
                sink,
                src: std::ptr::null_mut(),
            },
        }
    }
}

/// The reader [`TappedPty`] hands out: reads through to the wrapped PTY's own
/// reader, then shows the bytes to the sink.
pub struct Tap<R> {
    sink: Option<OutputTap>,
    /// The wrapped PTY's reader. Written by every
    /// [`TappedPty::reader`] call, immediately before the caller reads.
    ///
    /// SAFETY: `reader()` takes `&mut self` and returns a borrow of
    /// `self.tap`, so the `TappedPty` — and with it the `inner` this points
    /// into — cannot move or be dropped while that borrow, the only route to
    /// [`Tap::read`], is alive. The pointer is refreshed on every call, so a
    /// move *between* calls is harmless too. `tap` and `inner` are disjoint
    /// fields, so the borrow of one never invalidates a pointer into the
    /// other.
    src: *mut R,
}

// The pointer is into the same struct, which alacritty moves to its PTY
// thread as a unit; nothing else can reach it.
unsafe impl<R: Send> Send for Tap<R> {}

impl<R: Read> Read for Tap<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.src.is_null() {
            // Unreachable: `reader()` is the only way to obtain a `&mut Tap`.
            return Ok(0);
        }
        let read = unsafe { (*self.src).read(buf) }?;
        if read > 0 {
            if let Some(sink) = &self.sink {
                sink(&buf[..read]);
            }
        }
        Ok(read)
    }
}

impl<P: EventedReadWrite> EventedReadWrite for TappedPty<P> {
    type Reader = Tap<P::Reader>;
    type Writer = P::Writer;

    /// # Safety
    ///
    /// Same contract as the wrapped PTY's: the sources must outlive their
    /// registration.
    unsafe fn register(
        &mut self,
        poll: &Arc<Poller>,
        event: Event,
        mode: PollMode,
    ) -> io::Result<()> {
        unsafe { self.inner.register(poll, event, mode) }
    }

    fn reregister(&mut self, poll: &Arc<Poller>, event: Event, mode: PollMode) -> io::Result<()> {
        self.inner.reregister(poll, event, mode)
    }

    fn deregister(&mut self, poll: &Arc<Poller>) -> io::Result<()> {
        self.inner.deregister(poll)
    }

    fn reader(&mut self) -> &mut Self::Reader {
        // The borrow of `inner` ends at the assignment; see the SAFETY note
        // on `Tap::src`.
        self.tap.src = self.inner.reader();
        &mut self.tap
    }

    fn writer(&mut self) -> &mut Self::Writer {
        self.inner.writer()
    }
}

impl<P: EventedPty> EventedPty for TappedPty<P> {
    fn next_child_event(&mut self) -> Option<ChildEvent> {
        self.inner.next_child_event()
    }
}

impl<P: EventedReadWrite + OnResize> OnResize for TappedPty<P> {
    fn on_resize(&mut self, window_size: WindowSize) {
        self.inner.on_resize(window_size)
    }
}

//! Framebuffer capture, for `terra screenshot`.
//!
//! Every other request in `ipc.rs` is answered entirely on the connection
//! thread, out of the UI thread's way, because eframe does not run the app at
//! all while the window is occluded (another Space, minimised, fully covered)
//! and a request that waited for a frame would simply never be answered.
//!
//! A screenshot is the one request that *is* a frame: the pixels only exist
//! because the GPU drew them. So this is a rendezvous rather than a direct
//! execution, and it is the only place in terra where an IPC thread blocks on
//! the UI:
//!
//! 1. the connection thread summons the window (`ipc::screenshot`), takes a
//!    ticket and posts [`egui::ViewportCommand::Screenshot`] carrying it,
//! 2. eframe paints a frame, copies the surface, and reads it back
//!    asynchronously; the reply lands as an [`egui::Event::Screenshot`] in the
//!    raw input of a *later* frame,
//! 3. `App::ui` calls [`Screenshots::deliver`] on every frame, which matches
//!    the ticket and wakes the waiting thread,
//! 4. [`Screenshots::capture`] returns the image, or fails after [`TIMEOUT`].
//!
//! The timeout is what keeps the occluded-window case honest: summoning the
//! window is a request to the window server, not a guarantee, so a capture
//! that cannot happen has to end in a readable error rather than a hung CLI.
//! While a ticket is outstanding `App::ui` also keeps requesting repaints —
//! the readback completes on a later frame, and an idle terra would otherwise
//! park before that frame ever happened.
//!
//! # One window: the root one
//!
//! terra can have several OS windows (a tab torn out becomes a deferred
//! viewport with its own window and its own framebuffer), but `terra
//! screenshot` photographs the **root** window only — the one terra started
//! with. The request carries no window on the wire, so there is nothing to name
//! another one with, and adding that is a protocol change rather than a
//! refinement here.
//!
//! That limitation is enforced at both ends rather than assumed. The command
//! goes to [`egui::ViewportId::ROOT`] explicitly instead of to "the current
//! viewport", and [`Screenshots::deliver`] ignores any `Event::Screenshot`
//! that came from some other viewport. Neither is theoretical: `deliver` runs
//! once per frame from `App::ui`, an extra window that captures itself for its
//! own reasons would otherwise have its image handed to whichever IPC thread
//! happened to be waiting, and a ticket answered with the wrong window's pixels
//! is a screenshot that is quietly, plausibly wrong — the worst kind.

use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// How long a client waits for a frame before being told it will not come.
/// Long enough for the window to come forward from another Space on a busy
/// machine, short enough that a scripted `terra screenshot` fails fast.
pub const TIMEOUT: Duration = Duration::from_secs(2);

/// The ticket travelling in [`egui::UserData`]. A newtype rather than a bare
/// `u64` so a downcast cannot match somebody else's user data by accident.
struct Ticket(u64);

#[derive(Default)]
struct State {
    next: u64,
    /// Tickets posted and not yet answered — also the "keep painting" flag.
    waiting: Vec<u64>,
    /// Answers whose thread has not collected them yet.
    ready: BTreeMap<u64, Arc<egui::ColorImage>>,
}

/// The rendezvous itself: shared by the UI thread and every IPC thread.
#[derive(Default)]
pub struct Screenshots {
    state: Mutex<State>,
    arrived: Condvar,
}

/// Take the lock, ignoring poisoning — the rest of the app does the same, and
/// a panicked capture must not brick every later one.
fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(|err| err.into_inner())
}

impl Screenshots {
    /// Ask for a frame and block until it arrives. **Never call this from the
    /// UI thread**: it waits for the UI thread to make progress.
    ///
    /// Returns the PNG-encoded window, or a message fit to hand straight to
    /// the user.
    pub fn capture(&self, ctx: &egui::Context) -> Result<Vec<u8>, String> {
        self.capture_within(ctx, TIMEOUT)
    }

    /// [`capture`](Self::capture) with a caller-chosen patience. The short
    /// budget exists for the quiet first attempt in `ipc::screenshot`: a
    /// visible window delivers in a frame or two, so a fraction of a second
    /// decides whether summoning is needed at all.
    pub fn capture_within(
        &self,
        ctx: &egui::Context,
        timeout: Duration,
    ) -> Result<Vec<u8>, String> {
        let ticket = {
            let mut state = lock(&self.state);
            state.next += 1;
            let ticket = state.next;
            state.waiting.push(ticket);
            ticket
        };

        // Addressed to the root viewport by name. `send_viewport_cmd` targets
        // the *current* one, which off the UI thread resolves to the root by
        // default — true today, but "by default" is not the same as "on
        // purpose", and this is the line that decides which window the user
        // gets a picture of. See the module docs.
        ctx.send_viewport_cmd_to(
            egui::ViewportId::ROOT,
            egui::ViewportCommand::Screenshot(egui::UserData::new(Ticket(ticket))),
        );
        ctx.request_repaint();

        let deadline = Instant::now() + timeout;
        let mut state = lock(&self.state);
        loop {
            if let Some(image) = state.ready.remove(&ticket) {
                drop(state);
                return encode(&image)
                    .map_err(|err| format!("cannot encode the screenshot: {err}"));
            }
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                state.waiting.retain(|t| *t != ticket);
                state.ready.remove(&ticket);
                return Err(format!(
                    "timed out after {:.1}s waiting for terra to draw a frame — \
                     the window may be minimised, fully covered or on another Space",
                    timeout.as_secs_f32()
                ));
            };
            state = self
                .arrived
                .wait_timeout(state, left)
                .unwrap_or_else(|err| err.into_inner())
                .0;
        }
    }

    /// Hand any screenshots in this frame's input to whoever asked for them.
    /// Called from the UI thread, once per frame.
    ///
    /// Only the root viewport's images are collected: every ticket this type
    /// hands out was posted to the root window, so an image from anywhere else
    /// answers no question of ours (see the module docs).
    pub fn deliver(&self, ctx: &egui::Context) {
        // Collected under `ctx.input` and matched outside it: the closure runs
        // with egui's input lock held, and the state lock must never be taken
        // underneath another one.
        let arrived: Vec<(u64, Arc<egui::ColorImage>)> = ctx.input(|input| {
            input
                .events
                .iter()
                .filter_map(|event| match event {
                    egui::Event::Screenshot {
                        viewport_id,
                        user_data,
                        image,
                    } if *viewport_id == egui::ViewportId::ROOT => {
                        let ticket = user_data.data.as_ref()?.downcast_ref::<Ticket>()?.0;
                        Some((ticket, Arc::clone(image)))
                    }
                    _ => None,
                })
                .collect()
        });
        if arrived.is_empty() {
            return;
        }

        let mut state = lock(&self.state);
        for (ticket, image) in arrived {
            // A ticket that is no longer waiting timed out; its image is the
            // answer to a question nobody is asking any more.
            if let Some(at) = state.waiting.iter().position(|t| *t == ticket) {
                state.waiting.swap_remove(at);
                state.ready.insert(ticket, image);
            }
        }
        drop(state);
        self.arrived.notify_all();
    }

    /// Whether a capture is outstanding. The UI keeps painting while it is —
    /// the GPU readback lands a frame or two after the one it captured, and
    /// terra is otherwise happy to sit idle in between.
    pub fn pending(&self) -> bool {
        !lock(&self.state).waiting.is_empty()
    }
}

/// Encode egui's framebuffer as an 8-bit RGBA PNG.
///
/// [`egui::Color32`] is *premultiplied*, which is not what PNG stores, so the
/// pixels go through `to_srgba_unmultiplied` rather than being reinterpreted.
/// A terra window is opaque, so in practice this is the identity — but a
/// window that ever gains transparency would otherwise silently darken.
fn encode(image: &egui::ColorImage) -> Result<Vec<u8>, png::EncodingError> {
    let [width, height] = image.size;
    let mut rgba = Vec::with_capacity(width * height * 4);
    for pixel in &image.pixels {
        rgba.extend_from_slice(&pixel.to_srgba_unmultiplied());
    }

    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, width as u32, height as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(&rgba)?;
    writer.finish()?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(width: usize, height: usize, color: egui::Color32) -> egui::ColorImage {
        egui::ColorImage::new([width, height], vec![color; width * height])
    }

    #[test]
    fn a_framebuffer_encodes_to_a_png_of_the_same_size() {
        let png = encode(&image(3, 2, egui::Color32::from_rgb(0x1e, 0x1e, 0x1e))).unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");

        let decoder = png::Decoder::new(std::io::Cursor::new(&png));
        let mut reader = decoder.read_info().unwrap();
        let info = reader.info();
        assert_eq!((info.width, info.height), (3, 2));
        assert_eq!(info.color_type, png::ColorType::Rgba);

        let mut buf = vec![0; reader.output_buffer_size().unwrap()];
        let frame = reader.next_frame(&mut buf).unwrap();
        assert_eq!(&buf[..frame.buffer_size()][..4], &[0x1e, 0x1e, 0x1e, 0xff]);
    }

    /// Premultiplied in, straight alpha out — a half-transparent white must
    /// come back white, not grey.
    #[test]
    fn translucent_pixels_are_unmultiplied_rather_than_reinterpreted() {
        let translucent = egui::Color32::from_rgba_unmultiplied(0xff, 0xff, 0xff, 0x80);
        let png = encode(&image(1, 1, translucent)).unwrap();
        let decoder = png::Decoder::new(std::io::Cursor::new(&png));
        let mut reader = decoder.read_info().unwrap();
        let mut buf = vec![0; reader.output_buffer_size().unwrap()];
        reader.next_frame(&mut buf).unwrap();
        assert_eq!(buf[3], 0x80, "alpha must survive");
        assert!(buf[0] >= 0xfe, "got {:#x}, expected ~0xff", buf[0]);
    }

    /// Play the UI thread's half: run one frame whose input carries a
    /// screenshot event from `viewport`, and let `deliver` sort it out.
    fn feed(ctx: &egui::Context, shots: &Screenshots, viewport: egui::ViewportId, ticket: u64) {
        let input = egui::RawInput {
            events: vec![egui::Event::Screenshot {
                viewport_id: viewport,
                user_data: egui::UserData::new(Ticket(ticket)),
                image: Arc::new(image(1, 1, egui::Color32::RED)),
            }],
            ..Default::default()
        };
        // Delivered from *inside* the frame, which is where `App::ui` calls it
        // and the only place `ctx.input` reads this frame's events.
        let inner = ctx.clone();
        let _ = ctx.run_ui(input, |_ui| shots.deliver(&inner));
    }

    /// `terra screenshot` is the root window's. A torn-out window that
    /// captures itself must not have its pixels handed to the IPC thread
    /// waiting on the root's ticket — a screenshot of the wrong window is
    /// wrong in a way nobody would catch by looking at it.
    #[test]
    fn an_image_from_another_window_answers_nobody() {
        let shots = Screenshots::default();
        let ctx = egui::Context::default();
        {
            let mut state = lock(&shots.state);
            state.next = 7;
            state.waiting.push(7);
        }

        let torn_out = egui::ViewportId::from_hash_of("a torn-out terra window");
        assert_ne!(torn_out, egui::ViewportId::ROOT);
        feed(&ctx, &shots, torn_out, 7);
        assert!(shots.pending(), "the ticket is still outstanding");
        assert!(
            lock(&shots.state).ready.is_empty(),
            "another window's framebuffer was collected"
        );

        // The root's own image, same ticket, does answer it.
        feed(&ctx, &shots, egui::ViewportId::ROOT, 7);
        assert!(!shots.pending());
        assert!(lock(&shots.state).ready.contains_key(&7));
    }

    /// The wait must end even when no UI thread ever answers — this is the
    /// occluded window, and a CLI that hangs forever is the bug it prevents.
    #[test]
    fn a_capture_nobody_answers_fails_instead_of_hanging() {
        let shots = Screenshots::default();
        let ctx = egui::Context::default();
        // Not `TIMEOUT` itself: the point is the mechanism, not the wall clock.
        let mut state = lock(&shots.state);
        state.next = 41;
        state.waiting.push(42);
        drop(state);
        assert!(shots.pending());

        let started = Instant::now();
        let err = shots.capture(&ctx).unwrap_err();
        assert!(err.contains("timed out"), "{err}");
        assert!(started.elapsed() >= TIMEOUT);
        assert!(
            !shots.pending(),
            "a timed-out ticket must not keep the UI awake"
        );
    }

    /// The happy path, with the UI thread's half played by the test: an
    /// answered ticket wakes the waiter and yields a PNG.
    #[test]
    fn an_answered_ticket_comes_back_as_a_png() {
        let shots = Arc::new(Screenshots::default());
        let ctx = egui::Context::default();

        let ui = {
            let shots = Arc::clone(&shots);
            std::thread::spawn(move || {
                let deadline = Instant::now() + TIMEOUT;
                loop {
                    let mut state = lock(&shots.state);
                    if let Some(ticket) = state.waiting.pop() {
                        state
                            .ready
                            .insert(ticket, Arc::new(image(2, 2, egui::Color32::RED)));
                        drop(state);
                        shots.arrived.notify_all();
                        return;
                    }
                    drop(state);
                    assert!(Instant::now() < deadline, "no ticket was ever posted");
                    std::thread::sleep(Duration::from_millis(5));
                }
            })
        };

        let png = shots.capture(&ctx).expect("a delivered screenshot");
        ui.join().unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert!(!shots.pending());
    }
}

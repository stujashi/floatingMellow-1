// Floating, always-on-top, transparent, draggable image widget with a
// squash-and-stretch bounce + click sound effect.
//
// Usage:
//   cargo run --release                    -> uses default "image.png" in project root
//   cargo run --release -- path/to/img.png -> uses a specific image
//
// Controls:
//   - Left-click the image         -> plays a bounce animation + click sound
//   - Left-click + drag the image  -> moves the window anywhere on screen
//   - Right-click the image        -> closes the app
//   - Scroll wheel over the image  -> resizes it up/down

use std::io::Cursor;
use std::sync::Arc;

use eframe::egui;
use egui::{ViewportBuilder, ViewportCommand};
use rodio::{DeviceSinkBuilder, MixerDeviceSink, Player};

const DEFAULT_IMAGE_PATH: &str = "image.png";
const CLICK_SOUND_PATH: &str = "assets/click.mp3"; // <- match your actual filename
const CLICK_VOLUME: f32 = 0.4; // <- adjust this: 0.0 (silent) to 1.0+ (louder)

// How many frames we're willing to keep re-asserting the target window
// size for, while waiting for the OS/backend to report back a matching
// size. Needed because on scaled displays (e.g. 125% on a 1366x768
// laptop panel) the reported `pixels_per_point` / inner_rect can take a
// couple of frames to settle after window creation.
const MAX_RESIZE_ATTEMPTS: u8 = 20;
// How close (in logical points) actual vs target size need to be before
// we consider the resize "done".
const RESIZE_EPSILON: f32 = 0.5;

fn main() -> eframe::Result<()> {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| DEFAULT_IMAGE_PATH.to_string());

    let image_bytes = std::fs::read(&path).unwrap_or_else(|e| {
        eprintln!("Failed to read image at '{path}': {e}");
        std::process::exit(1);
    });

    let decoded = image::load_from_memory(&image_bytes)
        .unwrap_or_else(|e| {
            eprintln!("Failed to decode image: {e}");
            std::process::exit(1);
        })
        .to_rgba8();

    let (img_w, img_h) = decoded.dimensions();

    // Cap the initial window size so huge images don't fill the screen.
    let max_dim = 500.0_f32;
    let scale = (max_dim / img_w.max(img_h) as f32).min(1.0);
    let start_size = [img_w as f32 * scale, img_h as f32 * scale];

    // Deliberately request a slightly different size at window creation
    // than the real target size. `with_inner_size` / `InnerSize` work in
    // *logical points*, but the actual render surface is allocated in
    // *physical pixels* (logical x pixels_per_point). On scaled displays
    // (very common on 1366x768 panels, which are often set to 125%
    // scaling) the backend can report a stale/incorrect pixels_per_point
    // on the very first frame, and if our frame-1 resize command asks for
    // the same *logical* size the window was already created at, no real
    // resize event fires and the render surface stays under-allocated --
    // showing up as the image being cropped (e.g. the bottom edge cut
    // off). Booting 1px taller guarantees frame 1 sees a genuine size
    // delta, forcing a proper resize. We then verify + correct against
    // actual reported size for a few frames in `update()` to handle any
    // remaining DPI settling.
    let boot_size = [start_size[0], start_size[1] + 1.0];

    let options = eframe::NativeOptions {
        viewport: ViewportBuilder::default()
            .with_inner_size(boot_size)
            .with_min_inner_size([32.0, 32.0])
            .with_decorations(false) // no title bar / borders
            .with_transparent(true) // let the desktop show through
            .with_always_on_top() // stay above other windows
            .with_resizable(false) // we resize manually via scroll
            .with_taskbar(false), // optional: hide from taskbar
        ..Default::default()
    };

    eframe::run_native(
        "Floating Image",
        options,
        Box::new(move |cc| {
            Ok(Box::new(FloatingImageApp::new(
                cc, decoded, img_w, img_h, start_size,
            )))
        }),
    )
}

struct FloatingImageApp {
    texture: egui::TextureHandle,
    aspect: f32,
    current_size: egui::Vec2,

    // DPI-aware startup sizing: we keep re-asserting `current_size` until
    // the OS reports back an inner_rect that actually matches it, or we
    // give up after `MAX_RESIZE_ATTEMPTS` frames.
    resize_attempts: u8,

    // Bounce animation state.
    bounce_start: Option<f32>,

    // Audio playback. `_device_sink` must stay alive for the app's
    // lifetime -- dropping it stops all playback and disposes the OS
    // output device.
    _device_sink: MixerDeviceSink,
    click_sound_bytes: Option<Arc<Vec<u8>>>,
}

impl FloatingImageApp {
    fn new(
        cc: &eframe::CreationContext<'_>,
        rgba: image::RgbaImage,
        img_w: u32,
        img_h: u32,
        start_size: [f32; 2],
    ) -> Self {
        let color_image =
            egui::ColorImage::from_rgba_unmultiplied([img_w as usize, img_h as usize], &rgba);
        let texture =
            cc.egui_ctx
                .load_texture("floating_image", color_image, Default::default());

        // Open the default audio output device.
        let device_sink = DeviceSinkBuilder::open_default_sink()
            .expect("failed to open default audio output device");

        // Load click sound bytes once; if missing, just skip audio
        // (app still runs fine, just silently).
        let click_sound_bytes = match std::fs::read(CLICK_SOUND_PATH) {
            Ok(bytes) => Some(Arc::new(bytes)),
            Err(e) => {
                eprintln!(
                    "Warning: couldn't load click sound at '{CLICK_SOUND_PATH}': {e} (continuing without sound)"
                );
                None
            }
        };

        Self {
            texture,
            aspect: img_w as f32 / img_h as f32,
            current_size: egui::vec2(start_size[0], start_size[1]),
            resize_attempts: 0,
            bounce_start: None,
            _device_sink: device_sink,
            click_sound_bytes,
        }
    }

    fn play_click_sound(&self) {
        let Some(bytes) = self.click_sound_bytes.clone() else {
            return;
        };

        let player = Player::connect_new(&self._device_sink.mixer());

        match rodio::Decoder::try_from(Cursor::new((*bytes).clone())) {
            Ok(source) => {
                player.append(source);
                player.set_volume(CLICK_VOLUME);
                player.detach();
            }
            Err(e) => eprintln!("Failed to decode click sound: {e}"),
        }
    }

    /// Re-assert `current_size` on the OS window if the actual reported
    /// size doesn't match yet (DPI/scale-factor settling), up to a frame
    /// budget. Returns true if it requested a repaint (i.e. still settling).
    fn enforce_target_size(&mut self, ctx: &egui::Context) -> bool {
        if self.resize_attempts >= MAX_RESIZE_ATTEMPTS {
            return false;
        }

        let actual_logical = ctx.input(|i| {
            i.viewport()
                .inner_rect
                .map(|r| r.size())
                // Fall back to outer_rect if inner_rect isn't reported
                // yet on this backend/frame.
                .or_else(|| i.viewport().outer_rect.map(|r| r.size()))
        });

        let matches = actual_logical
            .map(|a| (a - self.current_size).length() < RESIZE_EPSILON)
            .unwrap_or(false);

        if matches {
            self.resize_attempts = MAX_RESIZE_ATTEMPTS;
            return false;
        }

        self.resize_attempts += 1;
        // InnerSize expects logical points (already DPI-independent), so
        // no manual pixels_per_point conversion is needed here -- we just
        // keep re-sending it each frame so that if the backend's reported
        // scale factor changes between frame 1 and frame N (as the OS
        // finishes reporting true display scaling), we correct for it.
        ctx.send_viewport_cmd(ViewportCommand::InnerSize(self.current_size));
        ctx.request_repaint();
        true
    }
}

impl eframe::App for FloatingImageApp {
    // Returning a fully transparent clear color is what makes the
    // window background see-through instead of solid.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        // DEBUG: background set to 80% transparent (20% opaque black) so the
        // actual window bounds are visible while debugging layout/sizing.
        egui::Rgba::TRANSPARENT.to_array()
        //[0.0, 0.0, 0.0, 0.2]
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Keep correcting window size for the first several frames to
        // handle both the "no resize event fired" issue and OS display
        // scaling (DPI) settling -- common on scaled 1366x768 panels.
        self.enforce_target_size(ctx);

        let now = ctx.input(|i| i.time) as f32;

        // --- Squash & stretch bounce animation (click-triggered) ---
        let bounce_duration = 0.4; // seconds, total time for the bounce to settle
        let bounce_strength = 0.18; // how extreme the squash is

        let (squash_x, squash_y) = if let Some(start) = self.bounce_start {
            let elapsed = now - start;
            if elapsed >= bounce_duration {
                self.bounce_start = None;
                (1.0, 1.0)
            } else {
                // Damped oscillation: sine wave that decays to 0 over the duration.
                let progress = elapsed / bounce_duration;
                let decay = 1.0 - progress;
                let wave = (progress * std::f32::consts::PI * 3.0).sin() * decay;
                (1.0 + bounce_strength * wave, 1.0 - bounce_strength * wave)
            }
        } else {
            (1.0, 1.0)
        };

        egui::CentralPanel::default()
            .frame(egui::Frame::none())
            .show(ctx, |ui| {
                let avail = ui.available_size();

                // IMPORTANT: fit the image into `avail` while preserving its
                // true aspect ratio ("contain" fit) -- never stretch it to
                // match avail's shape directly. Stretching to fill avail
                // (a previous attempt) caused visible distortion/squishing
                // whenever the real OS window size didn't exactly match
                // what we'd requested (e.g. mid-scroll-resize), which reads
                // to the eye as content getting "cropped" or squeezed even
                // though nothing is actually being clipped.
                let fit_size = if avail.x / avail.y > self.aspect {
                    // avail is proportionally wider than the image -> the
                    // image's height is the limiting dimension.
                    egui::vec2(avail.y * self.aspect, avail.y)
                } else {
                    // avail is proportionally taller/narrower -> the
                    // image's width is the limiting dimension.
                    egui::vec2(avail.x, avail.x / self.aspect)
                };
                let animated_size = egui::vec2(fit_size.x * squash_x, fit_size.y * squash_y);

                // Center the (possibly smaller-than-window) animated image
                // inside the available space so it squashes symmetrically
                // instead of drifting toward a corner.
                let offset = (avail - animated_size) * 0.5;
                let rect = egui::Rect::from_min_size(ui.min_rect().min + offset, animated_size);

                ui.allocate_new_ui(egui::UiBuilder::new().max_rect(rect), |ui| {
                    let response = ui.add(
                        egui::Image::new((self.texture.id(), animated_size))
                            .sense(egui::Sense::click_and_drag()),
                    );
                    // Show a pointing-hand cursor whenever hovering the image.
                    ctx.set_cursor_icon(if response.hovered() {
                        egui::CursorIcon::PointingHand
                    } else {
                        egui::CursorIcon::Default
                    });

                    // Left-click (press+release, no drag) -> bounce + sound.
                    if response.clicked() {
                        self.bounce_start = Some(now);
                        self.play_click_sound();
                    }

                    // Drag anywhere on the image to move the window.
                    if response.drag_started() {
                        ctx.send_viewport_cmd(ViewportCommand::StartDrag);
                    }

                    // Right-click to quit.
                    if response.secondary_clicked() {
                        ctx.send_viewport_cmd(ViewportCommand::Close);
                    }

                    // Scroll to resize, keeping aspect ratio.
                    let scroll = ui.input(|i| i.smooth_scroll_delta.y);
                    if scroll != 0.0 {
                        let factor = 1.0 + scroll * 0.001;
                        let new_w = (self.current_size.x * factor).clamp(32.0, 2000.0);
                        self.current_size = egui::vec2(new_w, new_w / self.aspect);
                        ctx.send_viewport_cmd(ViewportCommand::InnerSize(self.current_size));
                        // A manual resize changes the target again, so give
                        // the settling logic a fresh budget of frames to
                        // re-verify against the new size.
                        self.resize_attempts = 0;
                    }
                });
            });

        // Repaint continuously while a bounce is playing or while we're
        // still settling the window size against the OS.
        if self.bounce_start.is_some() || self.resize_attempts < MAX_RESIZE_ATTEMPTS {
            ctx.request_repaint();
        }
    }
}
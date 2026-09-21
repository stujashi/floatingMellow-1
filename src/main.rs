// Floating, always-on-top, transparent, draggable image widget with a
// squash-and-stretch bounce + click sound effect.
//
// Usage:
//   cargo run --release                    -> uses default "image.png" in project root
//   cargo run --release -- path/to/img.png -> uses a specific image

use std::io::Cursor;
use std::sync::Arc;

use eframe::egui;
use egui::{ViewportBuilder, ViewportCommand};
use rodio::{DeviceSinkBuilder, MixerDeviceSink, Player};

const DEFAULT_IMAGE_PATH: &str = "image.png";
const CLICK_SOUND_PATH: &str = "assets/click.mp3";
const CLICK_VOLUME: f32 = 0.4;
const MAX_DIM: f32 = 400.0;
// We make the invisible OS window 25% larger than the image.
// This prevents cropping during fast zooming (OS lag) and bounce stretching.
const WINDOW_BUFFER: f32 = 1.25; 

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

    let scale = (MAX_DIM / img_w.max(img_h) as f32).min(1.0);
    
    // Apply the 25% buffer to the initial window size
    let start_size = [
        (img_w as f32 * scale) * WINDOW_BUFFER, 
        (img_h as f32 * scale) * WINDOW_BUFFER
    ];

    let options = eframe::NativeOptions {
        viewport: ViewportBuilder::default()
            .with_inner_size(start_size)
            .with_min_inner_size([40.0, 40.0])
            .with_decorations(false)
            .with_transparent(true) 
            .with_always_on_top()   
            .with_resizable(false)  
            .with_taskbar(false),   
        ..Default::default()
    };

    eframe::run_native(
        "Floating Image",
        options,
        Box::new(move |cc| Ok(Box::new(FloatingImageApp::new(cc, decoded, img_w, img_h)))),
    )
}

struct FloatingImageApp {
    texture: egui::TextureHandle,
    aspect: f32,
    current_size: egui::Vec2,
    initialized: bool,

    bounce_start: Option<f32>,

    _device_sink: MixerDeviceSink,
    click_sound_bytes: Option<Arc<Vec<u8>>>,
}

impl FloatingImageApp {
    fn new(
        cc: &eframe::CreationContext<'_>,
        rgba: image::RgbaImage,
        img_w: u32,
        img_h: u32,
    ) -> Self {
        let color_image =
            egui::ColorImage::from_rgba_unmultiplied([img_w as usize, img_h as usize], &rgba);
        let texture =
            cc.egui_ctx
                .load_texture("floating_image", color_image, Default::default());

        let scale = (MAX_DIM / img_w.max(img_h) as f32).min(1.0);

        let device_sink = DeviceSinkBuilder::open_default_sink()
            .expect("failed to open default audio output device");

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
            current_size: egui::vec2(img_w as f32 * scale, img_h as f32 * scale),
            initialized: false,
            bounce_start: None,
            _device_sink: device_sink,
            click_sound_bytes,
        }
    }

    fn play_click_sound(&self) {
        let Some(bytes) = self.click_sound_bytes.clone() else { return };
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
}

impl eframe::App for FloatingImageApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        egui::Rgba::TRANSPARENT.to_array()
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if !self.initialized {
            self.initialized = true;
            // Tell the OS window to be 25% larger than the image
            ctx.send_viewport_cmd(ViewportCommand::InnerSize(self.current_size * WINDOW_BUFFER));
            ctx.request_repaint();
        }

        let now = ctx.input(|i| i.time) as f32;
        let bounce_duration = 0.4;
        let bounce_strength = 0.18; // Max stretch is +18%. Our 25% window buffer absorbs this!

        let (squash_x, squash_y) = if let Some(start) = self.bounce_start {
            let elapsed = now - start;
            if elapsed >= bounce_duration {
                self.bounce_start = None;
                (1.0, 1.0)
            } else {
                let progress = elapsed / bounce_duration;
                let decay = 1.0 - progress;
                let wave = (progress * std::f32::consts::PI * 3.0).sin() * decay;
                (1.0 + bounce_strength * wave, 1.0 - bounce_strength * wave)
            }
        } else {
            (1.0, 1.0)
        };

        let animated_size = egui::vec2(
            self.current_size.x * squash_x,
            self.current_size.y * squash_y,
        );

        egui::CentralPanel::default()
            .frame(egui::Frame::none())
            .show(ctx, |ui| {
                let avail = ui.available_size();
                // This offset now naturally centers the image inside our 25% larger OS window
                let offset = (avail - animated_size) * 0.5;
                let rect = egui::Rect::from_min_size(ui.min_rect().min + offset, animated_size);

                ui.allocate_new_ui(egui::UiBuilder::new().max_rect(rect), |ui| {
                    let response = ui.add(
                        egui::Image::new((self.texture.id(), animated_size))
                            .sense(egui::Sense::click_and_drag()),
                    );

                    ctx.set_cursor_icon(if response.hovered() {
                        egui::CursorIcon::PointingHand
                    } else {
                        egui::CursorIcon::Default
                    });

                    if response.clicked() {
                        self.bounce_start = Some(now);
                        self.play_click_sound();
                    }

                    if response.drag_started() {
                        ctx.send_viewport_cmd(ViewportCommand::StartDrag);
                    }

                    if response.secondary_clicked() {
                        ctx.send_viewport_cmd(ViewportCommand::Close);
                    }

                    let scroll = ui.input(|i| i.smooth_scroll_delta.y);
                    if scroll != 0.0 {
                        // Keep image size tracking the scroll
                        let factor = 1.0 + scroll * 0.001;
                        let new_w = (self.current_size.x * factor).clamp(32.0, 2000.0);
                        self.current_size = egui::vec2(new_w, new_w / self.aspect);
                        
                        // Scale the invisible OS window to maintain the 25% buffer
                        ctx.send_viewport_cmd(ViewportCommand::InnerSize(self.current_size * WINDOW_BUFFER));
                    }
                });
            });

        if self.bounce_start.is_some() {
            ctx.request_repaint();
        }
    }
}
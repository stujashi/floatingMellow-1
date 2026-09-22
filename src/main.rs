#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")] // Hides the console window on release builds

use std::io::Cursor;
use std::sync::Arc;

use eframe::egui;
use egui::{ViewportBuilder, ViewportCommand};
use rodio::{DeviceSinkBuilder, MixerDeviceSink, Player};

// 1. EMBED THE ASSETS AT COMPILE TIME
const EMBEDDED_IMAGE: &[u8] = include_bytes!("../image.png"); 
const EMBEDDED_SOUND: &[u8] = include_bytes!("../assets/click.mp3");

const CLICK_VOLUME: f32 = 0.4;
const MAX_DIM: f32 = 400.0;
const WINDOW_BUFFER: f32 = 1.25; 

fn main() -> eframe::Result<()> {
    let image_bytes = if let Some(path) = std::env::args().nth(1) {
        std::fs::read(&path).unwrap_or_else(|e| {
            eprintln!("Failed to read image at '{path}': {e}");
            std::process::exit(1);
        })
    } else {
        EMBEDDED_IMAGE.to_vec()
    };

    let decoded = image::load_from_memory(&image_bytes)
        .unwrap_or_else(|e| {
            eprintln!("Failed to decode image: {e}");
            std::process::exit(1);
        })
        .to_rgba8();

    let (img_w, img_h) = decoded.dimensions();

    let icon_data = egui::IconData {
        rgba: decoded.clone().into_raw(),
        width: img_w,
        height: img_h,
    };

    let scale = (MAX_DIM / img_w.max(img_h) as f32).min(1.0);
    
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
            .with_taskbar(false)
            .with_icon(std::sync::Arc::new(icon_data)), 
        ..Default::default()
    };
    
    eframe::run_native(
        "Floating Mellow",
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
    text_spawn_time: Option<f32>, 
    
    // NEW: We remember exactly where on the image you clicked!
    click_pivot: Option<egui::Vec2>, 

    _device_sink: MixerDeviceSink,
    click_sound_bytes: Arc<Vec<u8>>, 
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

        Self {
            texture,
            aspect: img_w as f32 / img_h as f32,
            current_size: egui::vec2(img_w as f32 * scale, img_h as f32 * scale),
            initialized: false,
            bounce_start: None,
            text_spawn_time: None,
            click_pivot: None, // Starts empty
            _device_sink: device_sink,
            click_sound_bytes: Arc::new(EMBEDDED_SOUND.to_vec()),
        }
    }

    fn play_click_sound(&self) {
        let player = Player::connect_new(&self._device_sink.mixer());

        match rodio::Decoder::try_from(Cursor::new((*self.click_sound_bytes).clone())) {
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
            ctx.send_viewport_cmd(ViewportCommand::InnerSize(self.current_size * WINDOW_BUFFER));
            ctx.request_repaint();
        }

        let now = ctx.input(|i| i.time) as f32;
        
        let bounce_duration = 0.4;
        let bounce_strength = 0.18; 

        let (squash_x, squash_y) = if let Some(start) = self.bounce_start {
            let elapsed = now - start;
            if elapsed >= bounce_duration {
                self.bounce_start = None;
                (1.0, 1.0)
            } else {
                let progress = elapsed / bounce_duration;
                let decay = 1.0 - progress;
                let wave = (progress * std::f32::consts::PI * 3.0).sin() * decay;
                (1.0 + bounce_strength * wave, 1.0 + bounce_strength * wave)
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
                
                // NEW CALCULATIONS: Keep the image anchored to where you clicked! ⚓
                let pivot = self.click_pivot.unwrap_or(egui::vec2(0.5, 0.5));
                let base_offset = (avail - self.current_size) * 0.5;
                let base_rect_min = ui.min_rect().min + base_offset;
                
                let pivot_screen_x = base_rect_min.x + self.current_size.x * pivot.x;
                let pivot_screen_y = base_rect_min.y + self.current_size.y * pivot.y;

                let new_min_x = pivot_screen_x - animated_size.x * pivot.x;
                let new_min_y = pivot_screen_y - animated_size.y * pivot.y;
                let rect = egui::Rect::from_min_size(egui::pos2(new_min_x, new_min_y), animated_size);

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
                        // Figure out exactly where the mouse was when it clicked
                        if let Some(pos) = response.interact_pointer_pos() {
                            let r = response.rect;
                            let px = (pos.x - r.min.x) / r.width();
                            let py = (pos.y - r.min.y) / r.height();
                            self.click_pivot = Some(egui::vec2(px.clamp(0.0, 1.0), py.clamp(0.0, 1.0)));
                        } else {
                            self.click_pivot = Some(egui::vec2(0.5, 0.5)); // default to center
                        }

                        self.bounce_start = Some(now);
                        self.text_spawn_time = Some(now);
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
                        let factor = 1.0 + scroll * 0.001;
                        let new_w = (self.current_size.x * factor).clamp(32.0, 2000.0);
                        self.current_size = egui::vec2(new_w, new_w / self.aspect);
                        
                        ctx.send_viewport_cmd(ViewportCommand::InnerSize(self.current_size * WINDOW_BUFFER));
                    }
                });

                // --- Floating Text Logic ---
                let text_duration = 0.5;
                if let Some(text_start) = self.text_spawn_time {
                    let elapsed = now - text_start;
                    if elapsed >= text_duration {
                        self.text_spawn_time = None;
                    } else {
                        let progress = elapsed / text_duration; 
                        let alpha = 1.0 - progress; 
                        let float_offset = progress * 40.0; 

                        // Make the text start exactly where you clicked! 🖱️
                        let center_x = pivot_screen_x;
                        let start_y = pivot_screen_y; 
                        let center_y = start_y - float_offset;

                        let text = "Meow!";
                        let font_size = (self.current_size.x * 0.15).clamp(16.0, 72.0); 
                        let font_id = egui::FontId::proportional(font_size);

                        ui.painter().text(
                            egui::pos2(center_x + 2.0, center_y + 2.0),
                            egui::Align2::CENTER_CENTER,
                            text,
                            font_id.clone(),
                            egui::Color32::from_rgba_unmultiplied(0, 0, 0, (255.0 * alpha) as u8),
                        );

                        ui.painter().text(
                            egui::pos2(center_x, center_y),
                            egui::Align2::CENTER_CENTER,
                            text,
                            font_id,
                            egui::Color32::from_rgba_unmultiplied(255, 165, 0, (255.0 * alpha) as u8),
                        );
                    }
                }
            });

        if self.bounce_start.is_some() || self.text_spawn_time.is_some() {
            ctx.request_repaint();
        }
    }
}
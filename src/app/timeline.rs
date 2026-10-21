//! Time slider (frame ticks, playhead, playback buttons) and range slider.
//! Red ticks mark the keys of the lead object's playing clip.

use egui::{Color32, RichText, Sense, Ui, Vec2};

use super::icons::{self, Icon};
use super::{ClideApp, theme};

const CONTROLS_W: f32 = 250.0;

impl ClideApp {
    pub(super) fn tick_playback(&mut self, ctx: &egui::Context) {
        let t = &mut self.timeline;
        let Some(dir) = t.playing else { return };
        let dt = ctx.input(|i| i.stable_dt).min(0.1);
        t.frame += dir * dt * t.fps;
        // Loop inside the playback range, like Maya's default "Looping: continuous".
        let len = (t.play_end - t.play_start + 1.0).max(1.0);
        if t.frame > t.play_end + 0.999 {
            t.frame -= len;
        } else if t.frame < t.play_start {
            t.frame += len;
        }
        ctx.request_repaint();
    }

    pub(super) fn time_slider(&mut self, ui: &mut Ui) {
        let keys = self.lead_key_frames();
        ui.horizontal(|ui| {
            let width = (ui.available_width() - CONTROLS_W).max(100.0);
            let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 30.0), Sense::click_and_drag());
            let t = &mut self.timeline;
            let painter = ui.painter_at(rect);
            painter.rect_filled(rect, 0.0, theme::BUTTON);

            let span = (t.play_end - t.play_start).max(1.0);
            let pad = 8.0;
            let px_per_frame = (rect.width() - 2.0 * pad) / span;
            let x_of = |f: f32| rect.left() + pad + (f - t.play_start) * px_per_frame;
            let step = [1, 2, 5, 10, 20, 50, 100, 200, 500]
                .into_iter()
                .find(|s| *s as f32 * px_per_frame >= 36.0)
                .unwrap_or(1000);
            let first = t.play_start.ceil() as i32;
            for f in first..=t.play_end.floor() as i32 {
                let x = x_of(f as f32);
                let major = f % step == 0;
                if !major && px_per_frame < 4.0 {
                    continue;
                }
                let h = if major { 12.0 } else { 5.0 };
                painter.line_segment([egui::pos2(x, rect.bottom() - h), egui::pos2(x, rect.bottom())], (1.0, Color32::from_gray(40)));
                if major {
                    painter.text(egui::pos2(x + 2.0, rect.top() + 2.0), egui::Align2::LEFT_TOP, f.to_string(), egui::FontId::proportional(10.0), Color32::from_gray(30));
                }
            }

            for &k in keys.iter().filter(|k| (t.play_start..=t.play_end).contains(*k)) {
                let x = x_of(k);
                painter.line_segment([egui::pos2(x, rect.top() + 14.0), egui::pos2(x, rect.bottom())], (2.0, Color32::from_rgb(200, 30, 30)));
            }

            // Playhead: a translucent block one frame wide with the frame number.
            let current = t.frame.floor();
            let x = x_of(current);
            let block = egui::Rect::from_min_max(egui::pos2(x - 1.0, rect.top()), egui::pos2(x + px_per_frame.max(2.0), rect.bottom()));
            painter.rect_filled(block, 0.0, Color32::from_rgba_unmultiplied(200, 200, 200, 140));
            painter.text(egui::pos2(block.right() + 3.0, rect.top() + 14.0), egui::Align2::LEFT_TOP, format!("{}", current as i32), egui::FontId::proportional(11.0), Color32::from_rgb(200, 30, 30));

            if (response.dragged() || response.clicked())
                && let Some(p) = response.interact_pointer_pos()
            {
                t.frame = (t.play_start + (p.x - rect.left() - pad) / px_per_frame).round().clamp(t.play_start, t.play_end);
            }
            if response.hovered() {
                self.help = "Time Slider: click or drag to change the current frame".into();
            }

            // Current frame field and playback buttons.
            ui.add(egui::DragValue::new(&mut self.timeline.frame).speed(0.2).range(self.timeline.anim_start..=self.timeline.anim_end).max_decimals(0));
            let s = 20.0;
            let t = &mut self.timeline;
            let playing_fwd = t.playing == Some(1.0);
            let playing_back = t.playing == Some(-1.0);
            if icons::button(ui, Icon::ToStart, s, false, true, "Go to start of playback range").clicked() {
                t.frame = t.play_start;
            }
            if icons::button(ui, Icon::StepBack, s, false, true, "Step back one frame").clicked() {
                t.frame = (t.frame.floor() - 1.0).max(t.play_start);
            }
            if icons::button(ui, if playing_back { Icon::Pause } else { Icon::PlayBack }, s, playing_back, true, "Play backwards").clicked() {
                t.playing = if playing_back { None } else { Some(-1.0) };
            }
            if icons::button(ui, if playing_fwd { Icon::Pause } else { Icon::Play }, s, playing_fwd, true, "Play forwards (Alt+V)").clicked() {
                t.playing = if playing_fwd { None } else { Some(1.0) };
            }
            if icons::button(ui, Icon::StepFwd, s, false, true, "Step forward one frame").clicked() {
                t.frame = (t.frame.floor() + 1.0).min(t.play_end);
            }
            if icons::button(ui, Icon::ToEnd, s, false, true, "Go to end of playback range").clicked() {
                t.frame = t.play_end;
            }
        });
    }

    pub(super) fn range_slider(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            let t = &mut self.timeline;
            let field = |ui: &mut Ui, v: &mut f32| {
                ui.add_sized([44.0, 18.0], egui::DragValue::new(v).speed(0.5).max_decimals(0));
            };
            field(ui, &mut t.anim_start);
            field(ui, &mut t.play_start);

            let width = (ui.available_width() - 470.0).max(80.0);
            let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 18.0), Sense::hover());
            let painter = ui.painter_at(rect);
            painter.rect_filled(rect, 2.0, theme::FIELD);
            let span = (t.anim_end - t.anim_start).max(1.0);
            let x_of = |f: f32| rect.left() + (f - t.anim_start) / span * rect.width();
            let bar = egui::Rect::from_min_max(egui::pos2(x_of(t.play_start), rect.top() + 2.0), egui::pos2(x_of(t.play_end), rect.bottom() - 2.0));
            painter.rect_filled(bar, 2.0, theme::BUTTON);
            let small = egui::FontId::proportional(10.0);
            painter.text(bar.left_center() + Vec2::new(4.0, 0.0), egui::Align2::LEFT_CENTER, format!("{}", t.play_start as i32), small.clone(), theme::TEXT);
            painter.text(bar.right_center() - Vec2::new(4.0, 0.0), egui::Align2::RIGHT_CENTER, format!("{}", t.play_end as i32), small, theme::TEXT);

            field(ui, &mut t.play_end);
            field(ui, &mut t.anim_end);
            t.anim_start = t.anim_start.min(t.anim_end - 1.0);
            t.play_start = t.play_start.clamp(t.anim_start, t.anim_end - 1.0);
            t.play_end = t.play_end.clamp(t.play_start + 1.0, t.anim_end);
            t.frame = t.frame.clamp(t.anim_start, t.anim_end);

            ui.separator();
            egui::ComboBox::from_id_salt("char_set").selected_text("No Character Set").width(120.0).show_ui(ui, |ui| {
                let _ = ui.selectable_label(true, "No Character Set");
            });
            egui::ComboBox::from_id_salt("anim_layer").selected_text("No Anim Layer").width(110.0).show_ui(ui, |ui| {
                let _ = ui.selectable_label(true, "No Anim Layer");
            });
            egui::ComboBox::from_id_salt("fps")
                .selected_text(format!("{} fps", t.fps as i32))
                .width(70.0)
                .show_ui(ui, |ui| {
                    for fps in [24.0, 25.0, 30.0, 48.0, 60.0] {
                        ui.selectable_value(&mut t.fps, fps, RichText::new(format!("{fps} fps")));
                    }
                });
        });
    }
}

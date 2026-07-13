use eframe::egui;
use egui_plot::{Line, Plot, PlotPoints};
use std::sync::{Arc, Mutex};

use crate::model::{hourly_totals_for, totals_for, Source, Stats};

pub struct App {
    snapshot: Arc<Mutex<Arc<Stats>>>,
    selected_model: Option<String>,
    selected_source: Option<Source>,
}

impl App {
    pub fn new(snapshot: Arc<Mutex<Arc<Stats>>>) -> Self {
        App {
            snapshot,
            selected_model: None,
            selected_source: None,
        }
    }

    fn source_label(source: Option<Source>) -> &'static str {
        match source {
            None => "All",
            Some(Source::Claude) => "Claude Code",
            Some(Source::Codex) => "Codex",
        }
    }

    fn source_tag(source: Source) -> &'static str {
        match source {
            Source::Claude => "CC",
            Source::Codex => "CX",
        }
    }
}

/// Thousand-separated integer, e.g. `1234567` -> `"1,234,567"`.
fn format_int(n: u64) -> String {
    let digits = n.to_string();
    let bytes = digits.as_bytes();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(*b as char);
    }
    out
}

fn stat_row(ui: &mut egui::Ui, label: &str, value: String, emphasize: bool) {
    if emphasize {
        ui.strong(label);
    } else {
        ui.label(label);
    }
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        if emphasize {
            ui.strong(value);
        } else {
            ui.monospace(value);
        }
    });
    ui.end_row();
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_secs(1));
        ui.spacing_mut().item_spacing.y = 4.0;
        let stats = self.snapshot.lock().unwrap().clone();

        egui::ComboBox::from_id_salt("model_filter")
            .width(ui.available_width())
            .selected_text(self.selected_model.as_deref().unwrap_or("Model: All"))
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut self.selected_model, None, "All");
                let mut models: Vec<String> = stats
                    .model_keys()
                    .into_iter()
                    .map(|(_, model)| model)
                    .collect();
                models.dedup();
                for model in models {
                    let label = model.clone();
                    ui.selectable_value(&mut self.selected_model, Some(model), label);
                }
            });

        egui::ComboBox::from_id_salt("source_filter")
            .width(ui.available_width())
            .selected_text(Self::source_label(self.selected_source))
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut self.selected_source, None, "All");
                ui.selectable_value(&mut self.selected_source, Some(Source::Claude), "Claude Code");
                ui.selectable_value(&mut self.selected_source, Some(Source::Codex), "Codex");
            });

        ui.add_space(4.0);

        let totals = totals_for(&stats, self.selected_model.as_deref(), self.selected_source);
        egui::Frame::group(ui.style()).show(ui, |ui| {
            egui::Grid::new("stat_grid")
                .num_columns(2)
                .min_col_width(ui.available_width() * 0.5)
                .show(ui, |ui| {
                    stat_row(ui, "Input", format_int(totals.input), false);
                    stat_row(ui, "Output", format_int(totals.output), false);
                    stat_row(ui, "Cache read", format_int(totals.cache_read), false);
                    stat_row(ui, "Cache write", format_int(totals.cache_write), false);
                    stat_row(ui, "Total", format_int(totals.total_tokens()), true);
                    stat_row(ui, "Est. cost", format!("${:.2}", totals.cost), true);
                });
        });

        ui.add_space(6.0);
        ui.label(egui::RichText::new("Live").strong().small());
        egui::Frame::group(ui.style()).show(ui, |ui| {
            egui::ScrollArea::vertical()
                .max_height(110.0)
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    if stats.feed.is_empty() {
                        ui.weak("no activity yet");
                    }
                    for entry in stats.feed.iter().rev() {
                        ui.horizontal(|ui| {
                            let local_time = entry.ts.with_timezone(&chrono::Local);
                            ui.monospace(local_time.format("%H:%M:%S").to_string());
                            ui.label(Self::source_tag(entry.source));
                            ui.label(entry.model.as_str());
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.monospace(format!(
                                    "{} tok  ${:.4}",
                                    format_int(entry.tokens),
                                    entry.cost
                                ));
                            });
                        });
                    }
                });
        });

        ui.add_space(6.0);
        ui.label(egui::RichText::new("Tokens / hour (today)").strong().small());
        let hours = hourly_totals_for(&stats, self.selected_model.as_deref(), self.selected_source);
        let points: PlotPoints<'_> = (0..24)
            .map(|hour| [hour as f64, hours[hour].total_tokens() as f64])
            .collect();
        Plot::new("hourly_chart")
            .height(90.0)
            .show_axes([true, false])
            .show(ui, |plot_ui| {
                plot_ui.line(Line::new("Tokens", points));
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_int_inserts_commas_every_three_digits() {
        assert_eq!(format_int(0), "0");
        assert_eq!(format_int(7), "7");
        assert_eq!(format_int(999), "999");
        assert_eq!(format_int(1000), "1,000");
        assert_eq!(format_int(1_234_567), "1,234,567");
        assert_eq!(format_int(100), "100");
    }
}

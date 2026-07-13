use chrono::Timelike;
use eframe::egui::{self, Color32, Stroke};
use egui_plot::{Bar, BarChart, Legend, Plot};
use std::sync::{Arc, Mutex};

use crate::model::{Source, Stats, hourly_totals_for, totals_for};

pub struct App {
    snapshot: Arc<Mutex<Arc<Stats>>>,
    visuals_configured: bool,
}

const CODEX: Color32 = Color32::from_rgb(76, 159, 255);
const CLAUDE: Color32 = Color32::from_rgb(255, 163, 72);
const SURFACE: Color32 = Color32::from_rgb(20, 27, 36);
const SURFACE_RAISED: Color32 = Color32::from_rgb(27, 36, 47);
const BORDER: Color32 = Color32::from_rgb(53, 67, 82);
const MUTED: Color32 = Color32::from_rgb(145, 160, 176);
// Distinct from CODEX/CLAUDE so the top-row token chips never get mistaken
// for a source color: green=in, rose=out, violet=cache.
const IN_COLOR: Color32 = Color32::from_rgb(74, 222, 128);
const OUT_COLOR: Color32 = Color32::from_rgb(248, 113, 113);
const CACHE_COLOR: Color32 = Color32::from_rgb(192, 132, 252);

impl App {
    pub fn new(snapshot: Arc<Mutex<Arc<Stats>>>) -> Self {
        App {
            snapshot,
            visuals_configured: false,
        }
    }

    fn source_tag(source: Source) -> &'static str {
        match source {
            Source::Claude => "CC",
            Source::Codex => "CX",
        }
    }

    fn source_color(source: Source) -> Color32 {
        match source {
            Source::Claude => CLAUDE,
            Source::Codex => CODEX,
        }
    }

    fn configure_visuals(&mut self, ctx: &egui::Context) {
        if self.visuals_configured {
            return;
        }
        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = Color32::from_rgb(14, 19, 27);
        visuals.window_fill = SURFACE;
        visuals.faint_bg_color = SURFACE_RAISED;
        visuals.extreme_bg_color = Color32::from_rgb(10, 15, 22);
        visuals.code_bg_color = SURFACE_RAISED;
        visuals.selection.bg_fill = CODEX.gamma_multiply(0.35);
        visuals.selection.stroke = Stroke::new(1.0, CODEX);
        visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
        visuals.widgets.inactive.bg_fill = SURFACE_RAISED;
        visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER);
        visuals.widgets.hovered.bg_fill = Color32::from_rgb(33, 49, 61);
        visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, CODEX);
        visuals.widgets.active.bg_fill = Color32::from_rgb(38, 60, 69);
        visuals.widgets.active.bg_stroke = Stroke::new(1.0, CODEX);
        ctx.set_theme(egui::Theme::Dark);
        ctx.set_visuals_of(egui::Theme::Dark, visuals);
        ctx.style_mut_of(egui::Theme::Dark, |style| {
            style.spacing.item_spacing = egui::vec2(6.0, 6.0);
            style.spacing.button_padding = egui::vec2(8.0, 5.0);
            style.spacing.interact_size.y = 28.0;
        });
        self.visuals_configured = true;
    }
}

/// Thousand-separated integer, e.g. `1234567` -> `"1,234,567"`.
fn format_int(n: u64) -> String {
    let digits = n.to_string();
    let bytes = digits.as_bytes();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*b as char);
    }
    out
}

/// Compact magnitude, e.g. `7832248` -> `"7.83M"`. Used where full
/// comma-separated numbers would overflow the (fixed, non-resizable) window.
fn format_compact(n: u64) -> String {
    let n = n as f64;
    if n >= 1_000_000_000.0 {
        format!("{:.2}B", n / 1_000_000_000.0)
    } else if n >= 1_000_000.0 {
        format!("{:.2}M", n / 1_000_000.0)
    } else if n >= 1_000.0 {
        format!("{:.1}K", n / 1_000.0)
    } else {
        format!("{n:.0}")
    }
}

fn surface() -> egui::Frame {
    egui::Frame::new()
        .fill(SURFACE)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(8)
        .inner_margin(egui::Margin::same(10))
}

fn metric(ui: &mut egui::Ui, label: &str, value: String, color: Color32, size: f32) {
    ui.label(egui::RichText::new(label).small().color(MUTED));
    ui.add_space(2.0);
    ui.label(
        egui::RichText::new(value)
            .monospace()
            .size(size)
            .strong()
            .color(color),
    );
}

/// A single `icon value` chip, color-coded so the icon and its number read
/// as one unit at a glance instead of requiring the glyph to be parsed.
fn chip(ui: &mut egui::Ui, icon: &str, value: String, color: Color32) {
    ui.label(
        egui::RichText::new(icon)
            .monospace()
            .size(12.0)
            .strong()
            .color(color),
    );
    ui.label(
        egui::RichText::new(value)
            .monospace()
            .size(12.0)
            .strong()
            .color(color),
    );
}

/// Remaining time until `target`, compact: `"2h14m"`, `"3d5h"`, or `"now"`
/// once the window has already rolled over.
fn format_remaining(target: chrono::DateTime<chrono::Utc>) -> String {
    let minutes = (target - chrono::Utc::now()).num_minutes();
    if minutes <= 0 {
        "now".to_string()
    } else if minutes >= 60 * 24 {
        format!("{}d{}h", minutes / (60 * 24), (minutes / 60) % 24)
    } else if minutes >= 60 {
        format!("{}h{}m", minutes / 60, minutes % 60)
    } else {
        format!("{minutes}m")
    }
}

/// A `TAG value` quota-reset chip, e.g. `CC 2h14m`.
fn reset_chip(
    ui: &mut egui::Ui,
    tag: &str,
    color: Color32,
    reset_at: Option<chrono::DateTime<chrono::Utc>>,
) {
    let value = reset_at
        .map(format_remaining)
        .unwrap_or_else(|| "—".to_string());
    ui.label(
        egui::RichText::new(tag)
            .monospace()
            .size(11.0)
            .strong()
            .color(color),
    );
    ui.label(
        egui::RichText::new(value)
            .monospace()
            .size(11.0)
            .color(MUTED),
    );
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.configure_visuals(ui.ctx());
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_secs(1));
        let stats = self.snapshot.lock().unwrap().clone();

        let totals = totals_for(&stats, None, None);
        surface().show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 5.0;
                chip(ui, "↑", format_compact(totals.input), IN_COLOR);
                chip(ui, "↓", format_compact(totals.output), OUT_COLOR);
                chip(
                    ui,
                    "⇄",
                    format_compact(totals.cache_read + totals.cache_write),
                    CACHE_COLOR,
                );
                ui.separator();
                reset_chip(ui, "CC", CLAUDE, stats.claude_reset_at());
                reset_chip(ui, "CX", CODEX, stats.codex_reset_at);
            });
            ui.separator();
            ui.columns(2, |columns| {
                metric(
                    &mut columns[0],
                    "TOTAL TOKENS",
                    format_int(totals.total_tokens()),
                    CODEX,
                    18.0,
                );
                metric(
                    &mut columns[1],
                    "EST. COST",
                    format!("${:.2}", totals.cost),
                    CODEX,
                    18.0,
                );
            });
        });

        ui.label(
            egui::RichText::new("LIVE ACTIVITY")
                .strong()
                .small()
                .color(MUTED),
        );
        surface().show(ui, |ui| {
            egui::ScrollArea::vertical()
                .max_height(118.0)
                .show(ui, |ui| {
                    if stats.feed.is_empty() {
                        ui.label(egui::RichText::new("No activity yet").color(MUTED));
                    }
                    // Newest first: the scroll area opens at its top by
                    // default, so the most recent entry is already in view
                    // with no scrolling, and new entries never get appended
                    // below the fold.
                    for entry in stats.feed.iter().rev() {
                        ui.horizontal(|ui| {
                            let local_time = entry.ts.with_timezone(&chrono::Local);
                            ui.label(
                                egui::RichText::new(local_time.format("%H:%M:%S").to_string())
                                    .monospace()
                                    .color(MUTED),
                            );
                            ui.label(
                                egui::RichText::new(Self::source_tag(entry.source))
                                    .small()
                                    .strong()
                                    .color(Self::source_color(entry.source)),
                            );
                            ui.label(egui::RichText::new(entry.model.as_str()).small());
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "{} tok  ${:.4}",
                                            format_int(entry.tokens),
                                            entry.cost
                                        ))
                                        .monospace()
                                        .small(),
                                    );
                                },
                            );
                        });
                    }
                });
        });

        ui.label(
            egui::RichText::new("TOKENS / HOUR")
                .strong()
                .small()
                .color(MUTED),
        );
        let claude_hours = hourly_totals_for(&stats, None, Some(Source::Claude));
        let codex_hours = hourly_totals_for(&stats, None, Some(Source::Codex));
        // Only elapsed hours carry real data; a 0..24 line chart made the
        // still-to-come hours look like usage crashing to zero. Bars over
        // 0..=now represent each hour's discrete total instead of a
        // misleading interpolated trend.
        let current_hour = chrono::Local::now().hour() as usize;
        let claude_bars: Vec<Bar> = (0..=current_hour)
            .map(|hour| {
                Bar::new(hour as f64 - 0.19, claude_hours[hour].total_tokens() as f64).width(0.38)
            })
            .collect();
        let codex_bars: Vec<Bar> = (0..=current_hour)
            .map(|hour| {
                Bar::new(hour as f64 + 0.19, codex_hours[hour].total_tokens() as f64).width(0.38)
            })
            .collect();
        surface().show(ui, |ui| {
            Plot::new("hourly_chart")
                .height(88.0)
                .show_axes([true, false])
                .show_grid([false, false])
                .allow_zoom(false)
                .allow_drag(false)
                .allow_scroll(false)
                .include_x(0.0)
                .include_x(23.0)
                .legend(Legend::default())
                .show(ui, |plot_ui| {
                    plot_ui.bar_chart(BarChart::new("Claude Code", claude_bars).color(CLAUDE));
                    plot_ui.bar_chart(BarChart::new("Codex", codex_bars).color(CODEX));
                });
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

    #[test]
    fn format_compact_uses_the_largest_fitting_unit() {
        assert_eq!(format_compact(0), "0");
        assert_eq!(format_compact(999), "999");
        assert_eq!(format_compact(1_500), "1.5K");
        assert_eq!(format_compact(537_819), "537.8K");
        assert_eq!(format_compact(7_832_248), "7.83M");
        assert_eq!(format_compact(1_500_000_000), "1.50B");
    }
}

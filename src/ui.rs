use chrono::Timelike;
use eframe::egui::{self, Color32, Stroke};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};

use crate::model::{Source, Stats, Totals, hourly_totals_for, totals_for};
use crate::pricing::PricingTable;
use crate::settings::Settings;
use crate::tray::{Command as TrayCommand, Tray};

pub struct App {
    snapshot: Arc<Mutex<Arc<Stats>>>,
    settings: Settings,
    settings_path: PathBuf,
    pricing_path: PathBuf,
    tray_events: mpsc::Receiver<TrayCommand>,
    _tray: Tray,
    styled: bool,
    view: View,
    window_visible: bool,
    quitting: bool,
}

// Only two hues carry meaning: which tool produced the usage. Everything else
// is greyscale, so the numbers — not the decoration — are what the eye lands on.
const CLAUDE: Color32 = Color32::from_rgb(232, 148, 74);
const CODEX: Color32 = Color32::from_rgb(76, 159, 255);

const BG: Color32 = Color32::from_rgb(15, 19, 25);
const TEXT: Color32 = Color32::from_rgb(232, 237, 242);
const MUTED: Color32 = Color32::from_rgb(122, 136, 153);
const DIVIDER: Color32 = Color32::from_rgb(33, 42, 53);
const TRACK: Color32 = Color32::from_rgb(27, 35, 48);

// 450px of window height is a hard budget: hero, quota, feed and chart all
// have to come out of it. Every space below was measured against that, not
// picked by feel — the feed is the flexible one, so slack anywhere else is
// slack taken directly out of it.
const MARGIN: i8 = 12;
const CHART_HEIGHT: f32 = 40.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    Today,
    Week,
    History,
}

impl View {
    fn next(self) -> Self {
        match self {
            Self::Today => Self::Week,
            Self::Week => Self::History,
            Self::History => Self::Today,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Today => "Today",
            Self::Week => "Week",
            Self::History => "History",
        }
    }
}

impl App {
    pub fn new(
        snapshot: Arc<Mutex<Arc<Stats>>>,
        settings: Settings,
        settings_path: PathBuf,
        pricing_path: PathBuf,
        tray_events: mpsc::Receiver<TrayCommand>,
        tray: Tray,
    ) -> Self {
        App {
            snapshot,
            settings,
            settings_path,
            pricing_path,
            tray_events,
            _tray: tray,
            styled: false,
            view: View::Today,
            window_visible: true,
            quitting: false,
        }
    }

    fn handle_tray_events(&mut self, ctx: &egui::Context) {
        while let Ok(command) = self.tray_events.try_recv() {
            match command {
                TrayCommand::ToggleWindow => {
                    self.window_visible = !self.window_visible;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(self.window_visible));
                }
                TrayCommand::ToggleCloseToTray => {
                    self.settings.close_to_tray = !self.settings.close_to_tray;
                    self.save_settings();
                }
                TrayCommand::ToggleNotifications => {
                    self.settings.notifications_enabled = !self.settings.notifications_enabled;
                    self.save_settings();
                }
                TrayCommand::OpenPricing => {
                    if let Err(error) = std::process::Command::new("explorer")
                        .arg(&self.pricing_path)
                        .spawn()
                    {
                        eprintln!("warning: failed opening pricing.json: {error}");
                    }
                }
                TrayCommand::Quit => {
                    self.quitting = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
    }

    fn save_settings(&self) {
        if let Err(error) = self.settings.save(&self.settings_path) {
            eprintln!("warning: failed saving settings: {error}");
        }
        self._tray.sync_settings(self.settings);
    }

    fn source_color(source: Source) -> Color32 {
        match source {
            Source::Claude => CLAUDE,
            Source::Codex => CODEX,
        }
    }

    fn style(&mut self, ctx: &egui::Context) {
        if self.styled {
            return;
        }
        install_fonts(ctx);

        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = BG;
        visuals.window_fill = BG;
        visuals.extreme_bg_color = BG;
        visuals.override_text_color = Some(TEXT);
        // Nothing in this window is clickable, so every widget outline egui
        // would otherwise draw is noise.
        visuals.widgets.noninteractive.bg_stroke = Stroke::NONE;
        ctx.set_theme(egui::Theme::Dark);
        ctx.set_visuals_of(egui::Theme::Dark, visuals);
        ctx.style_mut_of(egui::Theme::Dark, |style| {
            style.spacing.item_spacing = egui::vec2(6.0, 1.0);
            style.spacing.scroll.bar_width = 4.0;
            style.spacing.scroll.floating = true;
        });
        self.styled = true;
    }
}

/// Swap egui's bundled fonts for the system UI face and a monospace with
/// tabular figures — without this the digits shift horizontally on every
/// update, which is the single most amateur-looking thing a live counter
/// can do. Falls back to egui's defaults if the files aren't there.
fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let load = |path: &str| std::fs::read(path).ok().map(egui::FontData::from_owned);

    if let Some(ui_font) = load(r"C:\Windows\Fonts\segoeui.ttf") {
        fonts.font_data.insert("ui".into(), Arc::new(ui_font));
        fonts
            .families
            .entry(egui::FontFamily::Proportional)
            .or_default()
            .insert(0, "ui".into());
    }
    if let Some(mono_font) = load(r"C:\Windows\Fonts\CascadiaMono.ttf") {
        fonts.font_data.insert("mono".into(), Arc::new(mono_font));
        fonts
            .families
            .entry(egui::FontFamily::Monospace)
            .or_default()
            .insert(0, "mono".into());
    }
    ctx.set_fonts(fonts);
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

/// Remaining time until `target`, compact: `"2h 14m"`, `"3d 5h"`, or `"now"`
/// once the window has already rolled over.
fn format_remaining(target: chrono::DateTime<chrono::Utc>) -> String {
    let minutes = (target - chrono::Utc::now()).num_minutes();
    if minutes <= 0 {
        "now".to_string()
    } else if minutes >= 60 * 24 {
        format!("{}d {}h", minutes / (60 * 24), (minutes / 60) % 24)
    } else if minutes >= 60 {
        format!("{}h {}m", minutes / 60, minutes % 60)
    } else {
        format!("{minutes}m")
    }
}

fn label(text: &str, size: f32, color: Color32) -> egui::RichText {
    egui::RichText::new(text).size(size).color(color)
}

fn number(text: String, size: f32, color: Color32) -> egui::RichText {
    egui::RichText::new(text)
        .monospace()
        .size(size)
        .color(color)
}

/// Section heading. Deliberately quiet — it orients, it does not compete with
/// the values underneath it.
fn heading(ui: &mut egui::Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .size(10.0)
            .color(MUTED)
            .extra_letter_spacing(0.8),
    );
    ui.add_space(3.0);
}

fn divider(ui: &mut egui::Ui) {
    ui.add_space(6.0);
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 1.0), egui::Sense::hover());
    ui.painter()
        .hline(rect.x_range(), rect.center().y, Stroke::new(1.0, DIVIDER));
    ui.add_space(6.0);
}

/// A quota row: name, time left, and a hairline meter. `fill` is what the
/// meter shows, in `0.0..=1.0` — for Claude that is the account's real
/// utilization (quota consumed), for Codex, which reports no utilization, it
/// falls back to how far through the window we are.
fn reset_row(
    ui: &mut egui::Ui,
    name: &str,
    color: Color32,
    reset_at: Option<chrono::DateTime<chrono::Utc>>,
    fill: Option<f32>,
) {
    ui.horizontal(|ui| {
        ui.label(label(name, 12.0, MUTED));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let remaining = reset_at
                .map(format_remaining)
                .unwrap_or_else(|| "—".to_string());
            ui.label(number(remaining, 12.0, TEXT));
        });
    });
    ui.add_space(2.0);

    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 3.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 1.5, TRACK);
    if let Some(fraction) = fill {
        let mut filled = rect;
        filled.set_width(rect.width() * fraction.clamp(0.0, 1.0));
        ui.painter().rect_filled(filled, 1.5, color);
    }
    ui.add_space(6.0);
}

/// Both providers report quota consumed as a percentage; the meter wants a
/// fraction.
fn used_fill(used_percent: Option<f64>) -> Option<f32> {
    used_percent.map(|percent| (percent / 100.0) as f32)
}

/// 24 stacked bars, one per hour of the local day, drawn directly rather than
/// through a plot widget: at this size axes, gridlines and a legend cost more
/// pixels than the data they annotate. Hours that haven't happened yet are
/// left empty instead of plotted as zero.
fn hourly_chart(ui: &mut egui::Ui, stats: &Stats, height: f32) {
    let claude = hourly_totals_for(stats, None, Some(Source::Claude));
    let codex = hourly_totals_for(stats, None, Some(Source::Codex));
    let current_hour = chrono::Local::now().hour() as usize;

    let peak = (0..24)
        .map(|hour| claude[hour].total_tokens() + codex[hour].total_tokens())
        .max()
        .unwrap_or(0)
        .max(1) as f32;

    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    let painter = ui.painter();

    let gap = 2.0;
    let bar_width = (width - gap * 23.0) / 24.0;
    for hour in 0..24 {
        let x = rect.left() + hour as f32 * (bar_width + gap);
        // A 1px baseline stub keeps the 24-slot grid legible even where an
        // elapsed hour genuinely had no usage.
        let baseline = egui::Rect::from_min_size(
            egui::pos2(x, rect.bottom() - 1.0),
            egui::vec2(bar_width, 1.0),
        );
        painter.rect_filled(baseline, 0.0, TRACK);
        if hour > current_hour {
            continue;
        }

        let mut y = rect.bottom();
        for (totals, color) in [(&codex[hour], CODEX), (&claude[hour], CLAUDE)] {
            let value = totals.total_tokens();
            if value == 0 {
                continue;
            }
            let bar_height = (value as f32 / peak) * (height - 2.0);
            let segment = egui::Rect::from_min_size(
                egui::pos2(x, y - bar_height),
                egui::vec2(bar_width, bar_height),
            );
            painter.rect_filled(segment, 1.0, color);
            y -= bar_height;
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.style(ui.ctx());
        self.handle_tray_events(ui.ctx());
        if self.settings.close_to_tray
            && !self.quitting
            && ui.ctx().input(|input| input.viewport().close_requested())
        {
            self.window_visible = false;
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_secs(1));
        let stats = self.snapshot.lock().unwrap().clone();
        let totals = totals_for(&stats, None, None);

        egui::Frame::new()
            .inner_margin(egui::Margin::same(MARGIN))
            .show(ui, |ui| {
                if self.view != View::Today {
                    self.ledger(ui, &stats);
                    return;
                }
                // Stack the chart up from the bottom edge, then let everything
                // else fill the slack above it. Splitting the two by hand meant
                // subtracting a constant, and a wrong constant silently clipped
                // the caption off the bottom of the window (or, the other
                // direction, let the dashboard overflow into the chart).
                ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                    ui.horizontal(|ui| {
                        ui.label(label("TOKENS / HOUR", 9.0, MUTED));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(label("00:00 – 23:00", 9.0, MUTED));
                        });
                    });
                    ui.add_space(3.0);
                    hourly_chart(ui, &stats, CHART_HEIGHT);
                    ui.add_space(5.0);
                    divider(ui);

                    ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                        self.dashboard(ui, &stats, &totals);
                    });
                });
            });
    }
}

impl App {
    /// Everything above the chart: header, hero figure, quota meters, feed.
    fn dashboard(&mut self, ui: &mut egui::Ui, stats: &Stats, totals: &Totals) {
        self.view_header(ui, stats);
        ui.add_space(3.0);

        // The hero: the one number worth reading from across the room.
        ui.label(
            egui::RichText::new(format!("${:.2}", totals.cost))
                .monospace()
                .size(28.0)
                .color(TEXT),
        );
        ui.label(label(
            &format!("{} tokens today", format_int(totals.total_tokens())),
            11.0,
            MUTED,
        ));
        ui.add_space(5.0);
        // Spelled out rather than glyphed: an arrow pair is a guessing
        // game, and the one for cache (⇄) isn't even in the mono face.
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 11.0;
            for (name, value) in [
                ("in", totals.input),
                ("out", totals.output),
                ("cache", totals.cache_read + totals.cache_write),
            ] {
                ui.label(number(
                    format!("{name} {}", format_compact(value)),
                    11.0,
                    MUTED,
                ));
            }
        });

        divider(ui);
        heading(ui, "QUOTA");
        status_marker(ui, stats, PricingTable::load(&self.pricing_path).is_err());
        reset_row(
            ui,
            "Claude · session",
            CLAUDE,
            stats.claude_reset_at(),
            used_fill(stats.claude_quota.five_hour.map(|w| w.utilization)),
        );
        reset_row(
            ui,
            "Claude · week",
            CLAUDE,
            stats.claude_weekly_reset_at(),
            used_fill(stats.claude_quota.seven_day.map(|w| w.utilization)),
        );
        reset_row(
            ui,
            "Codex",
            CODEX,
            stats.codex_reset_at,
            used_fill(stats.codex_used_percent),
        );

        divider(ui);
        heading(ui, "ACTIVITY");
        // The chart below already claimed its space, so whatever is
        // left here is exactly the feed's.
        activity_feed(ui, stats, ui.available_height());
    }

    fn view_header(&mut self, ui: &mut egui::Ui, stats: &Stats) {
        ui.horizontal(|ui| {
            if ui.small_button(self.view.label()).clicked() {
                self.view = self.view.next();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                live_indicator(ui, stats);
            });
        });
    }

    fn ledger(&mut self, ui: &mut egui::Ui, stats: &Stats) {
        // The header may advance the view during this frame. Keep rendering
        // the view that entered the ledger; Today renders on the next frame.
        let view = self.view;
        self.view_header(ui, stats);
        let pricing = PricingTable::load(&self.pricing_path).ok();
        status_marker(ui, stats, pricing.is_none());
        divider(ui);

        match view {
            View::Week => {
                heading(ui, "LAST 7 DAYS");
                for offset in (0..7).rev() {
                    let day = stats.current_day - chrono::Duration::days(offset);
                    let totals = if day == stats.current_day {
                        totals_for(stats, None, None)
                    } else {
                        historical_totals(stats, pricing.as_ref(), day, day)
                    };
                    let name = if offset == 0 {
                        "Today".to_string()
                    } else {
                        day.format("%a %d").to_string()
                    };
                    totals_row(ui, &name, &totals);
                }
                divider(ui);
                heading(ui, "LIVE TODAY");
                activity_feed(ui, stats, ui.available_height());
            }
            View::History => {
                egui::ScrollArea::vertical()
                    .max_height(history_scroll_height(ui.available_height()))
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        heading(ui, "RECENT DAYS");
                        for offset in 1..=14 {
                            let day = stats.current_day - chrono::Duration::days(offset);
                            let totals = historical_totals(stats, pricing.as_ref(), day, day);
                            if totals.total_tokens() > 0 {
                                totals_row(ui, &day.format("%a %d %b").to_string(), &totals);
                            }
                        }
                        divider(ui);
                        heading(ui, "WEEKLY TOTALS");
                        for week in 1..=4 {
                            let end = stats.current_day - chrono::Duration::days((week * 7) as i64);
                            let start = end - chrono::Duration::days(6);
                            let totals = historical_totals(stats, pricing.as_ref(), start, end);
                            if totals.total_tokens() > 0 {
                                totals_row(
                                    ui,
                                    &format!("{}–{}", start.format("%d %b"), end.format("%d %b")),
                                    &totals,
                                );
                            }
                        }
                    });
            }
            View::Today => unreachable!(),
        }
    }
}

fn history_scroll_height(available: f32) -> f32 {
    available.max(0.0)
}

fn historical_totals(
    stats: &Stats,
    pricing: Option<&PricingTable>,
    start: chrono::NaiveDate,
    end: chrono::NaiveDate,
) -> Totals {
    pricing.map_or_else(Totals::default, |pricing| {
        stats
            .history
            .priced_totals_in(start..=end, pricing, None, None)
    })
}

fn totals_row(ui: &mut egui::Ui, name: &str, totals: &Totals) {
    ui.horizontal(|ui| {
        ui.label(label(name, 11.0, MUTED));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(number(format!("${:.2}", totals.cost), 11.0, TEXT));
            ui.label(number(format_compact(totals.total_tokens()), 11.0, MUTED));
        });
    });
    ui.add_space(3.0);
}

fn status_marker(ui: &mut egui::Ui, stats: &Stats, pricing_invalid: bool) {
    if pricing_invalid
        || !stats.unknown_pricing_models.is_empty()
        || stats.quota_is_stale_at(chrono::Utc::now())
    {
        let mut markers = Vec::new();
        if pricing_invalid || !stats.unknown_pricing_models.is_empty() {
            markers.push("pricing invalid");
        }
        if stats.quota_is_stale_at(chrono::Utc::now()) {
            markers.push("quota stale");
        }
        ui.label(label(
            &markers.join(" · "),
            10.0,
            Color32::from_rgb(224, 171, 81),
        ));
        ui.add_space(3.0);
    }
}

/// Pulses while usage is still arriving, so the window reads as live rather
/// than as a screenshot of a number.
fn live_indicator(ui: &mut egui::Ui, stats: &Stats) {
    let last = stats.feed.back().map(|entry| entry.ts);
    let active = last.is_some_and(|ts| (chrono::Utc::now() - ts).num_seconds() < 120);

    let (text, color) = if active {
        let phase = ui.input(|i| i.time) as f32 * 1.6;
        let alpha = 0.45 + 0.55 * (0.5 + 0.5 * phase.sin());
        ("live", TEXT.gamma_multiply(alpha))
    } else {
        ("idle", MUTED.gamma_multiply(0.7))
    };

    ui.label(label(text, 10.0, MUTED));
    let (rect, _) = ui.allocate_exact_size(egui::vec2(6.0, 6.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 3.0, color);
}

/// Fixed so the feed can be sized to a whole number of rows. A scroll area
/// that ends mid-row shows a sliced-off line of text, which is exactly the
/// kind of detail that makes a window look unfinished.
const FEED_ROW: f32 = 22.0;

fn activity_feed(ui: &mut egui::Ui, stats: &Stats, available: f32) {
    let height = activity_height(available);
    egui::ScrollArea::vertical()
        .max_height(height)
        .min_scrolled_height(0.0)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            // Rows must pitch at exactly FEED_ROW for the flooring above to
            // land on a row boundary; any vertical item spacing would add to
            // each row's height and put a sliced row back at the bottom.
            ui.spacing_mut().item_spacing.y = 0.0;
            if stats.feed.is_empty() {
                ui.label(label("Waiting for activity…", 11.0, MUTED));
                return;
            }
            // Newest first: the scroll area opens at its top, so the most
            // recent entry is in view without scrolling.
            for entry in stats.feed.iter().rev() {
                let row = egui::vec2(ui.available_width(), FEED_ROW);
                ui.allocate_ui(row, |ui| {
                    ui.horizontal_centered(|ui| {
                        ui.spacing_mut().item_spacing.x = 7.0;
                        let local_time = entry.ts.with_timezone(&chrono::Local);
                        ui.label(number(local_time.format("%H:%M").to_string(), 11.0, MUTED));

                        // The source is encoded in this 2px rule rather than a
                        // "CC"/"CX" tag: it reads instantly and costs no width.
                        let (rule, _) =
                            ui.allocate_exact_size(egui::vec2(2.0, 12.0), egui::Sense::hover());
                        ui.painter()
                            .rect_filled(rule, 1.0, App::source_color(entry.source));

                        ui.label(label(short_model(&entry.model), 11.0, TEXT));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            ui.label(number(format!("${:.3}", entry.cost), 11.0, MUTED));
                            ui.label(number(format_compact(entry.tokens), 11.0, TEXT));
                        });
                    });
                });
            }
        });
}

fn activity_height(available: f32) -> f32 {
    (available.max(0.0) / FEED_ROW).floor() * FEED_ROW
}

/// Vendor prefixes are redundant once the source rule is colored, and they
/// push the model name into the token column on a 360px window.
fn short_model(model: &str) -> &str {
    model
        .strip_prefix("claude-")
        .or_else(|| model.strip_prefix("anthropic/"))
        .unwrap_or(model)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_button_cycles_today_week_history_today() {
        assert_eq!(View::Today.next(), View::Week);
        assert_eq!(View::Week.next(), View::History);
        assert_eq!(View::History.next(), View::Today);
    }

    #[test]
    fn history_scroll_height_never_goes_negative() {
        assert_eq!(history_scroll_height(-1.0), 0.0);
        assert_eq!(history_scroll_height(120.0), 120.0);
    }

    #[test]
    fn activity_height_reserves_the_chart_footer_without_slicing_a_row() {
        assert_eq!(activity_height(-1.0), 0.0);
        assert_eq!(activity_height(FEED_ROW * 2.0 + 1.0), FEED_ROW * 2.0);
    }

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

    #[test]
    fn short_model_strips_only_vendor_prefixes() {
        assert_eq!(short_model("claude-opus-4-8"), "opus-4-8");
        assert_eq!(short_model("gpt-5.5"), "gpt-5.5");
    }
}

use eframe::egui;
use egui_plot::{Line, Plot, PlotPoints};
use std::sync::{Arc, Mutex};

use crate::model::{Source, Stats, hourly_totals_for, totals_for};

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
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_secs(1));
        let stats = self.snapshot.lock().unwrap().clone();

        egui::CentralPanel::default().show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("Model:");
                egui::ComboBox::from_id_salt("model_filter")
                    .selected_text(self.selected_model.as_deref().unwrap_or("All"))
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

                ui.label("Source:");
                egui::ComboBox::from_id_salt("source_filter")
                    .selected_text(Self::source_label(self.selected_source))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.selected_source, None, "All");
                        ui.selectable_value(
                            &mut self.selected_source,
                            Some(Source::Claude),
                            "Claude Code",
                        );
                        ui.selectable_value(
                            &mut self.selected_source,
                            Some(Source::Codex),
                            "Codex",
                        );
                    });
            });

            ui.separator();

            let totals = totals_for(&stats, self.selected_model.as_deref(), self.selected_source);
            ui.label(format!("Input          {}", totals.input));
            ui.label(format!("Output         {}", totals.output));
            ui.label(format!("Cache read     {}", totals.cache_read));
            ui.label(format!("Cache write    {}", totals.cache_write));
            ui.separator();
            ui.label(format!("Total          {}", totals.total_tokens()));
            ui.label(format!("Est. cost      ${:.2}", totals.cost));

            ui.separator();
            ui.label("Tokens / hour (today)");
            let hours =
                hourly_totals_for(&stats, self.selected_model.as_deref(), self.selected_source);
            let points: PlotPoints<'_> = (0..24)
                .map(|hour| [hour as f64, hours[hour].total_tokens() as f64])
                .collect();
            Plot::new("hourly_chart")
                .view_aspect(3.0)
                .show(ui, |plot_ui| {
                    plot_ui.line(Line::new("Tokens", points));
                });
        });
    }
}

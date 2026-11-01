//! The Resources tab: the TUI's readings as meters, plus live CPU and memory charts. Core feeds
//! `app.insight` and `app.histories` and decides each value and colour; this module only draws
//! them, with egui's `Painter`.

use aisbx_core::actions::Action;
use aisbx_core::app::App;
use aisbx_core::insight::{self, Kind};
use aisbx_core::model::Sandbox;
use aisbx_core::stats::{self, Point};
use eframe::egui::{self, Align2, Color32, FontId, Pos2, RichText, Sense, Stroke};

use crate::{details, theme};

/// The charts' height, and their ids — fixed, so a test can find where they were drawn.
const CHART_H: f32 = 96.0;
pub const CPU_CHART: &str = "aisbx-chart-cpu";
pub const MEMORY_CHART: &str = "aisbx-chart-memory";

/// The tab, or — for a stopped sandbox, which is never started just to be measured — why not,
/// with what starts it as buttons (*Start*, *Shell*). Returns the action one asked for.
pub fn show(ui: &mut egui::Ui, app: &App, sb: &Sandbox) -> Option<Action> {
    if !sb.running {
        let notes = insight::resources_stopped_notes();
        let clicked = details::notes(ui, app, &details::prose(&notes));
        return clicked.or(details::key_buttons(ui, app, &notes));
    }
    let cell = &app.insight.resources;
    let Some(sample) = &cell.value else {
        let pending = insight::pending_text(Kind::Resources, cell.is_loading());
        ui.label(RichText::new(pending).weak());
        if let Some(err) = &cell.error {
            ui.label(RichText::new(err).color(theme::danger(ui.visuals().dark_mode)));
        }
        return None;
    };

    let vcpus = format!("{} vCPU", sample.cpus);
    let used = |used, total| format!("{} used / {} total", stats::gb(used), stats::gb(total));
    let memory = used(sample.mem_used_kb(), sample.mem_total_kb);
    let disk = used(sample.disk_used_kb, sample.disk_total_kb);
    let uptime = stats::duration(sample.uptime_s as u64);
    egui::Grid::new("resources")
        .num_columns(2)
        .min_col_width(96.0)
        .spacing([10.0, 6.0])
        .show(ui, |ui| {
            ui.label(RichText::new("CPUs").weak());
            match app.insight.cpu {
                Some(load) => meter(ui, load, vcpus),
                None => {
                    let text = format!("{vcpus} — {}", insight::LOAD_NEEDS_SECOND);
                    ui.label(RichText::new(text).weak());
                }
            }
            ui.end_row();
            ui.label(RichText::new("Memory").weak());
            meter(ui, sample.mem_fraction(), memory);
            ui.end_row();
            ui.label(RichText::new("Disk").weak());
            meter(ui, sample.disk_fraction(), disk);
            ui.end_row();
            ui.label(RichText::new("Uptime").weak());
            ui.label(uptime);
            ui.end_row();
        });

    let points: Vec<Point> = match app.histories.get(&sb.name) {
        Some(history) => history.points().copied().collect(),
        None => Vec::new(),
    };
    let minutes = stats::HISTORY_SPAN_S / 60.0;
    let about = format!(
        "the last {minutes:.0} minutes of the sandbox's uptime, read while this tab is open"
    );
    ui.add_space(8.0);
    ui.label(theme::strong(RichText::new("CPU %")))
        .on_hover_text(format!("Busy share of all its vCPUs over {about}"));
    chart(ui, CPU_CHART, &points, |p| p.cpu, false);
    ui.add_space(6.0);
    ui.label(theme::strong(RichText::new("Memory")))
        .on_hover_text(format!("Memory in use over {about}"));
    chart(ui, MEMORY_CHART, &points, memory_fraction, true);

    ui.add_space(6.0);
    ui.label(RichText::new(insight::RESOURCES_SCOPE).weak());
    if let Some(err) = &cell.error {
        let color = theme::warn(ui.visuals().dark_mode);
        ui.label(RichText::new(insight::reading_failed(err)).color(color));
    }
    None
}

/// The width of a meter's bar.
const METER_W: f32 = 96.0;

/// A fraction as a thin bar in core's level's colour, its percentage and text beside it, not on it.
fn meter(ui: &mut egui::Ui, fraction: f32, text: String) {
    let dark = ui.visuals().dark_mode;
    let color = theme::level(stats::level(fraction), dark);
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(METER_W, 8.0), Sense::hover());
        let track = ui.visuals().widgets.noninteractive.bg_stroke.color;
        ui.painter().rect_filled(rect, 4.0, track);
        let filled = rect.with_max_x(rect.left() + rect.width() * fraction.clamp(0.0, 1.0));
        ui.painter().rect_filled(filled, 4.0, color);
        let percent =
            theme::strong(RichText::new(format!("{:.0}%", fraction * 100.0))).color(color);
        ui.label(percent);
        ui.label(text);
    });
}

fn memory_fraction(p: &Point) -> Option<f32> {
    Some(p.mem_fraction())
}

/// What the pointer reads off either chart: the point's CPU share and memory.
pub fn readout(p: &Point) -> String {
    let cpu = match p.cpu {
        Some(cpu) => format!("CPU {:.0}%", cpu * 100.0),
        None => "CPU —".to_string(),
    };
    let memory = format!(
        "{} of {}",
        stats::gb(p.mem_used_kb),
        stats::gb(p.mem_total_kb)
    );
    let at = stats::duration(p.uptime_s as u64);
    format!("{cpu} · memory {memory} · up {at}")
}

/// One chart over the history's window, newest on the right: a line per segment (a point with
/// `gap_before` or no value starts a new one), filled below for memory, and under the pointer a
/// marker with the [`readout`] of the nearest point.
fn chart(
    ui: &mut egui::Ui,
    id: &str,
    points: &[Point],
    value: fn(&Point) -> Option<f32>,
    fill: bool,
) {
    let width = ui.available_width().max(120.0);
    let (_, rect) = ui.allocate_space(egui::vec2(width, CHART_H));
    let response = ui.interact(rect, egui::Id::unique(id), Sense::hover());
    let painter = ui.painter_at(rect);
    let visuals = ui.visuals();
    let dark = visuals.dark_mode;
    let weak = visuals.weak_text_color();
    painter.rect_filled(rect, 4.0, visuals.extreme_bg_color);
    let grid = Stroke::new(1.0, weak.gamma_multiply(0.25));
    for f in [0.25, 0.5, 0.75] {
        let y = rect.bottom() - f * rect.height();
        painter.line_segment(
            [Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)],
            grid,
        );
    }
    let small = FontId::proportional(10.0);
    let corner = |pos, align, text: &str| {
        painter.text(pos, align, text, small.clone(), weak);
    };
    let pad = egui::vec2(4.0, 2.0);
    corner(
        rect.right_top() + egui::vec2(-pad.x, pad.y),
        Align2::RIGHT_TOP,
        "100%",
    );
    let span = format!("−{:.0} min", stats::HISTORY_SPAN_S / 60.0);
    corner(
        rect.left_bottom() + egui::vec2(pad.x, -pad.y),
        Align2::LEFT_BOTTOM,
        &span,
    );
    corner(rect.right_bottom() - pad, Align2::RIGHT_BOTTOM, "now");

    let Some(newest) = points.last() else {
        let text = "the chart starts with the next reading";
        painter.text(rect.center(), Align2::CENTER_CENTER, text, small, weak);
        return;
    };
    let left = newest.uptime_s - stats::HISTORY_SPAN_S;
    let x = |p: &Point| {
        let f = ((p.uptime_s - left) / stats::HISTORY_SPAN_S).clamp(0.0, 1.0) as f32;
        rect.left() + f * rect.width()
    };
    let y = |v: f32| rect.bottom() - v.clamp(0.0, 1.0) * rect.height();
    let last = points.iter().rev().find_map(value).unwrap_or(0.0);
    let color = theme::level(stats::level(last), dark);

    let mut segments: Vec<Vec<Pos2>> = Vec::new();
    let mut open = false;
    for p in points {
        let Some(v) = value(p) else {
            open = false;
            continue;
        };
        if !open || p.gap_before {
            segments.push(Vec::new());
        }
        open = true;
        if let Some(segment) = segments.last_mut() {
            segment.push(Pos2::new(x(p), y(v)));
        }
    }
    for segment in &segments {
        if fill {
            let under = color.gamma_multiply(0.25);
            for pair in segment.windows(2) {
                let (a, b) = (pair[0], pair[1]);
                let floor = |p: Pos2| Pos2::new(p.x, rect.bottom());
                let quad = vec![a, b, floor(b), floor(a)];
                painter.add(egui::Shape::convex_polygon(quad, under, Stroke::NONE));
            }
        }
        match segment.as_slice() {
            [only] => {
                painter.circle_filled(*only, 1.5, color);
            }
            many => {
                painter.add(egui::Shape::line(many.to_vec(), Stroke::new(1.5, color)));
            }
        }
    }

    if let Some(pointer) = response.hover_pos() {
        let nearest = points.iter().min_by(|a, b| {
            (x(a) - pointer.x)
                .abs()
                .total_cmp(&(x(b) - pointer.x).abs())
        });
        if let Some(p) = nearest {
            let marker = Stroke::new(1.0, weak);
            let top = Pos2::new(x(p), rect.top());
            painter.line_segment([top, Pos2::new(x(p), rect.bottom())], marker);
            let font = FontId::proportional(12.0);
            let text = readout(p);
            let at = rect.left_top() + egui::vec2(6.0, 4.0);
            let text_color: Color32 = visuals.strong_text_color();
            painter.text(at, Align2::LEFT_TOP, text, font, text_color);
        }
    }
}

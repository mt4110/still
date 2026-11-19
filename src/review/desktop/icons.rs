use super::egui;

#[derive(Clone, Copy)]
pub(super) enum Icon {
    Help,
    #[cfg(target_os = "macos")]
    Disk,
    #[cfg(target_os = "macos")]
    Docker,
    Folder,
}

pub(super) fn paint(ui: &egui::Ui, rect: egui::Rect, icon: Icon, color: egui::Color32) {
    let painter = ui.painter();
    let stroke = egui::Stroke::new(1.5_f32, color);
    let at = |x: f32, y: f32| {
        egui::pos2(
            rect.left() + x * rect.width(),
            rect.top() + y * rect.height(),
        )
    };
    match icon {
        Icon::Help => {
            painter.circle_stroke(rect.center(), rect.width() * 0.42, stroke);
            painter.text(
                at(0.5, 0.49),
                egui::Align2::CENTER_CENTER,
                "?",
                egui::FontId::proportional(14.0),
                color,
            );
        }
        #[cfg(target_os = "macos")]
        Icon::Disk => {
            painter.rect_stroke(
                egui::Rect::from_min_max(at(0.06, 0.22), at(0.94, 0.83)),
                3.0,
                stroke,
                egui::StrokeKind::Inside,
            );
            painter.line_segment([at(0.15, 0.57), at(0.85, 0.57)], stroke);
            painter.circle_filled(at(0.75, 0.7), 1.2, color);
        }
        #[cfg(target_os = "macos")]
        Icon::Docker => {
            for (x, y) in [
                (0.08, 0.18),
                (0.4, 0.18),
                (0.08, 0.51),
                (0.4, 0.51),
                (0.72, 0.51),
            ] {
                painter.rect_stroke(
                    egui::Rect::from_min_max(at(x, y), at(x + 0.23, y + 0.25)),
                    1.0,
                    stroke,
                    egui::StrokeKind::Inside,
                );
            }
        }
        Icon::Folder => {
            painter.line_segment([at(0.08, 0.24), at(0.41, 0.24)], stroke);
            painter.line_segment([at(0.41, 0.24), at(0.53, 0.4)], stroke);
            painter.rect_stroke(
                egui::Rect::from_min_max(at(0.08, 0.4), at(0.94, 0.85)),
                2.0,
                stroke,
                egui::StrokeKind::Inside,
            );
            painter.line_segment([at(0.08, 0.24), at(0.08, 0.5)], stroke);
        }
    }
}

pub(super) fn button(ui: &mut egui::Ui, icon: Icon, label: &str) -> egui::Response {
    let font = egui::TextStyle::Button.resolve(ui.style());
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font, ui.visuals().text_color());
    let width = 18.0 + galley.size().x + 26.0;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 32.0), egui::Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label)
    });
    let visuals = ui.style().interact(&response);
    ui.painter().rect(
        rect,
        6.0,
        visuals.weak_bg_fill,
        visuals.bg_stroke,
        egui::StrokeKind::Inside,
    );
    let icon_rect = egui::Rect::from_center_size(
        egui::pos2(rect.left() + 17.0, rect.center().y),
        egui::vec2(18.0, 18.0),
    );
    paint(ui, icon_rect, icon, visuals.fg_stroke.color);
    ui.painter().galley(
        egui::pos2(rect.left() + 33.0, rect.center().y - galley.size().y / 2.0),
        galley,
        visuals.fg_stroke.color,
    );
    response
}

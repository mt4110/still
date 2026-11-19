use super::egui;

#[derive(Clone, Copy)]
pub(super) enum Icon {
    #[cfg(target_os = "macos")]
    Home,
    Help,
    #[cfg(target_os = "macos")]
    Disk,
    #[cfg(target_os = "macos")]
    Docker,
    Folder,
    #[cfg(target_os = "macos")]
    Sdk,
    #[cfg(target_os = "macos")]
    Clock,
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
        #[cfg(target_os = "macos")]
        Icon::Home => {
            painter.line_segment([at(0.07, 0.45), at(0.5, 0.09)], stroke);
            painter.line_segment([at(0.5, 0.09), at(0.93, 0.45)], stroke);
            painter.line_segment([at(0.2, 0.35), at(0.2, 0.9)], stroke);
            painter.line_segment([at(0.8, 0.35), at(0.8, 0.9)], stroke);
            painter.line_segment([at(0.2, 0.9), at(0.4, 0.9)], stroke);
            painter.line_segment([at(0.6, 0.9), at(0.8, 0.9)], stroke);
            painter.line_segment([at(0.4, 0.9), at(0.4, 0.62)], stroke);
            painter.line_segment([at(0.4, 0.62), at(0.6, 0.62)], stroke);
            painter.line_segment([at(0.6, 0.62), at(0.6, 0.9)], stroke);
        }
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
        #[cfg(target_os = "macos")]
        Icon::Sdk => {
            painter.line_segment([at(0.06, 0.31), at(0.5, 0.08)], stroke);
            painter.line_segment([at(0.5, 0.08), at(0.94, 0.31)], stroke);
            painter.line_segment([at(0.94, 0.31), at(0.5, 0.55)], stroke);
            painter.line_segment([at(0.5, 0.55), at(0.06, 0.31)], stroke);
            for y in [0.51, 0.72] {
                painter.line_segment([at(0.06, y), at(0.5, y + 0.23)], stroke);
                painter.line_segment([at(0.5, y + 0.23), at(0.94, y)], stroke);
            }
        }
        #[cfg(target_os = "macos")]
        Icon::Clock => {
            painter.circle_stroke(rect.center(), rect.width() * 0.41, stroke);
            painter.line_segment([at(0.5, 0.26), at(0.5, 0.51)], stroke);
            painter.line_segment([at(0.5, 0.51), at(0.7, 0.63)], stroke);
        }
    }
}

pub(super) fn button(ui: &mut egui::Ui, icon: Icon, label: &str) -> egui::Response {
    let font = egui::TextStyle::Button.resolve(ui.style());
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font, ui.visuals().text_color());
    let inset = super::controls::PADDING_X;
    let width = inset * 2.0 + 18.0 + 8.0 + galley.size().x;
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(width, super::controls::HEIGHT),
        egui::Sense::click(),
    );
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label)
    });
    let visuals = ui.style().interact(&response);
    ui.painter().rect(
        rect,
        7.0,
        visuals.weak_bg_fill,
        visuals.bg_stroke,
        egui::StrokeKind::Inside,
    );
    let icon_rect = egui::Rect::from_center_size(
        egui::pos2(rect.left() + inset + 9.0, rect.center().y),
        egui::vec2(18.0, 18.0),
    );
    paint(ui, icon_rect, icon, visuals.fg_stroke.color);
    #[cfg(target_os = "macos")]
    {
        let mut text_rect = rect;
        text_rect.min.x += inset + 18.0 + 8.0;
        text_rect.max.x -= inset;
        super::controls::paint_label(
            ui,
            text_rect,
            label,
            egui::Align::Min,
            visuals.fg_stroke.color,
        );
    }
    #[cfg(not(target_os = "macos"))]
    ui.painter().galley(
        egui::pos2(
            rect.left() + inset + 18.0 + 8.0,
            rect.center().y - galley.size().y / 2.0,
        ),
        galley,
        visuals.fg_stroke.color,
    );
    response
}

//! Icons drawn in code (original artwork; no third-party assets).

use egui::{Color32, Painter, Pos2, Rect, Stroke, StrokeKind, pos2, vec2};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    NewMail,
    Delete,
    Archive,
    Junk,
    Reply,
    ReplyAll,
    Forward,
    Move,
    Unread,
    Flag,
    Attach,
    Mail,
    Calendar,
    People,
    Tasks,
    Undo,
    Send,
    Discard,
    SendReceive,
    Offline,
}

fn p(r: Rect, x: f32, y: f32) -> Pos2 {
    pos2(r.left() + x * r.width(), r.top() + y * r.height())
}

fn envelope(painter: &Painter, r: Rect, s: Stroke) {
    let body = Rect::from_min_max(p(r, 0.08, 0.22), p(r, 0.92, 0.78));
    painter.rect_stroke(body, 1.5, s, StrokeKind::Middle);
    painter.line_segment([body.left_top(), p(r, 0.5, 0.52)], s);
    painter.line_segment([p(r, 0.5, 0.52), body.right_top()], s);
}

fn arrow_back(painter: &Painter, r: Rect, s: Stroke, dx: f32) {
    // A curved reply arrow: head pointing left, tail sweeping right and down.
    let tip = p(r, 0.12 + dx, 0.42);
    painter.line_segment([tip, p(r, 0.36 + dx, 0.2)], s);
    painter.line_segment([tip, p(r, 0.36 + dx, 0.64)], s);
    let pts: Vec<Pos2> = (0..=8)
        .map(|i| {
            let t = i as f32 / 8.0;
            let x = 0.12 + dx + t * (0.78 - dx);
            let y = 0.42 + t * t * 0.38;
            p(r, x, y)
        })
        .collect();
    painter.add(egui::Shape::line(pts, s));
}

pub fn paint(painter: &Painter, rect: Rect, icon: Icon, color: Color32) {
    let w = (rect.width() / 14.0).max(1.2);
    let s = Stroke::new(w, color);
    let r = rect;
    match icon {
        Icon::NewMail => {
            envelope(painter, r, s);
            let c = p(r, 0.82, 0.24);
            let k = r.width() * 0.16;
            painter.circle_filled(c, k, Color32::WHITE);
            let plus = Stroke::new(w, Color32::from_rgb(0x1a, 0x8f, 0x3c));
            painter.line_segment([c - vec2(k * 0.7, 0.0), c + vec2(k * 0.7, 0.0)], plus);
            painter.line_segment([c - vec2(0.0, k * 0.7), c + vec2(0.0, k * 0.7)], plus);
        }
        Icon::Mail => envelope(painter, r, s),
        Icon::Unread => {
            envelope(painter, r, s);
            painter.circle_filled(p(r, 0.85, 0.2), r.width() * 0.11, color);
        }
        Icon::Delete => {
            painter.line_segment([p(r, 0.18, 0.24), p(r, 0.82, 0.24)], s);
            painter.line_segment([p(r, 0.4, 0.24), p(r, 0.42, 0.14)], s);
            painter.line_segment([p(r, 0.42, 0.14), p(r, 0.58, 0.14)], s);
            painter.line_segment([p(r, 0.58, 0.14), p(r, 0.6, 0.24)], s);
            let pts = vec![p(r, 0.26, 0.28), p(r, 0.32, 0.88), p(r, 0.68, 0.88), p(r, 0.74, 0.28)];
            painter.add(egui::Shape::line(pts, s));
            for x in [0.42, 0.58] {
                painter.line_segment([p(r, x, 0.38), p(r, x, 0.78)], s);
            }
        }
        Icon::Archive => {
            painter.rect_stroke(Rect::from_min_max(p(r, 0.12, 0.18), p(r, 0.88, 0.36)), 1.0, s, StrokeKind::Middle);
            painter.rect_stroke(Rect::from_min_max(p(r, 0.18, 0.36), p(r, 0.82, 0.84)), 1.0, s, StrokeKind::Middle);
            painter.line_segment([p(r, 0.4, 0.5), p(r, 0.6, 0.5)], s);
        }
        Icon::Junk => {
            painter.circle_stroke(p(r, 0.5, 0.5), r.width() * 0.34, s);
            painter.line_segment([p(r, 0.26, 0.74), p(r, 0.74, 0.26)], s);
        }
        Icon::Reply => arrow_back(painter, r, s, 0.0),
        Icon::ReplyAll => {
            arrow_back(painter, r, s, 0.12);
            let tip = p(r, 0.04, 0.42);
            painter.line_segment([tip, p(r, 0.26, 0.2)], s);
            painter.line_segment([tip, p(r, 0.26, 0.64)], s);
        }
        Icon::Forward => {
            let tip = p(r, 0.88, 0.42);
            painter.line_segment([tip, p(r, 0.64, 0.2)], s);
            painter.line_segment([tip, p(r, 0.64, 0.64)], s);
            let pts: Vec<Pos2> = (0..=8)
                .map(|i| {
                    let t = i as f32 / 8.0;
                    p(r, 0.88 - t * 0.66, 0.42 + t * t * 0.38)
                })
                .collect();
            painter.add(egui::Shape::line(pts, s));
        }
        Icon::Move => {
            let pts = vec![p(r, 0.1, 0.28), p(r, 0.38, 0.28), p(r, 0.46, 0.38), p(r, 0.9, 0.38), p(r, 0.9, 0.82), p(r, 0.1, 0.82), p(r, 0.1, 0.28)];
            painter.add(egui::Shape::line(pts, s));
            painter.line_segment([p(r, 0.34, 0.6), p(r, 0.68, 0.6)], s);
            painter.line_segment([p(r, 0.68, 0.6), p(r, 0.58, 0.5)], s);
            painter.line_segment([p(r, 0.68, 0.6), p(r, 0.58, 0.7)], s);
        }
        Icon::Flag => {
            painter.line_segment([p(r, 0.24, 0.1), p(r, 0.24, 0.92)], s);
            let pts = vec![p(r, 0.24, 0.14), p(r, 0.8, 0.14), p(r, 0.66, 0.32), p(r, 0.8, 0.5), p(r, 0.24, 0.5)];
            painter.add(egui::Shape::convex_polygon(pts, color, Stroke::NONE));
        }
        Icon::Attach => {
            let pts = vec![
                p(r, 0.62, 0.3),
                p(r, 0.62, 0.72),
                p(r, 0.5, 0.86),
                p(r, 0.38, 0.72),
                p(r, 0.38, 0.22),
                p(r, 0.46, 0.12),
                p(r, 0.54, 0.22),
                p(r, 0.54, 0.66),
            ];
            painter.add(egui::Shape::line(pts, s));
        }
        Icon::Calendar => {
            let b = Rect::from_min_max(p(r, 0.12, 0.2), p(r, 0.88, 0.86));
            painter.rect_stroke(b, 2.0, s, StrokeKind::Middle);
            painter.line_segment([p(r, 0.12, 0.38), p(r, 0.88, 0.38)], s);
            for x in [0.32, 0.68] {
                painter.line_segment([p(r, x, 0.1), p(r, x, 0.28)], s);
            }
            for (x, y) in [(0.3, 0.54), (0.5, 0.54), (0.7, 0.54), (0.3, 0.72), (0.5, 0.72)] {
                painter.circle_filled(p(r, x, y), w * 0.8, color);
            }
        }
        Icon::People => {
            painter.circle_stroke(p(r, 0.5, 0.32), r.width() * 0.16, s);
            let pts: Vec<Pos2> = (0..=10)
                .map(|i| {
                    let a = std::f32::consts::PI * (i as f32 / 10.0);
                    p(r, 0.5 - 0.3 * a.cos(), 0.86 - 0.3 * a.sin())
                })
                .collect();
            painter.add(egui::Shape::line(pts, s));
        }
        Icon::Tasks => {
            painter.rect_stroke(Rect::from_min_max(p(r, 0.14, 0.14), p(r, 0.86, 0.86)), 2.0, s, StrokeKind::Middle);
            painter.line_segment([p(r, 0.3, 0.52), p(r, 0.44, 0.66)], s);
            painter.line_segment([p(r, 0.44, 0.66), p(r, 0.72, 0.34)], s);
        }
        Icon::Undo => {
            let tip = p(r, 0.18, 0.4);
            painter.line_segment([tip, p(r, 0.38, 0.22)], s);
            painter.line_segment([tip, p(r, 0.38, 0.58)], s);
            let pts: Vec<Pos2> = (0..=10)
                .map(|i| {
                    let a = std::f32::consts::PI * (i as f32 / 10.0);
                    p(r, 0.54 + 0.3 * a.sin(), 0.62 - 0.22 * a.cos())
                })
                .collect();
            painter.line_segment([tip, p(r, 0.54, 0.4)], s);
            painter.add(egui::Shape::line(pts, s));
        }
        Icon::Send => {
            let pts = vec![p(r, 0.1, 0.18), p(r, 0.9, 0.5), p(r, 0.1, 0.82), p(r, 0.24, 0.5), p(r, 0.1, 0.18)];
            painter.add(egui::Shape::line(pts, s));
            painter.line_segment([p(r, 0.24, 0.5), p(r, 0.56, 0.5)], s);
        }
        Icon::Discard => {
            painter.line_segment([p(r, 0.24, 0.24), p(r, 0.76, 0.76)], s);
            painter.line_segment([p(r, 0.76, 0.24), p(r, 0.24, 0.76)], s);
        }
        Icon::SendReceive => {
            // Two curved arrows chasing each other round a circle.
            for (start, tip) in [(0.15f32, 0.85f32), (1.15, 1.85)] {
                let pts: Vec<Pos2> = (0..=12)
                    .map(|i| {
                        let a = std::f32::consts::PI * (start + (tip - start) * i as f32 / 12.0);
                        p(r, 0.5 + 0.34 * a.cos(), 0.5 + 0.34 * a.sin())
                    })
                    .collect();
                let a = std::f32::consts::PI * tip;
                let end = p(r, 0.5 + 0.34 * a.cos(), 0.5 + 0.34 * a.sin());
                let back = std::f32::consts::PI * (tip - 0.18);
                let out = p(r, 0.5 + 0.48 * back.cos(), 0.5 + 0.48 * back.sin());
                let inn = p(r, 0.5 + 0.2 * back.cos(), 0.5 + 0.2 * back.sin());
                painter.add(egui::Shape::line(pts, s));
                painter.line_segment([end, out], s);
                painter.line_segment([end, inn], s);
            }
        }
        Icon::Offline => {
            // A plug pulled out of its socket.
            painter.rect_stroke(Rect::from_min_max(p(r, 0.12, 0.36), p(r, 0.4, 0.64)), 2.0, s, StrokeKind::Middle);
            painter.line_segment([p(r, 0.02, 0.5), p(r, 0.12, 0.5)], s);
            painter.line_segment([p(r, 0.4, 0.43), p(r, 0.5, 0.43)], s);
            painter.line_segment([p(r, 0.4, 0.57), p(r, 0.5, 0.57)], s);
            painter.rect_stroke(Rect::from_min_max(p(r, 0.62, 0.32), p(r, 0.86, 0.68)), 2.0, s, StrokeKind::Middle);
            painter.line_segment([p(r, 0.86, 0.5), p(r, 0.98, 0.5)], s);
        }
    }
}

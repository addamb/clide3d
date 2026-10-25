//! Small vector icons drawn with the egui painter (no image assets).
//!
//! Every icon is drawn on a 16x16 grid scaled into the button rect, so one
//! description works at toolbox (26px), shelf (32px) and outliner (14px) size.

use egui::{Color32, Pos2, Rect, Response, Sense, Shape, Stroke, Ui, Vec2, pos2};

use super::theme;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Select,
    Lasso,
    Paint,
    /// A brush raising a mound (the Sculpt tool).
    Sculpt,
    Move,
    Rotate,
    Scale,
    Cube,
    Sphere,
    Cylinder,
    Cone,
    Plane,
    Torus,
    Duplicate,
    Delete,
    NewFile,
    Open,
    Save,
    Undo,
    Redo,
    ObjectMode,
    VertexMode,
    EdgeMode,
    FaceMode,
    SnapGrid,
    SnapCurve,
    SnapPoint,
    FrameAll,
    FrameSelected,
    Grid,
    Wireframe,
    Shaded,
    WireOnShaded,
    MeshNode,
    CameraNode,
    SetNode,
    SingleView,
    FourView,
    PerspOutliner,
    ToStart,
    StepBack,
    PlayBack,
    Play,
    Pause,
    StepFwd,
    ToEnd,
    ScriptEditor,
}

/// A square icon button. `active` draws Maya's highlighted-tool background.
pub fn button(ui: &mut Ui, icon: Icon, size: f32, active: bool, enabled: bool, tip: &str) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), if enabled { Sense::click() } else { Sense::hover() });
    let painter = ui.painter();
    if active {
        painter.rect_filled(rect, 2.0, theme::SELECT.gamma_multiply(0.8));
    } else if enabled && response.hovered() {
        painter.rect_filled(rect, 2.0, theme::HOVER);
    }
    paint(painter, rect.shrink(size * 0.12), icon, if enabled { 1.0 } else { 0.3 });
    let tip = if enabled { tip.to_string() } else { format!("{tip} (not built yet)") };
    response.on_hover_text(tip)
}

pub fn paint(painter: &egui::Painter, rect: Rect, icon: Icon, alpha: f32) {
    let s = rect.width().min(rect.height()) / 16.0;
    let o = rect.center() - Vec2::splat(8.0 * s);
    let p = |x: f32, y: f32| o + Vec2::new(x * s, y * s);
    let c = |g: u8| Color32::from_gray(g).gamma_multiply(alpha);
    let rgb = |r, g, b| Color32::from_rgb(r, g, b).gamma_multiply(alpha);
    let light = c(225);
    let mid = c(170);
    let dark = c(115);
    let line = |w: f32, col: Color32| Stroke::new(w * s.max(0.75), col);
    let poly = |pts: &[(f32, f32)], fill: Color32| Shape::convex_polygon(pts.iter().map(|&(x, y)| p(x, y)).collect(), fill, Stroke::NONE);
    let path = |pts: &[(f32, f32)], stroke: Stroke| Shape::line(pts.iter().map(|&(x, y)| p(x, y)).collect(), stroke);
    let seg = |a: (f32, f32), b: (f32, f32), stroke: Stroke| Shape::line_segment([p(a.0, a.1), p(b.0, b.1)], stroke);
    let arc = |cx: f32, cy: f32, r: f32, a0: f32, a1: f32| -> Vec<Pos2> {
        (0..=16).map(|i| {
            let a = a0 + (a1 - a0) * i as f32 / 16.0;
            p(cx + r * a.cos(), cy + r * a.sin())
        }).collect()
    };
    let mut shapes: Vec<Shape> = Vec::new();
    let iso_cube = |shapes: &mut Vec<Shape>, scale: f32, cx: f32, cy: f32| {
        let q = |x: f32, y: f32| (cx + x * scale, cy + y * scale);
        shapes.push(poly(&[q(0.0, -6.0), q(6.0, -3.0), q(0.0, 0.0), q(-6.0, -3.0)], light));
        shapes.push(poly(&[q(-6.0, -3.0), q(0.0, 0.0), q(0.0, 7.0), q(-6.0, 4.0)], mid));
        shapes.push(poly(&[q(0.0, 0.0), q(6.0, -3.0), q(6.0, 4.0), q(0.0, 7.0)], dark));
    };
    let tri = |dir: f32, x: f32, fill: Color32| poly(&[(x, 4.0), (x + 6.0 * dir, 8.0), (x, 12.0)], fill);
    let bar = |x: f32| poly(&[(x, 4.0), (x + 1.5, 4.0), (x + 1.5, 12.0), (x, 12.0)], light);

    match icon {
        Icon::Select => shapes.push(poly(&[(4.0, 1.0), (4.0, 13.0), (7.0, 10.0), (12.0, 10.0)], light)),
        Icon::Lasso => {
            shapes.push(Shape::closed_line(arc(8.5, 6.5, 5.5, 0.0, std::f32::consts::TAU), line(1.3, light)));
            shapes.push(poly(&[(3.0, 9.0), (3.0, 15.0), (5.0, 13.0), (7.5, 13.0)], light));
        }
        Icon::Paint => {
            shapes.push(seg((13.0, 2.0), (7.0, 9.0), line(2.0, mid)));
            shapes.push(Shape::circle_filled(p(5.0, 11.5), 3.2 * s, light));
        }
        Icon::Sculpt => {
            shapes.push(Shape::convex_polygon(arc(8.0, 14.0, 6.5, std::f32::consts::PI, std::f32::consts::TAU), mid, Stroke::NONE));
            shapes.push(seg((14.0, 1.5), (9.0, 6.5), line(1.8, light)));
            shapes.push(Shape::circle_filled(p(8.0, 7.5), 1.6 * s, light));
        }
        Icon::Move => {
            shapes.push(seg((8.0, 1.5), (8.0, 14.5), line(1.4, light)));
            shapes.push(seg((1.5, 8.0), (14.5, 8.0), line(1.4, light)));
            for (a, b, cc) in [((8.0, 0.5), (5.5, 4.0), (10.5, 4.0)), ((8.0, 15.5), (5.5, 12.0), (10.5, 12.0)), ((0.5, 8.0), (4.0, 5.5), (4.0, 10.5)), ((15.5, 8.0), (12.0, 5.5), (12.0, 10.5))] {
                shapes.push(poly(&[a, b, cc], light));
            }
        }
        Icon::Rotate => {
            shapes.push(Shape::line(arc(8.0, 8.0, 5.5, -0.6, 4.4), line(1.6, light)));
            shapes.push(poly(&[(13.5, 1.5), (14.0, 6.5), (9.5, 4.5)], light));
        }
        Icon::Scale => {
            shapes.push(Shape::rect_stroke(Rect::from_min_max(p(2.0, 2.0), p(14.0, 14.0)), 0.0, line(1.2, mid), egui::StrokeKind::Inside));
            shapes.push(Shape::rect_filled(Rect::from_min_max(p(2.0, 9.0), p(7.0, 14.0)), 0.0, light));
            shapes.push(seg((6.0, 10.0), (12.5, 3.5), line(1.4, light)));
            shapes.push(poly(&[(13.5, 2.5), (13.5, 7.0), (9.0, 2.5)], light));
        }
        Icon::Cube => iso_cube(&mut shapes, 1.0, 8.0, 7.5),
        Icon::Sphere => {
            shapes.push(Shape::circle_filled(p(8.0, 8.0), 6.5 * s, dark));
            shapes.push(Shape::circle_filled(p(7.2, 7.2), 5.3 * s, mid));
            shapes.push(Shape::circle_filled(p(6.0, 5.8), 2.8 * s, light));
        }
        Icon::Cylinder => {
            shapes.push(poly(&[(3.0, 4.0), (13.0, 4.0), (13.0, 12.5), (3.0, 12.5)], mid));
            shapes.push(Shape::ellipse_filled(p(8.0, 12.5), Vec2::new(5.0 * s, 2.0 * s), mid));
            shapes.push(Shape::ellipse_filled(p(8.0, 4.0), Vec2::new(5.0 * s, 2.0 * s), light));
        }
        Icon::Cone => {
            shapes.push(Shape::ellipse_filled(p(8.0, 12.5), Vec2::new(5.5 * s, 2.0 * s), dark));
            shapes.push(poly(&[(8.0, 1.5), (13.5, 12.5), (2.5, 12.5)], mid));
            shapes.push(poly(&[(8.0, 1.5), (8.0, 14.5), (2.5, 12.5)], light));
        }
        Icon::Plane => shapes.push(poly(&[(1.0, 9.0), (8.0, 5.0), (15.0, 9.0), (8.0, 13.0)], light)),
        Icon::Torus => {
            shapes.push(Shape::ellipse_stroke(p(8.0, 8.5), Vec2::new(5.3 * s, 3.6 * s), line(3.2, mid)));
            shapes.push(Shape::line(arc(8.0, 8.5, 5.3, 3.5, 5.9).iter().map(|q| pos2(q.x, o.y + (q.y - o.y - 8.5 * s) * 0.68 + 8.5 * s - 0.6 * s)).collect(), line(1.0, light)));
        }
        Icon::Duplicate => {
            iso_cube(&mut shapes, 0.6, 6.0, 7.0);
            iso_cube(&mut shapes, 0.6, 10.5, 10.5);
        }
        Icon::Delete => {
            shapes.push(seg((3.5, 3.5), (12.5, 12.5), line(2.2, rgb(220, 80, 70))));
            shapes.push(seg((12.5, 3.5), (3.5, 12.5), line(2.2, rgb(220, 80, 70))));
        }
        Icon::NewFile => {
            shapes.push(poly(&[(3.0, 1.0), (10.0, 1.0), (13.0, 4.0), (13.0, 15.0), (3.0, 15.0)], light));
            shapes.push(poly(&[(10.0, 1.0), (13.0, 4.0), (10.0, 4.0)], dark));
        }
        Icon::Open => {
            shapes.push(poly(&[(1.0, 3.0), (6.0, 3.0), (7.5, 5.0), (14.0, 5.0), (14.0, 13.0), (1.0, 13.0)], rgb(200, 170, 90)));
            shapes.push(poly(&[(3.0, 7.0), (15.5, 7.0), (14.0, 13.0), (1.0, 13.0)], rgb(235, 205, 120)));
        }
        Icon::Save => {
            shapes.push(poly(&[(2.0, 2.0), (12.0, 2.0), (14.0, 4.0), (14.0, 14.0), (2.0, 14.0)], rgb(110, 140, 190)));
            shapes.push(Shape::rect_filled(Rect::from_min_max(p(4.5, 2.0), p(11.0, 6.5)), 0.0, light));
            shapes.push(Shape::rect_filled(Rect::from_min_max(p(4.0, 9.0), p(12.0, 14.0)), 0.0, c(235)));
        }
        Icon::Undo | Icon::Redo => {
            let flip = if icon == Icon::Undo { 1.0 } else { -1.0 };
            let fx = |x: f32| 8.0 + (x - 8.0) * flip;
            let pts: Vec<Pos2> = arc(8.0, 9.0, 4.5, -2.8, 0.9).iter().map(|q| pos2(o.x + fx((q.x - o.x) / s) * s, q.y)).collect();
            shapes.push(Shape::line(pts, line(1.8, light)));
            shapes.push(poly(&[(fx(1.0), 6.5), (fx(6.5), 4.0), (fx(5.5), 9.5)], light));
        }
        Icon::ObjectMode => iso_cube(&mut shapes, 0.85, 8.0, 7.5),
        Icon::VertexMode | Icon::EdgeMode | Icon::FaceMode => {
            let r = Rect::from_min_max(p(3.0, 3.0), p(13.0, 13.0));
            if icon == Icon::FaceMode {
                shapes.push(Shape::rect_filled(r, 0.0, rgb(110, 160, 220)));
            }
            shapes.push(Shape::rect_stroke(r, 0.0, line(1.0, mid), egui::StrokeKind::Middle));
            if icon == Icon::EdgeMode {
                shapes.push(seg((3.0, 3.0), (13.0, 3.0), line(2.4, rgb(110, 160, 220))));
            }
            if icon == Icon::VertexMode {
                for (x, y) in [(3.0, 3.0), (13.0, 3.0), (3.0, 13.0), (13.0, 13.0)] {
                    shapes.push(Shape::circle_filled(p(x, y), 1.8 * s, rgb(140, 100, 220)));
                }
            }
        }
        Icon::SnapGrid | Icon::SnapCurve | Icon::SnapPoint => {
            match icon {
                Icon::SnapGrid => {
                    for i in [3.0, 8.0, 13.0] {
                        shapes.push(seg((i, 2.0), (i, 14.0), line(0.8, mid)));
                        shapes.push(seg((2.0, i), (14.0, i), line(0.8, mid)));
                    }
                }
                Icon::SnapCurve => shapes.push(path(&[(1.0, 13.0), (5.0, 6.0), (10.0, 4.0), (15.0, 9.0)], line(1.4, mid))),
                _ => shapes.push(Shape::circle_filled(p(5.0, 11.0), 2.2 * s, mid)),
            }
            // Magnet.
            shapes.push(Shape::line(arc(11.0, 6.0, 2.8, 0.0, std::f32::consts::PI).into_iter().rev().collect(), line(2.0, rgb(220, 70, 70))));
            shapes.push(seg((8.2, 6.0), (8.2, 3.0), line(2.0, rgb(220, 70, 70))));
            shapes.push(seg((13.8, 6.0), (13.8, 3.0), line(2.0, rgb(220, 70, 70))));
        }
        Icon::FrameAll | Icon::FrameSelected => {
            for (a, b, cc) in [((1.5, 5.0), (1.5, 1.5), (5.0, 1.5)), ((11.0, 1.5), (14.5, 1.5), (14.5, 5.0)), ((14.5, 11.0), (14.5, 14.5), (11.0, 14.5)), ((5.0, 14.5), (1.5, 14.5), (1.5, 11.0))] {
                shapes.push(path(&[a, b, cc], line(1.3, light)));
            }
            if icon == Icon::FrameAll {
                iso_cube(&mut shapes, 0.55, 8.0, 8.0);
            } else {
                shapes.push(Shape::circle_filled(p(8.0, 8.0), 2.5 * s, rgb(90, 220, 150)));
            }
        }
        Icon::Grid => {
            for i in [2.0, 6.0, 10.0, 14.0] {
                shapes.push(seg((i, 2.0), (i, 14.0), line(1.0, mid)));
                shapes.push(seg((2.0, i), (14.0, i), line(1.0, mid)));
            }
        }
        Icon::Wireframe => {
            shapes.push(Shape::circle_stroke(p(8.0, 8.0), 6.0 * s, line(1.1, light)));
            shapes.push(Shape::ellipse_stroke(p(8.0, 8.0), Vec2::new(6.0 * s, 2.2 * s), line(0.9, light)));
            shapes.push(Shape::ellipse_stroke(p(8.0, 8.0), Vec2::new(2.2 * s, 6.0 * s), line(0.9, light)));
        }
        Icon::Shaded | Icon::WireOnShaded => {
            shapes.push(Shape::circle_filled(p(8.0, 8.0), 6.2 * s, dark));
            shapes.push(Shape::circle_filled(p(7.3, 7.3), 5.0 * s, mid));
            shapes.push(Shape::circle_filled(p(6.2, 6.0), 2.4 * s, light));
            if icon == Icon::WireOnShaded {
                shapes.push(Shape::ellipse_stroke(p(8.0, 8.0), Vec2::new(6.2 * s, 2.2 * s), line(0.9, c(30))));
                shapes.push(Shape::ellipse_stroke(p(8.0, 8.0), Vec2::new(2.2 * s, 6.2 * s), line(0.9, c(30))));
            }
        }
        Icon::MeshNode => {
            shapes.push(poly(&[(2.0, 10.0), (8.0, 6.0), (14.0, 10.0), (8.0, 14.0)], rgb(120, 175, 215)));
            shapes.push(seg((5.0, 8.0), (11.0, 12.0), line(0.8, c(40))));
            shapes.push(seg((11.0, 8.0), (5.0, 12.0), line(0.8, c(40))));
        }
        Icon::CameraNode => {
            shapes.push(Shape::rect_filled(Rect::from_min_max(p(1.5, 5.0), p(10.5, 12.0)), 1.0, mid));
            shapes.push(poly(&[(10.5, 8.5), (14.5, 5.5), (14.5, 11.5)], mid));
        }
        Icon::SetNode => {
            shapes.push(Shape::circle_stroke(p(6.0, 8.0), 4.0 * s, line(1.2, mid)));
            shapes.push(Shape::circle_stroke(p(10.0, 8.0), 4.0 * s, line(1.2, mid)));
        }
        Icon::SingleView | Icon::FourView | Icon::PerspOutliner => {
            let r = Rect::from_min_max(p(1.5, 2.5), p(14.5, 13.5));
            shapes.push(Shape::rect_filled(r, 0.0, c(150)));
            shapes.push(Shape::rect_stroke(r, 0.0, line(1.0, light), egui::StrokeKind::Middle));
            if icon == Icon::FourView {
                shapes.push(seg((8.0, 2.5), (8.0, 13.5), line(1.0, c(60))));
                shapes.push(seg((1.5, 8.0), (14.5, 8.0), line(1.0, c(60))));
            }
            if icon == Icon::PerspOutliner {
                shapes.push(Shape::rect_filled(Rect::from_min_max(p(1.5, 2.5), p(6.0, 13.5)), 0.0, c(95)));
            }
        }
        Icon::ToStart => {
            shapes.push(bar(2.0));
            shapes.push(tri(-1.0, 9.5, light));
            shapes.push(tri(-1.0, 15.0, light));
        }
        Icon::StepBack => {
            shapes.push(tri(-1.0, 11.0, light));
            shapes.push(bar(11.5));
        }
        Icon::PlayBack => shapes.push(tri(-1.0, 11.0, light)),
        Icon::Play => shapes.push(tri(1.0, 5.0, light)),
        Icon::Pause => {
            shapes.push(bar(5.0));
            shapes.push(bar(9.5));
        }
        Icon::StepFwd => {
            shapes.push(bar(3.0));
            shapes.push(tri(1.0, 5.0, light));
        }
        Icon::ToEnd => {
            shapes.push(tri(1.0, 1.0, light));
            shapes.push(tri(1.0, 6.5, light));
            shapes.push(bar(12.5));
        }
        Icon::ScriptEditor => {
            shapes.push(Shape::rect_filled(Rect::from_min_max(p(2.0, 2.0), p(14.0, 14.0)), 1.0, c(40)));
            shapes.push(path(&[(4.5, 5.5), (7.0, 8.0), (4.5, 10.5)], line(1.3, light)));
            shapes.push(seg((8.0, 11.0), (11.5, 11.0), line(1.3, light)));
        }
    }
    painter.extend(shapes);
}

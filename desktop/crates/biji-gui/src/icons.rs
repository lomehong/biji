//! 自绘线性图标——对位原客户端的矢量线性图标集。全部用 egui Painter
//! 画线/画形（不依赖图标字体），绘制函数统一入参：画笔、目标矩形、颜色。
//! 图标本身不持色：颜色由调用方传入（导航行随选中态、工具钮随悬停态）。

use egui::{Color32, Painter, Rect, Sense, Stroke, Vec2};

use crate::theme;

/// 画一个图标（rect 内居中、按短边等比缩放）。
pub type PaintFn = dyn Fn(&Painter, Rect, Color32);

/// 线宽基准（14px 图标下 ~1.6px，随尺寸轻微缩放）。
fn sw(r: Rect) -> f32 {
    (r.width().min(r.height()) / 14.0).clamp(1.2, 2.4)
}

fn lerp(a: f32, t: f32, b: f32) -> f32 {
    a + (b - a) * t
}

// ---------- 品牌动作 ----------

/// 绿圆白＋（新建笔记主按钮）。
pub fn plus_circle(p: &Painter, r: Rect, fill: Color32, plus: Color32) {
    let c = r.center();
    let rad = r.width().min(r.height()) * 0.5;
    p.circle_filled(c, rad, fill);
    let a = rad * 0.42;
    let s = Stroke::new(rad * 0.16, plus);
    p.line_segment([egui::pos2(c.x - a, c.y), egui::pos2(c.x + a, c.y)], s);
    p.line_segment([egui::pos2(c.x, c.y - a), egui::pos2(c.x, c.y + a)], s);
}

/// 文档 + 右下角小＋（新建普通笔记）。
pub fn doc_plus(p: &Painter, r: Rect, c: Color32) {
    doc(
        p,
        Rect::from_min_size(r.min, Vec2::splat(r.height() * 0.80)),
        c,
    );
    // 实心圆徽 + 白色加号（对位官方精致小徽）
    let d = r.height() * 0.42;
    let cc = egui::pos2(lerp(r.min.x, 0.66, r.max.x), lerp(r.min.y, 0.66, r.max.y));
    p.circle_filled(cc, d * 0.5, c);
    let a = d * 0.24;
    let w = Stroke::new(sw(r) * 1.15, Color32::WHITE);
    p.line_segment([egui::pos2(cc.x - a, cc.y), egui::pos2(cc.x + a, cc.y)], w);
    p.line_segment([egui::pos2(cc.x, cc.y - a), egui::pos2(cc.x, cc.y + a)], w);
}

// ---------- 导航 ----------

/// 文档（全部笔记/普通行）。
pub fn doc(p: &Painter, r: Rect, c: Color32) {
    let s = Stroke::new(sw(r), c);
    // 页身（右上折角）
    let fold = r.width() * 0.32;
    let pts = [
        egui::pos2(r.min.x, r.min.y),
        egui::pos2(r.max.x - fold, r.min.y),
        egui::pos2(r.max.x, r.min.y + fold),
        egui::pos2(r.max.x, r.max.y),
        egui::pos2(r.min.x, r.max.y),
    ];
    p.add(egui::Shape::line(pts.to_vec(), s));
    p.line_segment(
        [
            egui::pos2(r.max.x - fold, r.min.y),
            egui::pos2(r.max.x - fold, r.min.y + fold),
        ],
        s,
    );
    p.line_segment(
        [
            egui::pos2(r.max.x - fold, r.min.y + fold),
            egui::pos2(r.max.x, r.min.y + fold),
        ],
        s,
    );
    // 文本行
    for t in [0.42, 0.62] {
        let y = lerp(r.min.y, t, r.max.y);
        p.line_segment(
            [
                egui::pos2(r.min.x + r.width() * 0.18, y),
                egui::pos2(r.max.x - r.width() * 0.18, y),
            ],
            s,
        );
    }
}

/// 笔记本（侧栏 笔记本/元信息 chip）。
pub fn book(p: &Painter, r: Rect, c: Color32) {
    let s = Stroke::new(sw(r), c);
    p.rect_stroke(r, egui::Rounding::same(2.0), s);
    let spine_x = r.min.x + r.width() * 0.26;
    p.line_segment(
        [egui::pos2(spine_x, r.min.y), egui::pos2(spine_x, r.max.y)],
        s,
    );
    for t in [0.36, 0.62] {
        let y = lerp(r.min.y, t, r.max.y);
        p.line_segment(
            [
                egui::pos2(spine_x + r.width() * 0.14, y),
                egui::pos2(r.max.x - r.width() * 0.14, y),
            ],
            s,
        );
    }
}

/// 五角星（快捷方式）。
pub fn star(p: &Painter, r: Rect, c: Color32) {
    let cc = r.center();
    let ro = r.width().min(r.height()) * 0.5;
    let ri = ro * 0.42;
    let mut pts = Vec::with_capacity(10);
    for i in 0..10 {
        let ang = -std::f32::consts::FRAC_PI_2 + i as f32 * std::f32::consts::PI / 5.0;
        let rad = if i % 2 == 0 { ro } else { ri };
        pts.push(egui::pos2(cc.x + rad * ang.cos(), cc.y + rad * ang.sin()));
    }
    p.add(egui::Shape::line(pts, Stroke::new(sw(r), c)));
}

/// 标签（标签区/卡内标签）。
pub fn tag(p: &Painter, r: Rect, c: Color32) {
    let s = Stroke::new(sw(r), c);
    let w = r.width();
    let h = r.height();
    let pts = [
        egui::pos2(r.min.x, r.min.y + h * 0.06),
        egui::pos2(r.min.x + w * 0.52, r.min.y),
        egui::pos2(r.max.x, r.min.y + h * 0.5),
        egui::pos2(r.min.x + w * 0.52, r.max.y),
        egui::pos2(r.min.x, r.max.y - h * 0.06),
    ];
    p.add(egui::Shape::line(pts.to_vec(), s));
    p.circle_filled(
        egui::pos2(r.min.x + w * 0.24, r.min.y + h * 0.28),
        sw(r) * 0.9,
        c,
    );
}

/// 回收站。
pub fn trash(p: &Painter, r: Rect, c: Color32) {
    let s = Stroke::new(sw(r) * 0.9, c);
    let lid_y = r.min.y + r.height() * 0.22;
    p.line_segment([egui::pos2(r.min.x, lid_y), egui::pos2(r.max.x, lid_y)], s);
    p.line_segment(
        [
            egui::pos2(r.center().x - r.width() * 0.12, r.min.y),
            egui::pos2(r.center().x + r.width() * 0.12, r.min.y),
        ],
        s,
    );
    // 桶身（上宽下窄）
    let inset = r.width() * 0.14;
    p.line_segment(
        [
            egui::pos2(r.min.x + inset, lid_y),
            egui::pos2(r.min.x + inset * 1.8, r.max.y),
        ],
        s,
    );
    p.line_segment(
        [
            egui::pos2(r.max.x - inset, lid_y),
            egui::pos2(r.max.x - inset * 1.8, r.max.y),
        ],
        s,
    );
    for t in [0.42, 0.58] {
        let x_top = lerp(r.min.x + inset, t, r.max.x - inset);
        let x_bot = lerp(r.min.x + inset * 1.8, t, r.max.x - inset * 1.8);
        p.line_segment(
            [
                egui::pos2(x_top, lid_y + r.height() * 0.10),
                egui::pos2(x_bot, r.max.y - r.height() * 0.06),
            ],
            s,
        );
    }
    p.line_segment(
        [
            egui::pos2(r.min.x + inset * 1.8, r.max.y),
            egui::pos2(r.max.x - inset * 1.8, r.max.y),
        ],
        s,
    );
    {
        let x = lerp(r.min.x, 0.42, r.max.x);
        p.line_segment(
            [
                egui::pos2(x, lid_y + r.height() * 0.12),
                egui::pos2(x + r.width() * 0.16, r.max.y - r.height() * 0.08),
            ],
            s,
        );
    }
}

/// 企业空间（楼）。
pub fn corp(p: &Painter, r: Rect, c: Color32) {
    let s = Stroke::new(sw(r), c);
    p.rect_stroke(
        Rect::from_min_size(
            egui::pos2(r.min.x + r.width() * 0.12, r.min.y + r.height() * 0.15),
            Vec2::new(r.width() * 0.55, r.height() * 0.85),
        ),
        egui::Rounding::same(1.0),
        s,
    );
    p.add(egui::Shape::line(
        vec![
            egui::pos2(r.min.x + r.width() * 0.67, r.max.y - r.height() * 0.32),
            egui::pos2(r.max.x - r.width() * 0.1, r.max.y - r.height() * 0.32),
            egui::pos2(r.max.x - r.width() * 0.1, r.max.y),
            egui::pos2(r.min.x + r.width() * 0.12, r.max.y),
        ],
        s,
    ));
    for (tx, ty) in [(0.3, 0.34), (0.3, 0.56), (0.52, 0.34)] {
        p.circle_filled(
            egui::pos2(lerp(r.min.x, tx, r.max.x), lerp(r.min.y, ty, r.max.y)),
            sw(r) * 0.8,
            c,
        );
    }
}

/// AI 闪光（AI 助理）。
pub fn sparkle(p: &Painter, r: Rect, c: Color32) {
    // 实心四角星（主星 + 两颗辅星），对位官方 AI 星芒
    let four = |cc: egui::Pos2, ro: f32, ri: f32| {
        let mut pts = Vec::with_capacity(8);
        for i in 0..8 {
            let ang = -std::f32::consts::FRAC_PI_2 + i as f32 * std::f32::consts::PI / 4.0;
            let rad = if i % 2 == 0 { ro } else { ri };
            pts.push(egui::pos2(cc.x + rad * ang.cos(), cc.y + rad * ang.sin()));
        }
        p.add(egui::Shape::Path(egui::epaint::PathShape {
            points: pts,
            closed: true,
            fill: c,
            stroke: egui::Stroke::NONE.into(),
        }));
    };
    let main_c = egui::pos2(lerp(r.min.x, 0.42, r.max.x), r.center().y);
    let ro = r.height() * 0.34;
    four(main_c, ro, ro * 0.30);
    four(
        egui::pos2(lerp(r.min.x, 0.80, r.max.x), lerp(r.min.y, 0.22, r.max.y)),
        ro * 0.48,
        ro * 0.16,
    );
    four(
        egui::pos2(lerp(r.min.x, 0.78, r.max.x), lerp(r.min.y, 0.80, r.max.y)),
        ro * 0.38,
        ro * 0.13,
    );
}

// ---------- 列表列 ----------

/// 放大镜（搜索）。
pub fn search(p: &Painter, r: Rect, c: Color32) {
    let s = Stroke::new(sw(r) * 1.1, c);
    let cr = r.height() * 0.30;
    let cc = egui::pos2(lerp(r.min.x, 0.40, r.max.x), lerp(r.min.y, 0.40, r.max.y));
    p.circle_stroke(cc, cr, s);
    p.line_segment(
        [
            egui::pos2(cc.x + cr * 0.72, cc.y + cr * 0.72),
            egui::pos2(r.max.x - r.width() * 0.08, r.max.y - r.height() * 0.08),
        ],
        s,
    );
}

/// 排序（下箭头 + 两短横）。
pub fn sort(p: &Painter, r: Rect, c: Color32) {
    let s = Stroke::new(sw(r) * 1.1, c);
    let ax = r.min.x + r.width() * 0.3;
    let top = r.min.y + r.height() * 0.12;
    let bot = r.max.y - r.height() * 0.12;
    p.line_segment([egui::pos2(ax, top), egui::pos2(ax, bot)], s);
    p.add(egui::Shape::line(
        vec![
            egui::pos2(ax - r.width() * 0.16, bot - r.height() * 0.18),
            egui::pos2(ax, bot),
            egui::pos2(ax + r.width() * 0.16, bot - r.height() * 0.18),
        ],
        s,
    ));
    for t in [0.3, 0.58] {
        let y = lerp(r.min.y, t, r.max.y);
        p.line_segment(
            [
                egui::pos2(lerp(r.min.x, 0.52, r.max.x), y),
                egui::pos2(r.max.x, y),
            ],
            s,
        );
    }
}

/// 卡片视图（两小卡）。
pub fn cards(p: &Painter, r: Rect, c: Color32) {
    let s = Stroke::new(sw(r), c);
    let gap = r.height() * 0.12;
    let h = (r.height() - gap) * 0.5;
    let w = r.width() * 0.72;
    for (i, y) in [(0, r.min.y), (1, r.min.y + h + gap)] {
        let x = if i == 0 {
            r.min.x
        } else {
            r.min.x + r.width() * 0.18
        };
        p.rect_stroke(
            Rect::from_min_size(egui::pos2(x, y), Vec2::new(w, h)),
            egui::Rounding::same(2.0),
            s,
        );
    }
}

/// 列表视图（三行点线）。
pub fn list(p: &Painter, r: Rect, c: Color32) {
    let s = Stroke::new(sw(r), c);
    for t in [0.2, 0.5, 0.8] {
        let y = lerp(r.min.y, t, r.max.y);
        p.circle_filled(egui::pos2(r.min.x + r.width() * 0.08, y), sw(r) * 0.8, c);
        p.line_segment(
            [
                egui::pos2(r.min.x + r.width() * 0.28, y),
                egui::pos2(r.max.x, y),
            ],
            s,
        );
    }
}

/// 省略号（更多）。
pub fn dots(p: &Painter, r: Rect, c: Color32) {
    let rad = sw(r) * 1.1;
    for t in [0.2, 0.5, 0.8] {
        p.circle_filled(egui::pos2(lerp(r.min.x, t, r.max.x), r.center().y), rad, c);
    }
}

// ---------- 编辑器工具行 ----------

/// 回形针（附件）。
pub fn clip(p: &Painter, r: Rect, c: Color32) {
    let s = Stroke::new(sw(r) * 1.1, c);
    let x0 = r.min.x + r.width() * 0.34;
    let x1 = r.min.x + r.width() * 0.66;
    p.add(egui::Shape::line(
        vec![
            egui::pos2(x1, r.min.y + r.height() * 0.18),
            egui::pos2(x1, r.max.y - r.height() * 0.24),
            egui::pos2(r.max.x - r.width() * 0.1, r.max.y - r.height() * 0.1),
            egui::pos2(r.min.x + r.width() * 0.1, r.max.y - r.height() * 0.1),
            egui::pos2(r.min.x + r.width() * 0.1, r.min.y + r.height() * 0.24),
            egui::pos2(x0, r.min.y + r.height() * 0.12),
            egui::pos2(x0, r.max.y - r.height() * 0.36),
        ],
        s,
    ));
}

/// 分享（三节点连线）。
/// 上一代顶栏/编辑器钮图标，官方钮组改排后暂留备用。
#[allow(dead_code)]
pub fn share(p: &Painter, r: Rect, c: Color32) {
    let s = Stroke::new(sw(r), c);
    let n1 = egui::pos2(lerp(r.min.x, 0.18, r.max.x), r.center().y);
    let n2 = egui::pos2(lerp(r.min.x, 0.8, r.max.x), lerp(r.min.y, 0.16, r.max.y));
    let n3 = egui::pos2(lerp(r.min.x, 0.8, r.max.x), lerp(r.min.y, 0.84, r.max.y));
    let rad = sw(r) * 1.7;
    p.line_segment([n1, n2], s);
    p.line_segment([n1, n3], s);
    for n in [n1, n2, n3] {
        p.circle_filled(n, rad, c);
    }
}

/// 人形＋（共享给联系人）。
pub fn person_plus(p: &Painter, r: Rect, c: Color32) {
    let s = Stroke::new(sw(r), c);
    let head_c = egui::pos2(lerp(r.min.x, 0.4, r.max.x), lerp(r.min.y, 0.26, r.max.y));
    p.circle_stroke(head_c, r.height() * 0.2, s);
    p.add(egui::Shape::line(
        vec![
            egui::pos2(r.min.x + r.width() * 0.1, r.max.y - r.height() * 0.1),
            egui::pos2(r.min.x + r.width() * 0.1, lerp(r.min.y, 0.68, r.max.y)),
            egui::pos2(lerp(r.min.x, 0.7, r.max.x), lerp(r.min.y, 0.68, r.max.y)),
        ],
        s,
    ));
    let pc = egui::pos2(lerp(r.min.x, 0.84, r.max.x), lerp(r.min.y, 0.72, r.max.y));
    let a = r.width() * 0.14;
    let s2 = Stroke::new(sw(r) * 1.1, c);
    p.line_segment([egui::pos2(pc.x - a, pc.y), egui::pos2(pc.x + a, pc.y)], s2);
    p.line_segment([egui::pos2(pc.x, pc.y - a), egui::pos2(pc.x, pc.y + a)], s2);
}

/// 时钟（历史版本）。
pub fn clock(p: &Painter, r: Rect, c: Color32) {
    let s = Stroke::new(sw(r), c);
    let cc = r.center();
    p.circle_stroke(cc, r.height() * 0.44, s);
    p.line_segment([cc, egui::pos2(cc.x, cc.y - r.height() * 0.26)], s);
    p.line_segment(
        [
            cc,
            egui::pos2(cc.x + r.width() * 0.2, cc.y + r.height() * 0.1),
        ],
        s,
    );
}

/// 球（已共享指示）。
pub fn globe(p: &Painter, r: Rect, c: Color32) {
    let s = Stroke::new(sw(r), c);
    let cc = r.center();
    p.circle_stroke(cc, r.height() * 0.44, s);
    p.line_segment([egui::pos2(r.min.x, cc.y), egui::pos2(r.max.x, cc.y)], s);
    p.add(egui::Shape::line(
        vec![
            egui::pos2(cc.x, r.min.y),
            egui::pos2(cc.x + r.width() * 0.22, cc.y),
            egui::pos2(cc.x, r.max.y),
            egui::pos2(cc.x - r.width() * 0.22, cc.y),
            egui::pos2(cc.x, r.min.y),
        ],
        s,
    ));
}

/// 信息（笔记信息）。
/// 上一代顶栏/编辑器钮图标，官方钮组改排后暂留备用。
#[allow(dead_code)]
pub fn info(p: &Painter, r: Rect, c: Color32) {
    let s = Stroke::new(sw(r), c);
    let cc = r.center();
    p.circle_stroke(cc, r.height() * 0.44, s);
    p.circle_filled(
        egui::pos2(cc.x, lerp(r.min.y, 0.3, r.max.y)),
        sw(r) * 0.9,
        c,
    );
    p.line_segment(
        [
            egui::pos2(cc.x, lerp(r.min.y, 0.5, r.max.y)),
            egui::pos2(cc.x, lerp(r.min.y, 0.78, r.max.y)),
        ],
        Stroke::new(sw(r) * 1.2, c),
    );
}

// ---------- 箭头 ----------

/// 折叠箭头：down=true 画 ▾，否则画 ›。
pub fn chevron(p: &Painter, r: Rect, c: Color32, down: bool) {
    let s = Stroke::new(sw(r) * 1.2, c);
    let (a, b, d) = if down {
        (
            egui::pos2(lerp(r.min.x, 0.12, r.max.x), lerp(r.min.y, 0.28, r.max.y)),
            egui::pos2(r.center().x, lerp(r.min.y, 0.75, r.max.y)),
            egui::pos2(lerp(r.min.x, 0.88, r.max.x), lerp(r.min.y, 0.28, r.max.y)),
        )
    } else {
        (
            egui::pos2(lerp(r.min.x, 0.28, r.max.x), lerp(r.min.y, 0.12, r.max.y)),
            egui::pos2(lerp(r.min.x, 0.75, r.max.x), r.center().y),
            egui::pos2(lerp(r.min.x, 0.28, r.max.x), lerp(r.min.y, 0.88, r.max.y)),
        )
    };
    p.add(egui::Shape::line(vec![a, b, d], s));
}

// ---------- 格式栏 / 同步 ----------

/// 荧光笔（高亮）。
pub fn highlight(p: &Painter, r: Rect, c: Color32) {
    let s = Stroke::new(sw(r) * 1.1, c);
    // 斜置笔杆 + 笔头粗线
    p.add(egui::Shape::line(
        vec![
            egui::pos2(lerp(r.min.x, 0.2, r.max.x), lerp(r.min.y, 0.86, r.max.y)),
            egui::pos2(lerp(r.min.x, 0.62, r.max.x), lerp(r.min.y, 0.14, r.max.y)),
            egui::pos2(lerp(r.min.x, 0.86, r.max.x), lerp(r.min.y, 0.38, r.max.y)),
            egui::pos2(lerp(r.min.x, 0.44, r.max.x), lerp(r.min.y, 0.9, r.max.y)),
        ],
        s,
    ));
    p.line_segment(
        [
            egui::pos2(lerp(r.min.x, 0.14, r.max.x), lerp(r.min.y, 0.62, r.max.y)),
            egui::pos2(lerp(r.min.x, 0.5, r.max.x), lerp(r.min.y, 0.98, r.max.y)),
        ],
        Stroke::new(sw(r) * 2.0, c),
    );
}

/// 行内代码（尖括号斜杠）。
pub fn code(p: &Painter, r: Rect, c: Color32) {
    let s = Stroke::new(sw(r) * 1.2, c);
    let lt = |dir: f32| {
        let x0 = if dir > 0.0 {
            lerp(r.min.x, 0.1, r.max.x)
        } else {
            lerp(r.min.x, 0.9, r.max.x)
        };
        let x1 = if dir > 0.0 {
            lerp(r.min.x, 0.4, r.max.x)
        } else {
            lerp(r.min.x, 0.6, r.max.x)
        };
        p.add(egui::Shape::line(
            vec![
                egui::pos2(x0, lerp(r.min.y, 0.25, r.max.y)),
                egui::pos2(x1, r.center().y),
                egui::pos2(x0, lerp(r.min.y, 0.75, r.max.y)),
            ],
            s,
        ));
    };
    lt(1.0);
    lt(-1.0);
    p.line_segment(
        [
            egui::pos2(lerp(r.min.x, 0.56, r.max.x), lerp(r.min.y, 0.18, r.max.y)),
            egui::pos2(lerp(r.min.x, 0.44, r.max.x), lerp(r.min.y, 0.82, r.max.y)),
        ],
        s,
    );
}

/// 表格（田字网格）。
pub fn table(p: &Painter, r: Rect, c: Color32) {
    let s = Stroke::new(sw(r), c);
    p.rect_stroke(r, egui::Rounding::same(2.0), s);
    let mid_x = r.center().x;
    let mid_y = r.center().y;
    p.line_segment([egui::pos2(mid_x, r.min.y), egui::pos2(mid_x, r.max.y)], s);
    p.line_segment([egui::pos2(r.min.x, mid_y), egui::pos2(r.max.x, mid_y)], s);
}

/// 水平分割线（上下短线夹一条中线）。
pub fn hr_line(p: &Painter, r: Rect, c: Color32) {
    let s = Stroke::new(sw(r) * 1.2, c);
    let faint = Stroke::new(sw(r), c.gamma_multiply(0.45));
    p.line_segment(
        [
            egui::pos2(r.min.x, lerp(r.min.y, 0.22, r.max.y)),
            egui::pos2(r.max.x, lerp(r.min.y, 0.22, r.max.y)),
        ],
        faint,
    );
    p.line_segment(
        [
            egui::pos2(r.min.x, r.center().y),
            egui::pos2(r.max.x, r.center().y),
        ],
        s,
    );
    p.line_segment(
        [
            egui::pos2(r.min.x, lerp(r.min.y, 0.78, r.max.y)),
            egui::pos2(r.max.x, lerp(r.min.y, 0.78, r.max.y)),
        ],
        faint,
    );
}

/// 待办勾选框。
pub fn todo(p: &Painter, r: Rect, c: Color32) {
    p.rect_stroke(r, egui::Rounding::same(2.0), Stroke::new(sw(r), c));
    p.add(egui::Shape::line(
        vec![
            egui::pos2(lerp(r.min.x, 0.22, r.max.x), r.center().y),
            egui::pos2(lerp(r.min.x, 0.42, r.max.x), lerp(r.min.y, 0.78, r.max.y)),
            egui::pos2(lerp(r.min.x, 0.8, r.max.x), lerp(r.min.y, 0.22, r.max.y)),
        ],
        Stroke::new(sw(r) * 1.2, c),
    ));
}

/// 链条（两节环扣）。
pub fn link(p: &Painter, r: Rect, c: Color32) {
    let s = Stroke::new(sw(r) * 1.2, c);
    let w = r.width();
    let h = r.height();
    // 左右两个斜置圆角矩形环，中间相扣
    let link_rect = |cx: f32, cy: f32| {
        let hw = w * 0.3;
        let hh = h * 0.16;
        let pts_off = [
            (-hw * 0.5, -hh),
            (hw * 0.5, -hh),
            (hw * 0.5, hh),
            (-hw * 0.5, hh),
        ];
        let rot = std::f32::consts::FRAC_PI_4;
        let pts: Vec<egui::Pos2> = pts_off
            .iter()
            .map(|(dx, dy)| {
                egui::pos2(
                    cx + dx * rot.cos() - dy * rot.sin(),
                    cy + dx * rot.sin() + dy * rot.cos(),
                )
            })
            .collect();
        p.add(egui::Shape::line(pts, s));
    };
    link_rect(lerp(r.min.x, 0.3, r.max.x), lerp(r.min.y, 0.62, r.max.y));
    link_rect(lerp(r.min.x, 0.7, r.max.x), lerp(r.min.y, 0.38, r.max.y));
}

/// 同步箭头（双弧循环）。
pub fn sync_arrows(p: &Painter, r: Rect, c: Color32) {
    let s = Stroke::new((r.width() / 9.0).clamp(1.4, 2.6), c);
    let cc = r.center();
    let rad = r.height() * 0.36;
    for (a0, a1) in [
        (-0.3, 2.4),
        (std::f32::consts::PI - 0.3, std::f32::consts::PI + 2.4),
    ] {
        let pts: Vec<egui::Pos2> = (0..=10)
            .map(|i| {
                let a = a0 + (a1 - a0) * i as f32 / 10.0;
                egui::pos2(cc.x + rad * a.cos(), cc.y + rad * a.sin())
            })
            .collect();
        p.add(egui::Shape::line(pts.clone(), s));
        // 箭头（弧末端）
        let end = *pts.last().unwrap();
        let prev = pts[pts.len() - 2];
        let dir = end - prev;
        let n = egui::vec2(-dir.y, dir.x).normalized();
        let ah = rad * 0.34;
        p.add(egui::Shape::convex_polygon(
            vec![
                end + dir.normalized() * ah,
                end - dir.normalized() * ah * 0.4 + n * ah * 0.7,
                end - dir.normalized() * ah * 0.4 - n * ah * 0.7,
            ],
            c,
            egui::Stroke::NONE,
        ));
    }
}

/// 太阳（浅色模式标记：圆 + 八向短芒）。
/// 顶栏日月钮收编进 ⋯ 弹层后暂缺调用（主题弹层复用留位）。
#[allow(dead_code)]
pub fn sun(p: &Painter, r: Rect, c: Color32) {
    let cc = r.center();
    let rad = r.height() * 0.24;
    p.circle_filled(cc, rad, c);
    let s = Stroke::new(sw(r) * 1.1, c);
    for i in 0..8 {
        let a = i as f32 * std::f32::consts::FRAC_PI_4;
        let (dx, dy) = (a.cos(), a.sin());
        let r0 = rad * 1.7;
        let r1 = rad * 2.5;
        p.line_segment(
            [
                egui::pos2(cc.x + dx * r0, cc.y + dy * r0),
                egui::pos2(cc.x + dx * r1, cc.y + dy * r1),
            ],
            s,
        );
    }
}

/// 月亮（深色模式标记：圆盘被背景色圆切出月牙；carve 传所在底色）。
/// 同 sun。
#[allow(dead_code)]
pub fn moon(p: &Painter, r: Rect, c: Color32, carve: Color32) {
    let cc = r.center();
    let rad = r.height() * 0.38;
    p.circle_filled(cc, rad, c);
    p.circle_filled(
        egui::pos2(cc.x + rad * 0.62, cc.y - rad * 0.42),
        rad * 0.92,
        carve,
    );
}

/// 手机（线性竖机 + 底部横杠）。
/// 上一代顶栏/编辑器钮图标，官方钮组改排后暂留备用。
#[allow(dead_code)]
pub fn phone(p: &Painter, r: Rect, c: Color32) {
    let s = Stroke::new(sw(r), c);
    let body = Rect::from_center_size(r.center(), egui::vec2(r.width() * 0.62, r.height() * 0.92));
    p.rect_stroke(body, egui::Rounding::same(3.0), s);
    p.line_segment(
        [
            egui::pos2(
                body.center().x - body.width() * 0.14,
                body.max.y - body.height() * 0.1,
            ),
            egui::pos2(
                body.center().x + body.width() * 0.14,
                body.max.y - body.height() * 0.1,
            ),
        ],
        s,
    );
}

/// 水平渐变圆角胶囊（四角以底色圆盖出圆角；bg 传所在面板底色）。
/// 上一代顶栏/编辑器钮图标，官方钮组改排后暂留备用。
#[allow(dead_code)]
pub fn gradient_pill(p: &Painter, r: Rect, left: Color32, right: Color32, bg: Color32) {
    // 内部渐变（24 根竖条线性插值；无纹理依赖）
    let steps = 24usize;
    let w = r.width() / steps as f32;
    for i in 0..steps {
        let t = i as f32 / (steps - 1) as f32;
        let c = Color32::from_rgb(
            lerp(left.r() as f32, t, right.r() as f32) as u8,
            lerp(left.g() as f32, t, right.g() as f32) as u8,
            lerp(left.b() as f32, t, right.b() as f32) as u8,
        );
        let x0 = r.min.x + w * i as f32;
        p.rect_filled(
            Rect::from_min_size(egui::pos2(x0, r.min.y), egui::vec2(w + 0.6, r.height())),
            0.0,
            c,
        );
    }
    // 四角用底色圆盖出圆角（rounding = 半高）
    let rad = r.height() * 0.5;
    for cc in [
        r.left_top(),
        r.right_top(),
        r.left_bottom(),
        r.right_bottom(),
    ] {
        p.circle_filled(cc, rad + 0.5, bg);
    }
}

/// 加号。
/// 菜单改纯文字行后暂由侧栏扩展预留。
#[allow(dead_code)]
pub fn plus(p: &Painter, r: Rect, c: Color32) {
    let s = Stroke::new(sw(r) * 1.3, c);
    let cc = r.center();
    let a = r.width() * 0.36;
    p.line_segment([egui::pos2(cc.x - a, cc.y), egui::pos2(cc.x + a, cc.y)], s);
    p.line_segment([egui::pos2(cc.x, cc.y - a), egui::pos2(cc.x, cc.y + a)], s);
}

/// 铅笔（编辑）。
pub fn pencil(p: &Painter, r: Rect, c: Color32) {
    let s = Stroke::new(sw(r) * 1.2, c);
    p.add(egui::Shape::line(
        vec![
            egui::pos2(lerp(r.min.x, 0.14, r.max.x), lerp(r.min.y, 0.86, r.max.y)),
            egui::pos2(lerp(r.min.x, 0.78, r.max.x), lerp(r.min.y, 0.22, r.max.y)),
            egui::pos2(lerp(r.min.x, 0.94, r.max.x), lerp(r.min.y, 0.38, r.max.y)),
            egui::pos2(lerp(r.min.x, 0.3, r.max.x), lerp(r.min.y, 0.94, r.max.y)),
            egui::pos2(lerp(r.min.x, 0.14, r.max.x), lerp(r.min.y, 0.86, r.max.y)),
        ],
        s,
    ));
}

/// 眼睛（预览）。
pub fn eye(p: &Painter, r: Rect, c: Color32) {
    let s = Stroke::new(sw(r) * 1.1, c);
    let cy = r.center().y;
    p.add(egui::Shape::line(
        vec![
            egui::pos2(r.min.x, cy),
            egui::pos2(lerp(r.min.x, 0.3, r.max.x), lerp(r.min.y, 0.24, r.max.y)),
            egui::pos2(lerp(r.min.x, 0.7, r.max.x), lerp(r.min.y, 0.24, r.max.y)),
            egui::pos2(r.max.x, cy),
            egui::pos2(lerp(r.min.x, 0.7, r.max.x), lerp(r.min.y, 0.76, r.max.y)),
            egui::pos2(lerp(r.min.x, 0.3, r.max.x), lerp(r.min.y, 0.76, r.max.y)),
            egui::pos2(r.min.x, cy),
        ],
        s,
    ));
    p.circle_stroke(r.center(), r.height() * 0.16, s);
}

/// 播放（放映）。
pub fn play(p: &Painter, r: Rect, c: Color32) {
    let pts = vec![
        egui::pos2(lerp(r.min.x, 0.26, r.max.x), lerp(r.min.y, 0.16, r.max.y)),
        egui::pos2(lerp(r.min.x, 0.26, r.max.x), lerp(r.min.y, 0.84, r.max.y)),
        egui::pos2(lerp(r.min.x, 0.84, r.max.x), r.center().y),
    ];
    p.add(egui::Shape::convex_polygon(pts, c, egui::Stroke::NONE));
}

/// 打印机（导出 PDF）。
pub fn printer(p: &Painter, r: Rect, c: Color32) {
    let s = Stroke::new(sw(r), c);
    // 出纸
    p.rect_stroke(
        Rect::from_min_size(
            egui::pos2(lerp(r.min.x, 0.26, r.max.x), lerp(r.min.y, 0.1, r.max.y)),
            egui::vec2(r.width() * 0.48, r.height() * 0.26),
        ),
        egui::Rounding::same(1.0),
        s,
    );
    // 机身
    p.rect_stroke(
        Rect::from_min_size(
            egui::pos2(r.min.x, lerp(r.min.y, 0.3, r.max.y)),
            egui::vec2(r.width(), r.height() * 0.34),
        ),
        egui::Rounding::same(2.0),
        s,
    );
    // 纸张出口
    p.rect_stroke(
        Rect::from_min_size(
            egui::pos2(lerp(r.min.x, 0.26, r.max.x), lerp(r.min.y, 0.6, r.max.y)),
            egui::vec2(r.width() * 0.48, r.height() * 0.32),
        ),
        egui::Rounding::same(1.0),
        s,
    );
    p.circle_filled(
        egui::pos2(lerp(r.min.x, 0.86, r.max.x), lerp(r.min.y, 0.42, r.max.y)),
        sw(r) * 0.9,
        c,
    );
}

/// 对话气泡（聊天）。
pub fn chat(p: &Painter, r: Rect, c: Color32) {
    let s = Stroke::new(sw(r), c);
    let body = Rect::from_min_size(
        egui::pos2(r.min.x, r.min.y),
        egui::vec2(r.width(), r.height() * 0.72),
    );
    p.rect_stroke(body, egui::Rounding::same(3.0), s);
    p.add(egui::Shape::line(
        vec![
            egui::pos2(lerp(r.min.x, 0.3, r.max.x), body.max.y),
            egui::pos2(lerp(r.min.x, 0.24, r.max.x), r.max.y),
            egui::pos2(lerp(r.min.x, 0.52, r.max.x), body.max.y),
        ],
        s,
    ));
}

/// 文件夹（素材库/移动到）。
pub fn folder(p: &Painter, r: Rect, c: Color32) {
    let s = Stroke::new(sw(r), c);
    p.add(egui::Shape::line(
        vec![
            egui::pos2(r.min.x, lerp(r.min.y, 0.2, r.max.y)),
            egui::pos2(lerp(r.min.x, 0.34, r.max.x), lerp(r.min.y, 0.2, r.max.y)),
            egui::pos2(lerp(r.min.x, 0.44, r.max.x), lerp(r.min.y, 0.34, r.max.y)),
            egui::pos2(r.max.x, lerp(r.min.y, 0.34, r.max.y)),
            egui::pos2(r.max.x, lerp(r.min.y, 0.84, r.max.y)),
            egui::pos2(r.min.x, lerp(r.min.y, 0.84, r.max.y)),
            egui::pos2(r.min.x, lerp(r.min.y, 0.2, r.max.y)),
        ],
        s,
    ));
}

/// 关闭叉（退出）。
pub fn x_mark(p: &Painter, r: Rect, c: Color32) {
    let s = Stroke::new(sw(r) * 1.2, c);
    p.line_segment(
        [egui::pos2(r.min.x, r.min.y), egui::pos2(r.max.x, r.max.y)],
        s,
    );
    p.line_segment(
        [egui::pos2(r.max.x, r.min.y), egui::pos2(r.min.x, r.max.y)],
        s,
    );
}

/// 菜单/弹层内图标行（图标 + 文字，整行可点，悬停灰底）。
/// 弹层宽度由调用方 `ui.set_min_width` 保证。返回是否被点击。
/// 菜单命令行（官方菜单样式：纯文字 + 右侧快捷键列 + 绿色对勾 + 灰色禁用）。
/// 返回是否点击（禁用行恒 false）。菜单栏下拉与右键菜单区分：
/// 下拉用本行（无图标），弹层下拉（来源/移动）用带图标的 menu_item。
pub fn menu_cmd(
    ui: &mut egui::Ui,
    label: &str,
    shortcut: &str,
    checked: bool,
    enabled: bool,
) -> bool {
    let pal = theme::pal();
    let text_w = ui
        .painter()
        .layout_no_wrap(
            label.to_string(),
            egui::FontId::new(14.0, egui::FontFamily::Proportional),
            pal.text,
        )
        .size()
        .x;
    let sc_w = if shortcut.is_empty() {
        0.0
    } else {
        ui.painter()
            .layout_no_wrap(
                shortcut.to_string(),
                egui::FontId::new(13.0, egui::FontFamily::Proportional),
                pal.subtle,
            )
            .size()
            .x
    };
    let w = (30.0 + text_w + 30.0 + sc_w + 16.0).max(150.0);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 26.0), egui::Sense::click());
    if enabled && resp.hovered() {
        ui.painter().rect_filled(rect, 4.0, pal.hovered);
    }
    let color = if enabled { pal.text } else { pal.subtle };
    if checked {
        let g = ui.painter().layout_no_wrap(
            "✓".to_string(),
            egui::FontId::new(13.0, egui::FontFamily::Proportional),
            pal.green_dark,
        );
        ui.painter().galley(
            egui::pos2(rect.min.x + 12.0, rect.center().y - g.size().y * 0.5),
            g,
            pal.green_dark,
        );
    }
    let g = ui.painter().layout_no_wrap(
        label.to_string(),
        egui::FontId::new(14.0, egui::FontFamily::Proportional),
        color,
    );
    ui.painter().galley(
        egui::pos2(rect.min.x + 30.0, rect.center().y - g.size().y * 0.5),
        g,
        color,
    );
    if !shortcut.is_empty() {
        let g = ui.painter().layout_no_wrap(
            shortcut.to_string(),
            egui::FontId::new(13.0, egui::FontFamily::Proportional),
            pal.subtle,
        );
        ui.painter().galley(
            egui::pos2(
                rect.max.x - 14.0 - g.size().x,
                rect.center().y - g.size().y * 0.5,
            ),
            g,
            pal.subtle,
        );
    }
    enabled && resp.clicked()
}

pub fn menu_item(ui: &mut egui::Ui, icon: Option<&PaintFn>, label: &str) -> bool {
    menu_item_sel(ui, icon, label, false)
}

/// 带当前项态的菜单行（官方下拉实拍：当前项整行极浅绿底 + 品牌绿
/// 图标与文字，如「移动笔记」下拉中的当前笔记本行）。
pub fn menu_item_sel(
    ui: &mut egui::Ui,
    icon: Option<&PaintFn>,
    label: &str,
    selected: bool,
) -> bool {
    // 自然宽度（量文字后拼行）：egui 弹层按内容测宽，整行撑满会把菜单
    // 顶到弹层宽度上限（实测 1600 宽下 400px），且离锚点过远难以命中
    let text_w = ui
        .painter()
        .layout_no_wrap(
            label.to_string(),
            egui::FontId::new(14.0, egui::FontFamily::Proportional),
            theme::pal().text,
        )
        .size()
        .x;
    let w = (10.0 + if icon.is_some() { 22.0 } else { 0.0 } + text_w + 10.0).max(120.0);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 24.0), egui::Sense::click());
    let pal = theme::pal();
    if selected {
        ui.painter().rect_filled(rect, 4.0, pal.green_tint);
    } else if resp.hovered() {
        ui.painter().rect_filled(rect, 4.0, pal.hovered);
    }
    let accent = if selected { pal.green_dark } else { pal.text };
    if let Some(icon) = icon {
        icon(
            ui.painter(),
            egui::Rect::from_center_size(
                egui::pos2(rect.min.x + 15.0, rect.center().y),
                egui::vec2(13.0, 13.0),
            ),
            accent,
        );
    }
    let g = ui.painter().layout_no_wrap(
        label.to_string(),
        egui::FontId::new(14.0, egui::FontFamily::Proportional),
        accent,
    );
    ui.painter().galley(
        egui::pos2(
            rect.min.x + if icon.is_some() { 28.0 } else { 10.0 },
            rect.center().y - g.size().y * 0.5,
        ),
        g,
        accent,
    );
    resp.clicked()
}

/// 图钉（快捷方式）。
pub fn pin(p: &Painter, r: Rect, c: Color32) {
    let s = Stroke::new(sw(r) * 1.2, c);
    p.circle_filled(
        egui::pos2(r.center().x, lerp(r.min.y, 0.3, r.max.y)),
        r.height() * 0.2,
        c,
    );
    p.line_segment(
        [
            egui::pos2(r.center().x, lerp(r.min.y, 0.48, r.max.y)),
            egui::pos2(r.center().x, lerp(r.min.y, 0.72, r.max.y)),
        ],
        s,
    );
    p.line_segment(
        [
            egui::pos2(lerp(r.min.x, 0.3, r.max.x), lerp(r.min.y, 0.86, r.max.y)),
            egui::pos2(lerp(r.min.x, 0.7, r.max.x), lerp(r.min.y, 0.86, r.max.y)),
        ],
        s,
    );
}

/// 左弯箭头（移出/还原）。
pub fn undo(p: &Painter, r: Rect, c: Color32) {
    let s = Stroke::new(sw(r) * 1.2, c);
    let mid_y = lerp(r.min.y, 0.4, r.max.y);
    let mut pts = Vec::new();
    for i in 0..=10 {
        let t = i as f32 / 10.0;
        let a = std::f32::consts::PI * t; // 上半圆从左到右
        pts.push(egui::pos2(
            lerp(r.min.x, 0.28, r.max.x) + r.width() * 0.36 * (1.0 - a.cos().abs()),
            mid_y - r.height() * 0.3 * a.sin().abs() + r.height() * 0.18,
        ));
    }
    p.add(egui::Shape::line(pts, s));
    p.line_segment(
        [
            egui::pos2(lerp(r.min.x, 0.28, r.max.x), mid_y + r.height() * 0.18),
            egui::pos2(lerp(r.min.x, 0.28, r.max.x), lerp(r.min.y, 0.86, r.max.y)),
        ],
        s,
    );
    p.add(egui::Shape::convex_polygon(
        vec![
            egui::pos2(lerp(r.min.x, 0.14, r.max.x), lerp(r.min.y, 0.52, r.max.y)),
            egui::pos2(lerp(r.min.x, 0.42, r.max.x), lerp(r.min.y, 0.52, r.max.y)),
            egui::pos2(lerp(r.min.x, 0.28, r.max.x), lerp(r.min.y, 0.7, r.max.y)),
        ],
        c,
        egui::Stroke::NONE,
    ));
}

/// 浮动窗口（矩形 + 标题条）。
pub fn window(p: &Painter, r: Rect, c: Color32) {
    let s = Stroke::new(sw(r), c);
    p.rect_stroke(r, egui::Rounding::same(2.0), s);
    p.line_segment(
        [
            egui::pos2(r.min.x, lerp(r.min.y, 0.28, r.max.y)),
            egui::pos2(r.max.x, lerp(r.min.y, 0.28, r.max.y)),
        ],
        s,
    );
}

/// 右箭头（移动到）。
pub fn arrow(p: &Painter, r: Rect, c: Color32) {
    let s = Stroke::new(sw(r) * 1.2, c);
    let cy = r.center().y;
    p.line_segment(
        [
            egui::pos2(r.min.x, cy),
            egui::pos2(lerp(r.min.x, 0.82, r.max.x), cy),
        ],
        s,
    );
    p.add(egui::Shape::convex_polygon(
        vec![
            egui::pos2(lerp(r.min.x, 0.7, r.max.x), lerp(r.min.y, 0.22, r.max.y)),
            egui::pos2(lerp(r.min.x, 0.7, r.max.x), lerp(r.min.y, 0.78, r.max.y)),
            egui::pos2(r.max.x, cy),
        ],
        c,
        egui::Stroke::NONE,
    ));
}

// ---------- 图标按钮 ----------

/// 朴素图标钮（列表头/元信息行）：悬停显灰底，图标居中。
pub fn icon_btn(ui: &mut egui::Ui, size: f32, tip: &str, paint: &PaintFn) -> egui::Response {
    let pal = theme::pal();
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(rect, 6.0, pal.hovered);
    }
    paint(
        ui.painter(),
        rect.shrink(size * 0.2),
        if resp.hovered() { pal.text } else { pal.subtle },
    );
    resp.on_hover_text(tip)
}

/// 灰底圆角图标钮（编辑器工具行，对位原客户端浅灰方钮 + 绿图标）。
/// 上一代顶栏/编辑器钮图标，官方钮组改排后暂留备用。
#[allow(dead_code)]
pub fn chrome_btn(ui: &mut egui::Ui, size: f32, tip: &str, paint: &PaintFn) -> egui::Response {
    let pal = theme::pal();
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
    let bg = if resp.hovered() {
        pal.hovered
    } else {
        pal.extreme
    };
    ui.painter().rect_filled(rect, 8.0, bg);
    paint(ui.painter(), rect.shrink(size * 0.24), pal.green_dark);
    resp.on_hover_text(tip)
}

/// 空间（立方盒）。
pub fn cube(p: &Painter, r: Rect, c: Color32) {
    let s = Stroke::new(sw(r), c);
    let mid_y = lerp(r.min.y, 0.32, r.max.y);
    let l = r.min.x + r.width() * 0.08;
    let rt = r.max.x - r.width() * 0.08;
    let cx = r.center().x;
    let top = r.min.y + r.height() * 0.06;
    let bot = r.max.y - r.height() * 0.06;
    p.add(egui::Shape::line(
        vec![
            egui::pos2(cx, top),
            egui::pos2(rt, mid_y),
            egui::pos2(cx, bot),
            egui::pos2(l, mid_y),
            egui::pos2(cx, top),
        ],
        s,
    ));
    for x in [l, rt] {
        p.line_segment(
            [
                egui::pos2(x, mid_y),
                egui::pos2(x, lerp(r.min.y, 0.68, r.max.y)),
            ],
            s,
        );
    }
    p.line_segment([egui::pos2(cx, bot), egui::pos2(cx, mid_y)], s);
}

/// 清单（圆 + 对勾）。
pub fn check_circle(p: &Painter, r: Rect, c: Color32) {
    let s = Stroke::new(sw(r), c);
    p.circle_stroke(r.center(), r.width() * 0.42, s);
    p.add(egui::Shape::line(
        vec![
            egui::pos2(lerp(r.min.x, 0.30, r.max.x), lerp(r.min.y, 0.52, r.max.y)),
            egui::pos2(lerp(r.min.x, 0.45, r.max.x), lerp(r.min.y, 0.68, r.max.y)),
            egui::pos2(lerp(r.min.x, 0.72, r.max.x), lerp(r.min.y, 0.32, r.max.y)),
        ],
        s,
    ));
}

/// 日历。
pub fn calendar(p: &Painter, r: Rect, c: Color32) {
    let s = Stroke::new(sw(r), c);
    let body = Rect::from_min_max(
        egui::pos2(r.min.x + r.width() * 0.10, lerp(r.min.y, 0.20, r.max.y)),
        egui::pos2(r.max.x - r.width() * 0.10, r.max.y - r.height() * 0.08),
    );
    p.rect_stroke(body, 2.0, s);
    p.line_segment(
        [
            egui::pos2(body.min.x, lerp(r.min.y, 0.38, r.max.y)),
            egui::pos2(body.max.x, lerp(r.min.y, 0.38, r.max.y)),
        ],
        s,
    );
    for tx in [0.30, 0.70] {
        p.line_segment(
            [
                egui::pos2(lerp(r.min.x, tx, r.max.x), r.min.y + r.height() * 0.04),
                egui::pos2(lerp(r.min.x, tx, r.max.x), lerp(r.min.y, 0.28, r.max.y)),
            ],
            s,
        );
    }
}

/// 文件库（档案盒）。
pub fn archive_box(p: &Painter, r: Rect, c: Color32) {
    let s = Stroke::new(sw(r), c);
    let body = Rect::from_min_max(
        egui::pos2(r.min.x + r.width() * 0.10, lerp(r.min.y, 0.26, r.max.y)),
        egui::pos2(r.max.x - r.width() * 0.10, r.max.y - r.height() * 0.08),
    );
    p.rect_stroke(body, 2.0, s);
    let lid = Rect::from_min_max(
        egui::pos2(r.min.x + r.width() * 0.06, lerp(r.min.y, 0.10, r.max.y)),
        egui::pos2(r.max.x - r.width() * 0.06, lerp(r.min.y, 0.26, r.max.y)),
    );
    p.rect_stroke(lid, 2.0, s);
    p.line_segment(
        [
            egui::pos2(lerp(r.min.x, 0.40, r.max.x), lerp(r.min.y, 0.44, r.max.y)),
            egui::pos2(lerp(r.min.x, 0.60, r.max.x), lerp(r.min.y, 0.44, r.max.y)),
        ],
        s,
    );
}

/// 模板库（页面 + 标题块 + 行）。
pub fn template(p: &Painter, r: Rect, c: Color32) {
    let s = Stroke::new(sw(r), c);
    let body = Rect::from_min_max(
        egui::pos2(r.min.x + r.width() * 0.14, r.min.y + r.height() * 0.06),
        egui::pos2(r.max.x - r.width() * 0.14, r.max.y - r.height() * 0.06),
    );
    p.rect_stroke(body, 2.0, s);
    p.line_segment(
        [
            egui::pos2(body.min.x, lerp(r.min.y, 0.34, r.max.y)),
            egui::pos2(body.max.x, lerp(r.min.y, 0.34, r.max.y)),
        ],
        s,
    );
    for t in [0.54, 0.72] {
        let y = lerp(r.min.y, t, r.max.y);
        p.line_segment(
            [
                egui::pos2(lerp(r.min.x, 0.26, r.max.x), y),
                egui::pos2(lerp(r.min.x, 0.74, r.max.x), y),
            ],
            s,
        );
    }
}

/// 知识星图（三节点网络）。
pub fn graph_nodes(p: &Painter, r: Rect, c: Color32) {
    let s = Stroke::new(sw(r), c);
    let a = egui::pos2(r.center().x, r.min.y + r.height() * 0.16);
    let b = egui::pos2(r.min.x + r.width() * 0.12, r.max.y - r.height() * 0.16);
    let d = egui::pos2(r.max.x - r.width() * 0.12, r.max.y - r.height() * 0.20);
    p.line_segment([a, b], s);
    p.line_segment([a, d], s);
    p.line_segment([b, d], s);
    for n in [a, b, d] {
        p.circle_filled(n, r.width() * 0.12, c);
    }
}

/// 重做（撤销的水平镜像）。
pub fn redo(p: &Painter, r: Rect, c: Color32) {
    let mirrored = Rect::from_min_max(egui::pos2(r.max.x, r.min.y), egui::pos2(r.min.x, r.max.y));
    undo(p, mirrored, c);
}

/// 帮助（圆 + 问号）。
pub fn help_circle(p: &Painter, r: Rect, c: Color32) {
    let s = Stroke::new(sw(r) * 0.9, c);
    p.circle_stroke(r.center(), r.width() * 0.44, s);
    let g = p.layout_no_wrap(
        "?".to_string(),
        egui::FontId::new(r.height() * 0.52, egui::FontFamily::Proportional),
        c,
    );
    p.galley(
        egui::pos2(
            r.center().x - g.size().x * 0.5,
            r.center().y - g.size().y * 0.55,
        ),
        g,
        c,
    );
}

/// 底部列表视图（两行方块 + 横线）。
pub fn rows_view(p: &Painter, r: Rect, c: Color32) {
    let s = Stroke::new(sw(r) * 0.9, c);
    for t in [0.16, 0.56] {
        let y = lerp(r.min.y, t, r.max.y);
        let sq = Rect::from_min_size(
            egui::pos2(r.min.x + r.width() * 0.04, y),
            egui::vec2(r.width() * 0.24, r.height() * 0.26),
        );
        p.rect_stroke(sq, 1.5, s);
        p.line_segment(
            [
                egui::pos2(lerp(r.min.x, 0.40, r.max.x), y + r.height() * 0.13),
                egui::pos2(r.max.x - r.width() * 0.02, y + r.height() * 0.13),
            ],
            s,
        );
    }
}

/// 通知铃。
pub fn bell(p: &Painter, r: Rect, c: Color32) {
    let s = Stroke::new(sw(r), c);
    let cx = r.center().x;
    p.add(egui::Shape::line(
        vec![
            egui::pos2(lerp(r.min.x, 0.26, r.max.x), lerp(r.min.y, 0.72, r.max.y)),
            egui::pos2(
                lerp(r.min.x, 0.30, r.max.y * 0.0 + 1.0) * 0.0 + lerp(r.min.x, 0.30, r.max.x),
                lerp(r.min.y, 0.42, r.max.y),
            ),
            egui::pos2(cx, r.min.y + r.height() * 0.12),
            egui::pos2(lerp(r.min.x, 0.70, r.max.x), lerp(r.min.y, 0.42, r.max.y)),
            egui::pos2(lerp(r.min.x, 0.74, r.max.x), lerp(r.min.y, 0.72, r.max.y)),
            egui::pos2(lerp(r.min.x, 0.26, r.max.x), lerp(r.min.y, 0.72, r.max.y)),
        ],
        s,
    ));
    p.line_segment(
        [
            egui::pos2(cx, r.min.y + r.height() * 0.12),
            egui::pos2(cx, lerp(r.min.y, 0.22, r.max.y)),
        ],
        s,
    );
    p.line_segment(
        [
            egui::pos2(lerp(r.min.x, 0.44, r.max.x), r.max.y - r.height() * 0.10),
            egui::pos2(lerp(r.min.x, 0.56, r.max.x), r.max.y - r.height() * 0.10),
        ],
        Stroke::new(sw(r) * 1.2, c),
    );
}

/// 分享（方框 + 外向箭头）。
pub fn share_box(p: &Painter, r: Rect, c: Color32) {
    let s = Stroke::new(sw(r), c);
    p.add(egui::Shape::line(
        vec![
            egui::pos2(lerp(r.min.x, 0.62, r.max.x), r.min.y + r.height() * 0.10),
            egui::pos2(r.min.x + r.width() * 0.10, r.min.y + r.height() * 0.10),
            egui::pos2(r.min.x + r.width() * 0.10, r.max.y - r.height() * 0.10),
            egui::pos2(r.max.x - r.width() * 0.10, r.max.y - r.height() * 0.10),
            egui::pos2(r.max.x - r.width() * 0.10, lerp(r.min.y, 0.38, r.max.y)),
        ],
        s,
    ));
    p.line_segment(
        [
            egui::pos2(lerp(r.min.x, 0.46, r.max.x), lerp(r.min.y, 0.54, r.max.y)),
            egui::pos2(r.max.x - r.width() * 0.08, r.min.y + r.height() * 0.08),
        ],
        s,
    );
    let t = Stroke::new(sw(r) * 1.2, c);
    p.line_segment(
        [
            egui::pos2(r.max.x - r.width() * 0.28, r.min.y + r.height() * 0.08),
            egui::pos2(r.max.x - r.width() * 0.06, r.min.y + r.height() * 0.08),
        ],
        t,
    );
    p.line_segment(
        [
            egui::pos2(r.max.x - r.width() * 0.08, r.min.y + r.height() * 0.06),
            egui::pos2(r.max.x - r.width() * 0.08, r.min.y + r.height() * 0.28),
        ],
        t,
    );
}

/// 表情（圆 + 双眼 + 弧笑）。
pub fn smiley(p: &Painter, r: Rect, c: Color32) {
    let s = Stroke::new(sw(r), c);
    p.circle_stroke(r.center(), r.width() * 0.42, s);
    for tx in [0.36, 0.64] {
        p.circle_filled(
            egui::pos2(lerp(r.min.x, tx, r.max.x), lerp(r.min.y, 0.42, r.max.y)),
            r.width() * 0.055,
            c,
        );
    }
    p.add(egui::Shape::line(
        vec![
            egui::pos2(lerp(r.min.x, 0.32, r.max.x), lerp(r.min.y, 0.60, r.max.y)),
            egui::pos2(r.center().x, lerp(r.min.y, 0.72, r.max.y)),
            egui::pos2(lerp(r.min.x, 0.68, r.max.x), lerp(r.min.y, 0.60, r.max.y)),
        ],
        s,
    ));
}

/// 盾牌（含锁孔）。
pub fn shield_key(p: &Painter, r: Rect, c: Color32) {
    let s = Stroke::new(sw(r), c);
    let cx = r.center().x;
    p.add(egui::Shape::line(
        vec![
            egui::pos2(cx, r.min.y + r.height() * 0.08),
            egui::pos2(r.max.x - r.width() * 0.12, lerp(r.min.y, 0.22, r.max.y)),
            egui::pos2(r.max.x - r.width() * 0.12, lerp(r.min.y, 0.55, r.max.y)),
            egui::pos2(cx, r.max.y - r.height() * 0.08),
            egui::pos2(r.min.x + r.width() * 0.12, lerp(r.min.y, 0.55, r.max.y)),
            egui::pos2(r.min.x + r.width() * 0.12, lerp(r.min.y, 0.22, r.max.y)),
            egui::pos2(cx, r.min.y + r.height() * 0.08),
        ],
        s,
    ));
    p.circle_filled(
        egui::pos2(cx, lerp(r.min.y, 0.42, r.max.y)),
        r.width() * 0.075,
        c,
    );
    p.line_segment(
        [
            egui::pos2(cx, lerp(r.min.y, 0.50, r.max.y)),
            egui::pos2(cx, lerp(r.min.y, 0.62, r.max.y)),
        ],
        s,
    );
}

/// 圈内三点（官方 ⋯ 钮）。
pub fn dots_circle(p: &Painter, r: Rect, c: Color32) {
    let s = Stroke::new(sw(r), c);
    p.circle_stroke(r.center(), r.width() * 0.42, s);
    for tx in [0.30, 0.50, 0.70] {
        p.circle_filled(
            egui::pos2(lerp(r.min.x, tx, r.max.x), r.center().y),
            r.width() * 0.05,
            c,
        );
    }
}

/// 左/右 chevron（顶栏导航箭头）。
pub fn chevron_lr(p: &Painter, r: Rect, c: Color32, left: bool) {
    let s = Stroke::new(sw(r) * 1.2, c);
    let (x1, x2) = if left {
        (lerp(r.min.x, 0.62, r.max.x), lerp(r.min.x, 0.34, r.max.x))
    } else {
        (lerp(r.min.x, 0.38, r.max.x), lerp(r.min.x, 0.66, r.max.x))
    };
    p.add(egui::Shape::line(
        vec![
            egui::pos2(x1, r.min.y + r.height() * 0.14),
            egui::pos2(x2, r.center().y),
            egui::pos2(x1, r.max.y - r.height() * 0.14),
        ],
        s,
    ));
}

/// 头像（绿圆 + 白色大象简笔：头 + 垂鼻 + 耳）。
pub fn avatar(p: &Painter, r: Rect, _c: Color32) {
    p.circle_filled(
        r.center(),
        r.width() * 0.48,
        Color32::from_rgb(0x2E, 0xB8, 0x72),
    );
    let w = Color32::WHITE;
    let body = Rect::from_center_size(
        egui::pos2(
            r.center().x - r.width() * 0.05,
            r.center().y - r.height() * 0.06,
        ),
        egui::vec2(r.width() * 0.42, r.height() * 0.34),
    );
    p.circle_filled(body.center(), body.width() * 0.5, w);
    p.add(egui::Shape::line(
        vec![
            egui::pos2(
                r.center().x + r.width() * 0.10,
                r.center().y - r.height() * 0.02,
            ),
            egui::pos2(
                r.center().x + r.width() * 0.15,
                r.center().y + r.height() * 0.20,
            ),
            egui::pos2(
                r.center().x + r.width() * 0.03,
                r.center().y + r.height() * 0.25,
            ),
        ],
        Stroke::new(r.width() * 0.09, w),
    ));
    p.circle_filled(
        egui::pos2(
            r.center().x - r.width() * 0.15,
            r.center().y - r.height() * 0.10,
        ),
        r.width() * 0.09,
        w,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 图标绘制函数在任意小矩形上不 panic（几何越界回归）。
    #[test]
    fn icons_render_on_tiny_rects() {
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(200.0, 200.0),
            )),
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let r = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(14.0, 14.0));
                let p = ui.painter().clone();
                let c = Color32::GRAY;
                plus_circle(&p, r, c, Color32::WHITE);
                doc(&p, r, c);
                doc_plus(&p, r, c);
                book(&p, r, c);
                star(&p, r, c);
                tag(&p, r, c);
                trash(&p, r, c);
                corp(&p, r, c);
                sparkle(&p, r, c);
                search(&p, r, c);
                sort(&p, r, c);
                cards(&p, r, c);
                list(&p, r, c);
                dots(&p, r, c);
                clip(&p, r, c);
                share(&p, r, c);
                person_plus(&p, r, c);
                clock(&p, r, c);
                globe(&p, r, c);
                info(&p, r, c);
                chevron(&p, r, c, true);
                chevron(&p, r, c, false);
            });
        });
    }
}

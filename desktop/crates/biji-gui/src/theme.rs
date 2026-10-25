//! 视觉主题——双主题（浅色 / 深色），浅色对位原 Windows 笔记客户端
//! （v7.4.30.9439）的实测配色，深色对位原客户端深色模式。
//!
//! 浅色取值来自原客户端实机截图的程序化采样（spike/ref-*.png）：
//! 品牌绿 = 「新建笔记」绿圆/同步圆钮的填充色，选中卡 = 绿描边 + 浅绿底，
//! 侧栏 = 浅灰底 + 选中项深灰底，正文区纯白，链接为经典蓝。
//! 深色：近黑底 + 深灰卡片/侧栏、浅色正文、亮一档的品牌绿与链接蓝。
//! 当前模式是进程级原子状态，选择持久化在库 meta 表（key = "theme"）。

use egui::{Color32, Context, Stroke, Visuals};
use std::sync::atomic::{AtomicBool, Ordering};

/// 双主题调色板（字段含义见各主题实例）。
pub struct Palette {
    /// 品牌绿（新建/同步等品牌动作、选中态文字与图标、列表日期）。
    pub green: Color32,
    /// 深一档的绿：浅色下白底绿字；深色下用更亮的绿保证对比。
    pub green_dark: Color32,
    /// 选中笔记卡的亮绿描边。
    pub green_edge: Color32,
    /// 选中笔记卡的浅绿底（深色下为深绿底）。
    pub green_tint: Color32,
    /// 侧栏底色。
    pub sidebar_bg: Color32,
    /// 侧栏选中项底。
    pub sel_bg: Color32,
    /// 侧栏悬停底。
    pub sidebar_hover: Color32,
    /// 列表/窗口底。
    pub bg: Color32,
    /// 卡片/弹窗底（浅色 = 纯白）。
    pub white: Color32,
    /// 主文本。
    pub text: Color32,
    /// 次级灰（计数、摘要、占位提示）。
    pub subtle: Color32,
    /// 发丝分隔线。
    pub hairline: Color32,
    /// 链接蓝（经典下划线蓝；深色下提亮）。
    pub link: Color32,
    /// 输入底/极底。
    pub extreme: Color32,
    /// 弱底（faint）。
    pub faint: Color32,
    /// 控件悬停底。
    pub hovered: Color32,
}

/// 浅色主题（对位原客户端浅色 UI，逐值保持既有取色）。
pub static LIGHT: Palette = Palette {
    green: Color32::from_rgb(0x00, 0xB8, 0x70),
    green_dark: Color32::from_rgb(0x00, 0x9E, 0x60),
    green_edge: Color32::from_rgb(0x3E, 0xE8, 0x7C),
    green_tint: Color32::from_rgb(0xEE, 0xFE, 0xF5),
    sidebar_bg: Color32::from_rgb(0xEC, 0xEC, 0xF0),
    sel_bg: Color32::from_rgb(0xE2, 0xE2, 0xE6),
    sidebar_hover: Color32::from_rgb(0xE6, 0xE6, 0xEA),
    bg: Color32::from_rgb(0xFC, 0xFC, 0xFC),
    white: Color32::WHITE,
    text: Color32::from_rgb(0x1A, 0x1A, 0x1E),
    subtle: Color32::from_rgb(0x8A, 0x8A, 0x90),
    hairline: Color32::from_rgb(0xE2, 0xE2, 0xE6),
    link: Color32::from_rgb(0x24, 0x56, 0xDC),
    extreme: Color32::from_rgb(0xF4, 0xF4, 0xF6),
    faint: Color32::from_rgb(0xF4, 0xF4, 0xF6),
    hovered: Color32::from_rgb(0xF0, 0xF0, 0xF3),
};

/// 深色主题（对位原客户端深色模式：近黑底、深灰卡片、浅色正文、亮绿）。
pub static DARK: Palette = Palette {
    green: Color32::from_rgb(0x00, 0xC8, 0x80),
    green_dark: Color32::from_rgb(0x35, 0xD9, 0x92),
    green_edge: Color32::from_rgb(0x3E, 0xE8, 0x7C),
    green_tint: Color32::from_rgb(0x18, 0x38, 0x28),
    sidebar_bg: Color32::from_rgb(0x23, 0x23, 0x26),
    sel_bg: Color32::from_rgb(0x3C, 0x3C, 0x41),
    sidebar_hover: Color32::from_rgb(0x2E, 0x2E, 0x33),
    bg: Color32::from_rgb(0x1E, 0x1E, 0x20),
    white: Color32::from_rgb(0x2A, 0x2A, 0x2E),
    text: Color32::from_rgb(0xE8, 0xE8, 0xEC),
    subtle: Color32::from_rgb(0x9A, 0x9A, 0xA2),
    hairline: Color32::from_rgb(0x3A, 0x3A, 0x40),
    link: Color32::from_rgb(0x7C, 0x9C, 0xF5),
    extreme: Color32::from_rgb(0x1A, 0x1A, 0x1C),
    faint: Color32::from_rgb(0x23, 0x23, 0x25),
    hovered: Color32::from_rgb(0x36, 0x36, 0x3B),
};

static DARK_MODE: AtomicBool = AtomicBool::new(false);

/// 当前是否深色模式（进程级状态，随库 meta 持久化恢复）。
pub fn is_dark() -> bool {
    DARK_MODE.load(Ordering::Relaxed)
}

/// 切换当前模式（启动时由库 meta 恢复，运行期由查看菜单/工具栏切换）。
pub fn set_dark(on: bool) {
    DARK_MODE.store(on, Ordering::Relaxed);
}

/// 当前调色板。
pub fn pal() -> &'static Palette {
    if is_dark() {
        &DARK
    } else {
        &LIGHT
    }
}

/// 应用当前主题（每帧调用；annotator 的局部深色画布后由本函数恢复）。
pub fn apply(ctx: &Context) {
    let (p, dark) = if is_dark() {
        (&DARK, true)
    } else {
        (&LIGHT, false)
    };
    let mut v = if dark { Visuals::dark() } else { Visuals::light() };
    v.override_text_color = Some(p.text);
    v.panel_fill = p.bg;
    v.window_fill = p.white;
    v.extreme_bg_color = p.extreme;
    v.faint_bg_color = p.faint;
    v.hyperlink_color = p.link;
    v.selection.bg_fill = p.green.gamma_multiply(if dark { 0.30 } else { 0.22 });
    v.selection.stroke = Stroke::new(1.0_f32, p.green);
    v.window_stroke = Stroke::new(1.0_f32, p.hairline);
    v.window_rounding = 6.0.into();
    v.window_shadow = egui::epaint::Shadow::NONE;
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.rounding = 6.0.into();
    }
    v.widgets.inactive.bg_fill = p.white;
    v.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, p.text);
    v.widgets.hovered.bg_fill = p.hovered;
    v.widgets.active.bg_fill = p.sel_bg;
    v.widgets.active.fg_stroke = Stroke::new(1.0_f32, p.green_dark);
    // 面板分隔线（egui 浅色默认纯黑，实测在侧栏右缘成粗黑带）改为发丝线
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, p.hairline);
    ctx.set_visuals(v);
    // 间距/字号在 egui 0.29 挂在 Style 上而非 Visuals。
    // 字号比例对位原客户端：正文 15 / 标题 24 / 按钮 14 / 小字 12（egui 默认 12.5/18/12.5/9）。
    ctx.style_mut(|style| {
        style.spacing.item_spacing = egui::vec2(8.0, 5.0);
        style.spacing.button_padding = egui::vec2(10.0, 4.0);
        let ts = &mut style.text_styles;
        ts.insert(
            egui::TextStyle::Small,
            egui::FontId::new(12.0, egui::FontFamily::Proportional),
        );
        ts.insert(
            egui::TextStyle::Body,
            egui::FontId::new(15.0, egui::FontFamily::Proportional),
        );
        ts.insert(
            egui::TextStyle::Button,
            egui::FontId::new(14.0, egui::FontFamily::Proportional),
        );
        ts.insert(
            egui::TextStyle::Heading,
            egui::FontId::new(24.0, egui::FontFamily::Proportional),
        );
        ts.insert(
            egui::TextStyle::Monospace,
            egui::FontId::new(13.0, egui::FontFamily::Monospace),
        );
        // 细滚动条（对位原客户端的窄灰条）
        style.spacing.scroll.bar_width = 6.0;
    });
}

/// 标注画布窗口的深色视觉（局部生效，下帧开头由 `apply` 恢复当前主题）。
pub fn apply_dark(ctx: &Context) {
    let markup_bg = Color32::from_rgb(0x2B, 0x2E, 0x33);
    let mut v = Visuals::dark();
    v.panel_fill = markup_bg;
    v.window_fill = markup_bg;
    v.extreme_bg_color = Color32::from_rgb(0x22, 0x24, 0x28);
    v.faint_bg_color = Color32::from_rgb(0x36, 0x39, 0x3F);
    v.selection.bg_fill = LIGHT.green.gamma_multiply(0.30);
    v.selection.stroke = Stroke::new(1.0_f32, LIGHT.green);
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.rounding = 6.0.into();
    }
    v.widgets.inactive.bg_fill = Color32::from_rgb(0x3A, 0x3E, 0x45);
    v.widgets.hovered.bg_fill = Color32::from_rgb(0x44, 0x49, 0x51);
    v.widgets.active.bg_fill = Color32::from_rgb(0x50, 0x56, 0x5F);
    ctx.set_visuals(v);
}

/// 品牌绿填充药丸按钮：同步、标注画布的保存写回。
pub fn green_button<'a>(text: &'a str) -> egui::Button<'a> {
    egui::Button::new(egui::RichText::new(text).color(Color32::WHITE))
        .fill(pal().green)
        .stroke(Stroke::NONE)
        .rounding(12.0)
}

/// 侧栏导航行：左对齐自绘控件（egui Button 会把文字水平居中，导致导航行
/// 与分区头/药丸错位——这里自绘底色 + 左起排版，图标恒定 10px 内边距）。
pub fn nav_row(ui: &mut egui::Ui, icon: &str, label: &str, selected: bool) -> egui::Response {
    nav_row_sized(ui, ui.available_width(), icon, label, selected)
}

/// 定宽变体（笔记本行右侧要留出计数与删除按钮）。
pub fn nav_row_sized(
    ui: &mut egui::Ui,
    width: f32,
    icon: &str,
    label: &str,
    selected: bool,
) -> egui::Response {
    let p = pal();
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, 26.0), egui::Sense::click());
    let fill = if selected {
        p.sel_bg
    } else if resp.hovered() {
        p.sidebar_hover
    } else {
        p.sidebar_bg
    };
    ui.painter().rect_filled(rect, 6.0, fill);
    let text_color = if selected { p.green_dark } else { p.text };
    // 图标/文字两段固定 x 排版：emoji 步进宽度不固定，合并成串会让文字
    // 起点漂移；拆两段后文字与分区头标签严格同轨（x+30）。
    let icon_galley = ui.painter().layout_no_wrap(
        icon.to_string(),
        egui::FontId::new(14.0, egui::FontFamily::Proportional),
        text_color,
    );
    ui.painter().galley(
        egui::pos2(rect.min.x + 10.0, rect.center().y - icon_galley.size().y * 0.5),
        icon_galley,
        text_color,
    );
    let label_galley = ui.painter().layout_no_wrap(
        label.to_string(),
        egui::FontId::new(14.0, egui::FontFamily::Proportional),
        text_color,
    );
    ui.painter().galley(
        egui::pos2(rect.min.x + 30.0, rect.center().y - label_galley.size().y * 0.5),
        label_galley,
        text_color,
    );
    resp
}

/// 侧栏分区头（手风琴）：箭头固定 11px、标签固定 29px 起排——与导航行
/// 的图标 10px / 文字 ~28px 两条对齐线一致。返回「＋」是否被点击。
/// 点击整行切换 `open`。整体包在 horizontal 内，保证「＋」与文字同行。
pub fn section_header(
    ui: &mut egui::Ui,
    label: &str,
    open: &mut bool,
    trailing_plus: bool,
) -> bool {
    let mut plus_clicked = false;
    ui.horizontal(|ui| {
        let p = pal();
        let w = ui.available_width() - if trailing_plus { 34.0 } else { 0.0 };
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 22.0), egui::Sense::click());
        if resp.hovered() {
            ui.painter().rect_filled(rect, 6.0, p.sidebar_hover);
        }
        let gray = p.subtle;
        let arrow = if *open { "▼" } else { "▶" };
        let arrow_galley = ui.painter().layout_no_wrap(
            arrow.to_string(),
            egui::FontId::new(10.0, egui::FontFamily::Proportional),
            gray,
        );
        ui.painter().galley(
            egui::pos2(rect.min.x + 11.0, rect.center().y - arrow_galley.size().y * 0.5),
            arrow_galley,
            gray,
        );
        let label_galley = ui.painter().layout_no_wrap(
            label.to_string(),
            egui::FontId::new(12.0, egui::FontFamily::Proportional),
            gray,
        );
        ui.painter().galley(
            egui::pos2(rect.min.x + 30.0, rect.center().y - label_galley.size().y * 0.5),
            label_galley,
            gray,
        );
        if resp.clicked() {
            *open = !*open;
        }
        if trailing_plus {
            // ＋ 用自绘 + interact（add_sized 在 horizontal 内会派生 top_down
            // 子区，把面板右缘撑出一条贯穿全高的黑带——实测 bug，勿改回）。
            let prect = egui::Rect::from_min_size(
                egui::pos2(rect.max.x + 8.0, rect.min.y - 2.0),
                egui::vec2(26.0, 22.0),
            );
            let presp = ui.interact(prect, egui::Id::new(("section-plus", label)), egui::Sense::click());
            if presp.hovered() {
                ui.painter().rect_filled(prect, 6.0, p.sel_bg);
            }
            let plus_galley = ui.painter().layout_no_wrap(
                "＋".to_string(),
                egui::FontId::new(12.0, egui::FontFamily::Proportional),
                gray,
            );
            ui.painter().galley(
                egui::pos2(
                    prect.center().x - plus_galley.size().x * 0.5,
                    prect.center().y - plus_galley.size().y * 0.5,
                ),
                plus_galley,
                gray,
            );
            if presp.clicked() {
                plus_clicked = true;
            }
        }
    });
    plus_clicked
}

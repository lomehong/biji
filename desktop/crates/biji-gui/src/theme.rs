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
    /// 菜单栏文字（原客户端为中性灰，非纯黑）。
    pub menu_text: Color32,
    /// 列表卡日期（原客户端为淡蓝，非品牌绿）。
    pub date_blue: Color32,
    /// AI 助理紫（图标与文字）。
    pub ai_purple: Color32,
    /// 发丝分隔线。
    pub hairline: Color32,
    /// 未选中笔记卡描边（比 hairline 更浅，实测 #F0F0F0）。
    pub card_border: Color32,
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
    green_edge: Color32::from_rgb(0x31, 0xEF, 0x6F),
    green_tint: Color32::from_rgb(0xEE, 0xFE, 0xF5),
    sidebar_bg: Color32::from_rgb(0xEF, 0xEF, 0xF2),
    sel_bg: Color32::from_rgb(0xE7, 0xE7, 0xEA),
    sidebar_hover: Color32::from_rgb(0xE6, 0xE6, 0xEA),
    bg: Color32::from_rgb(0xFC, 0xFC, 0xFC),
    white: Color32::WHITE,
    text: Color32::from_rgb(0x1A, 0x1A, 0x1E),
    subtle: Color32::from_rgb(0x8A, 0x8A, 0x90),
    menu_text: Color32::from_rgb(0x4F, 0x4F, 0x4F),
    date_blue: Color32::from_rgb(0x6F, 0xAE, 0xE0),
    ai_purple: Color32::from_rgb(0x9C, 0x8C, 0xFB),
    hairline: Color32::from_rgb(0xE2, 0xE2, 0xE6),
    card_border: Color32::from_rgb(0xF0, 0xF0, 0xF0),
    link: Color32::from_rgb(0x24, 0x56, 0xDC),
    extreme: Color32::from_rgb(0xF4, 0xF4, 0xF6),
    faint: Color32::from_rgb(0xF4, 0xF4, 0xF6),
    hovered: Color32::from_rgb(0xF0, 0xF0, 0xF3),
};

/// 深色主题。暖灰取值来自同代印象笔记 Mac 深色模式实拍采样
/// （spike/webref/cand1.jpg，appinn 2018 评测图，同代设计语言）：
/// 侧栏 #3B3738 / 列表 #2E2A29 / 卡片 #353130 / 正文 #E0E0E0 /
/// 日期蓝 #317AC7（暗底提亮为 #4E93DC）。
pub static DARK: Palette = Palette {
    green: Color32::from_rgb(0x00, 0xC8, 0x80),
    green_dark: Color32::from_rgb(0x35, 0xD9, 0x92),
    green_edge: Color32::from_rgb(0x31, 0xEF, 0x6F),
    green_tint: Color32::from_rgb(0x1C, 0x3A, 0x2D),
    sidebar_bg: Color32::from_rgb(0x3B, 0x37, 0x38),
    sel_bg: Color32::from_rgb(0x47, 0x42, 0x40),
    sidebar_hover: Color32::from_rgb(0x42, 0x3D, 0x3C),
    bg: Color32::from_rgb(0x2E, 0x2A, 0x29),
    white: Color32::from_rgb(0x35, 0x31, 0x30),
    text: Color32::from_rgb(0xE0, 0xE0, 0xE0),
    subtle: Color32::from_rgb(0xA2, 0x9C, 0x98),
    menu_text: Color32::from_rgb(0xC6, 0xC1, 0xBD),
    date_blue: Color32::from_rgb(0x4E, 0x93, 0xDC),
    ai_purple: Color32::from_rgb(0xA9, 0x9C, 0xFF),
    hairline: Color32::from_rgb(0x46, 0x41, 0x3F),
    card_border: Color32::from_rgb(0x42, 0x3E, 0x3C),
    link: Color32::from_rgb(0x5B, 0x9B, 0xD8),
    extreme: Color32::from_rgb(0x26, 0x23, 0x22),
    faint: Color32::from_rgb(0x2A, 0x26, 0x25),
    hovered: Color32::from_rgb(0x42, 0x3D, 0x3C),
};

static DARK_MODE: AtomicBool = AtomicBool::new(false);

/// headless 测试串行化：DARK_MODE 是进程级全局，并行测试会互踩主题态。
/// 触碰主题的测试在开头取此锁（用 unwrap_or_else 容忍毒化）。
/// 仅测试引用，非测试构建下 dead_code。
#[cfg_attr(not(test), allow(dead_code))]
pub static TEST_THEME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// 当前是否深色模式（进程级状态，随库 meta 持久化恢复）。
pub fn is_dark() -> bool {
    DARK_MODE.load(Ordering::Relaxed)
}

/// 切换当前模式（启动时由库 meta 恢复，运行期由查看菜单/工具栏切换）。
pub fn set_dark(on: bool) {
    DARK_MODE.store(on, Ordering::Relaxed);
}

/// 真字重粗体字族（`install_cjk_fonts` 注册雅黑 Bold；未注册时回退常规体）。
/// egui 0.29 无字重支持（RichText::strong 只加强颜色），标题类文本用本字族。
pub fn bold_family() -> egui::FontFamily {
    if BOLD_REGISTERED.load(Ordering::Relaxed) {
        egui::FontFamily::Name(std::sync::Arc::from("cjk-bold"))
    } else {
        egui::FontFamily::Proportional
    }
}

static BOLD_REGISTERED: AtomicBool = AtomicBool::new(false);

/// install_cjk_fonts 注册粗体面后调用（headless 测试进程不注册，
/// bold_family 随之回退常规字族，避免未绑定字族 panic）。
pub fn set_bold_registered(on: bool) {
    BOLD_REGISTERED.store(on, Ordering::Relaxed);
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
    let mut v = if dark {
        Visuals::dark()
    } else {
        Visuals::light()
    };
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
            egui::FontId::new(24.0, bold_family()),
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

/// 图标绘制回调（sidebar 行 / 图标钮共用）。
pub type IconPaint<'a> = &'a dyn Fn(&egui::Painter, egui::Rect, Color32);

/// 侧栏导航行（对位原客户端线性图标行）：自绘底色 + 图标 + 左起文字。
/// egui Button 会把文字水平居中导致与分区错位，这里自绘保持左对齐。
pub fn nav_row_p(
    ui: &mut egui::Ui,
    width: f32,
    label: &str,
    selected: bool,
    icon: IconPaint<'_>,
) -> egui::Response {
    let p = pal();
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, 34.0), egui::Sense::click());
    let fill = if selected {
        p.sel_bg
    } else if resp.hovered() {
        p.sidebar_hover
    } else {
        p.sidebar_bg
    };
    ui.painter().rect_filled(rect, 8.0, fill);
    let text_color = if selected { p.green_dark } else { p.text };
    let icon_rect = egui::Rect::from_center_size(
        egui::pos2(rect.min.x + 17.0, rect.center().y),
        egui::vec2(14.0, 14.0),
    );
    icon(ui.painter(), icon_rect, text_color);
    let label_galley = ui.painter().layout_no_wrap(
        label.to_string(),
        egui::FontId::new(14.0, egui::FontFamily::Proportional),
        text_color,
    );
    ui.painter().galley(
        egui::pos2(
            rect.min.x + 32.0,
            rect.center().y - label_galley.size().y * 0.5,
        ),
        label_galley,
        text_color,
    );
    resp
}

/// 带折叠箭头与右侧＋的分区行（快捷方式/全部笔记/笔记本/标签，对位原客户端
/// 「› ★ 快捷方式」形态）。`chevron_open` 为箭头视觉状态（None 不画箭头）。
/// 返回 (行点击, ＋是否点击)。
pub fn nav_row_chev(
    ui: &mut egui::Ui,
    width: f32,
    chevron_open: Option<bool>,
    label: &str,
    selected: bool,
    icon: IconPaint<'_>,
    trailing_plus: bool,
) -> (egui::Response, bool) {
    let mut plus_clicked = false;
    let mut row = None;
    ui.horizontal(|ui| {
        let p = pal();
        let w = width - if trailing_plus { 34.0 } else { 0.0 };
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 34.0), egui::Sense::click());
        let fill = if selected {
            p.sel_bg
        } else if resp.hovered() {
            p.sidebar_hover
        } else {
            p.sidebar_bg
        };
        ui.painter().rect_filled(rect, 8.0, fill);
        let text_color = if selected { p.green_dark } else { p.text };
        let mut x = rect.min.x + 8.0;
        if let Some(is_open) = chevron_open {
            let cr = egui::Rect::from_center_size(
                egui::pos2(x + 5.0, rect.center().y),
                egui::vec2(9.0, 9.0),
            );
            crate::icons::chevron(ui.painter(), cr, p.subtle, is_open);
            x += 16.0;
        }
        let icon_rect = egui::Rect::from_center_size(
            egui::pos2(x + 7.0, rect.center().y),
            egui::vec2(14.0, 14.0),
        );
        icon(ui.painter(), icon_rect, text_color);
        let label_galley = ui.painter().layout_no_wrap(
            label.to_string(),
            egui::FontId::new(14.0, egui::FontFamily::Proportional),
            text_color,
        );
        ui.painter().galley(
            egui::pos2(x + 20.0, rect.center().y - label_galley.size().y * 0.5),
            label_galley,
            text_color,
        );
        row = Some(resp);
        if trailing_plus {
            // ＋ 用自绘 + interact（add_sized 在 horizontal 内会派生 top_down
            // 子区，把面板右缘撑出一条贯穿全高的黑带——实测 bug，勿改回）。
            let prect = egui::Rect::from_min_size(
                egui::pos2(rect.max.x + 8.0, rect.min.y + 3.0),
                egui::vec2(26.0, 22.0),
            );
            let presp = ui.interact(
                prect,
                egui::Id::new(("section-plus", label)),
                egui::Sense::click(),
            );
            if presp.hovered() {
                ui.painter().rect_filled(prect, 6.0, p.sel_bg);
            }
            let plus_galley = ui.painter().layout_no_wrap(
                "＋".to_string(),
                egui::FontId::new(13.0, egui::FontFamily::Proportional),
                p.subtle,
            );
            ui.painter().galley(
                egui::pos2(
                    prect.center().x - plus_galley.size().x * 0.5,
                    prect.center().y - plus_galley.size().y * 0.5,
                ),
                plus_galley,
                p.subtle,
            );
            if presp.clicked() {
                plus_clicked = true;
            }
        }
    });
    (
        row.unwrap_or_else(|| unreachable!("horizontal 必然执行")),
        plus_clicked,
    )
}

use std::any::Any;

use ratatui::layout::Rect;
use ratatui::Frame;
use tui_textarea::Input;

use crate::config::{PeriConfig, ThinkingConfig};

use super::panel_component::PanelComponent;
use super::panel_manager::{EventResult, PanelContext, PanelKind};
use super::App;

// ─── AliasTab 枚举 ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum AliasTab {
    Opus,
    Sonnet,
    Haiku,
}

impl AliasTab {
    pub fn label(&self) -> &str {
        match self {
            Self::Opus => "Opus",
            Self::Sonnet => "Sonnet",
            Self::Haiku => "Haiku",
        }
    }

    pub fn to_key(&self) -> &'static str {
        match self {
            Self::Opus => "opus",
            Self::Sonnet => "sonnet",
            Self::Haiku => "haiku",
        }
    }

    pub fn description(&self) -> &str {
        match self {
            Self::Opus => "Most capable for complex work",
            Self::Sonnet => "Balanced performance and speed",
            Self::Haiku => "Fastest for quick answers",
        }
    }
}

// ─── 行索引常量 ─────────────────────────────────────────────────────────────────

pub const ROW_OPUS: usize = 0;
pub const ROW_SONNET: usize = 1;
pub const ROW_HAIKU: usize = 2;
pub const ROW_EFFORT: usize = 3;
pub const ROW_COUNT: usize = 4;

// ─── ModelPanel ─────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct ModelPanel {
    /// 当前激活 Provider 的显示名称
    pub provider_name: String,
    /// 竖向列表光标 (0..ROW_COUNT)
    pub cursor: usize,
    /// 当前选中的级别
    pub active_tab: AliasTab,
    /// Thinking effort 缓冲 "low" / "medium" / "high"
    pub buf_thinking_effort: String,
}

impl ModelPanel {
    pub fn from_config(cfg: &PeriConfig) -> Self {
        let active_tab = match cfg.config.active_alias.as_str() {
            "sonnet" => AliasTab::Sonnet,
            "haiku" => AliasTab::Haiku,
            _ => AliasTab::Opus,
        };

        let provider_name = cfg
            .config
            .providers
            .iter()
            .find(|p| p.id == cfg.config.active_provider_id)
            .map(|p| p.display_name().to_string())
            .unwrap_or_default();

        let cursor = match active_tab {
            AliasTab::Opus => ROW_OPUS,
            AliasTab::Sonnet => ROW_SONNET,
            AliasTab::Haiku => ROW_HAIKU,
        };

        let effort = cfg
            .config
            .thinking
            .as_ref()
            .map(|t| t.effort.clone())
            .unwrap_or_else(|| "high".to_string());

        Self {
            provider_name,
            cursor,
            active_tab,
            buf_thinking_effort: effort,
        }
    }

    /// 上下移动光标（循环）
    pub fn move_cursor(&mut self, delta: i32) {
        if delta > 0 {
            self.cursor = (self.cursor + 1) % ROW_COUNT;
        } else if delta < 0 {
            self.cursor = (self.cursor + ROW_COUNT - 1) % ROW_COUNT;
        }
    }

    /// 循环切换 effort（仅 Effort 行）：medium → high → low → medium
    pub fn cycle_effort(&mut self, reverse: bool) {
        if self.cursor != ROW_EFFORT {
            return;
        }
        if reverse {
            self.buf_thinking_effort = match self.buf_thinking_effort.as_str() {
                "low" => "high".to_string(),
                "high" => "medium".to_string(),
                _ => "low".to_string(),
            };
        } else {
            self.buf_thinking_effort = match self.buf_thinking_effort.as_str() {
                "low" => "medium".to_string(),
                "medium" => "high".to_string(),
                _ => "low".to_string(),
            };
        }
    }

    /// 将面板状态写入 PeriConfig（alias + thinking）
    pub fn apply_to_config(&self, cfg: &mut PeriConfig) {
        cfg.config.active_alias = self.active_tab.to_key().to_string();
        let t = cfg.config.thinking.get_or_insert_with(|| ThinkingConfig {
            enabled: true,
            budget_tokens: 8000,
            effort: self.buf_thinking_effort.clone(),
        });
        t.enabled = true;
        t.effort = self.buf_thinking_effort.clone();
    }
}

// ─── PanelComponent 实现 ──────────────────────────────────────────────────────

impl PanelComponent for ModelPanel {
    fn kind(&self) -> PanelKind {
        PanelKind::Model
    }

    fn handle_key(&mut self, input: Input, ctx: &mut PanelContext<'_>) -> EventResult {
        use tui_textarea::Key;
        match input {
            Input { key: Key::Esc, .. } => EventResult::ClosePanel,
            Input { key: Key::Up, .. } => {
                self.move_cursor(-1);
                EventResult::Consumed
            }
            Input { key: Key::Down, .. } => {
                self.move_cursor(1);
                EventResult::Consumed
            }
            Input {
                key: Key::Char(' ') | Key::Enter,
                ..
            } => match self.cursor {
                ROW_OPUS => {
                    self.active_tab = AliasTab::Opus;
                    Self::apply_and_close(self, ctx);
                    EventResult::ClosePanel
                }
                ROW_SONNET => {
                    self.active_tab = AliasTab::Sonnet;
                    Self::apply_and_close(self, ctx);
                    EventResult::ClosePanel
                }
                ROW_HAIKU => {
                    self.active_tab = AliasTab::Haiku;
                    Self::apply_and_close(self, ctx);
                    EventResult::ClosePanel
                }
                ROW_EFFORT => {
                    self.cycle_effort(false);
                    EventResult::Consumed
                }
                _ => EventResult::Consumed,
            },
            Input { key: Key::Left, .. } => {
                self.cycle_effort(true);
                EventResult::Consumed
            }
            Input {
                key: Key::Right, ..
            } => {
                self.cycle_effort(false);
                EventResult::Consumed
            }
            _ => EventResult::Consumed,
        }
    }

    fn desired_height(&self, _screen_height: u16, _screen_width: u16) -> u16 {
        12
    }

    fn render(&mut self, f: &mut Frame, app: &mut App, area: Rect) {
        crate::ui::main_ui::panels::model::render_model_panel(f, self, app, area);
    }

    fn as_any_ref(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn status_bar_hints(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("\u{2191}\u{2193}", "\u{5bfc}\u{822a}"),
            ("Enter", "\u{786e}\u{8ba4}"),
            ("Space", "\u{9009}\u{62e9}/\u{5207}\u{6362}"),
            ("Esc", "\u{5173}\u{95ed}"),
        ]
    }
}

impl ModelPanel {
    /// 将面板状态写入 config，推送系统消息，更新 provider/model 名称
    fn apply_and_close(panel: &ModelPanel, ctx: &mut PanelContext<'_>) {
        let alias_label = panel.active_tab.label().to_string();
        let effort = panel.buf_thinking_effort.clone();

        let Some(cfg) = ctx.services.peri_config.as_mut() else {
            return;
        };
        panel.apply_to_config(cfg);

        let effort_display = match effort.as_str() {
            "low" => "Low",
            "high" => "High",
            _ => "Medium",
        };

        ctx.session_mgr.sessions[ctx.session_mgr.active]
            .messages
            .view_messages
            .push(crate::app::MessageViewModel::system(format!(
                "\u{6a21}\u{578b}\u{5df2}\u{5207}\u{6362}\u{4e3a}: {} ({} effort)",
                alias_label, effort_display
            )));

        if let Err(e) = App::save_config(cfg, ctx.services.config_path_override.as_deref()) {
            ctx.session_mgr.sessions[ctx.session_mgr.active]
                .messages
                .view_messages
                .push(crate::app::MessageViewModel::system(format!(
                    "\u{914d}\u{7f6e}\u{4fdd}\u{5b58}\u{5931}\u{8d25}: {}",
                    e
                )));
        }

        if let Some(p) = crate::app::agent::LlmProvider::from_config(cfg) {
            ctx.services.provider_name = p.display_name().to_string();
            ctx.services.model_name = p.model_name().to_string();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::types::AppConfig;
    use crate::config::ProviderConfig;

    fn make_config() -> PeriConfig {
        PeriConfig {
            config: AppConfig {
                active_alias: "opus".to_string(),
                active_provider_id: "test".to_string(),
                providers: vec![ProviderConfig {
                    id: "test".to_string(),
                    name: Some("TestProvider".to_string()),
                    ..Default::default()
                }],
                thinking: Some(ThinkingConfig {
                    enabled: false,
                    budget_tokens: 8000,
                    effort: "medium".to_string(),
                }),
                ..Default::default()
            },
        }
    }

    #[test]
    fn test_from_config_defaults() {
        let cfg = make_config();
        let panel = ModelPanel::from_config(&cfg);
        assert_eq!(panel.active_tab, AliasTab::Opus);
        assert_eq!(panel.cursor, ROW_OPUS);
        assert_eq!(panel.provider_name, "TestProvider");
        assert_eq!(panel.buf_thinking_effort, "medium");
    }

    #[test]
    fn test_from_config_sonnet() {
        let mut cfg = make_config();
        cfg.config.active_alias = "sonnet".to_string();
        let panel = ModelPanel::from_config(&cfg);
        assert_eq!(panel.active_tab, AliasTab::Sonnet);
        assert_eq!(panel.cursor, ROW_SONNET);
    }

    #[test]
    fn test_move_cursor_wrap() {
        let cfg = make_config();
        let mut panel = ModelPanel::from_config(&cfg);
        assert_eq!(panel.cursor, ROW_OPUS);
        panel.move_cursor(1);
        assert_eq!(panel.cursor, ROW_SONNET);
        panel.move_cursor(1);
        assert_eq!(panel.cursor, ROW_HAIKU);
        panel.move_cursor(1);
        assert_eq!(panel.cursor, ROW_EFFORT);
        panel.move_cursor(1);
        assert_eq!(panel.cursor, ROW_OPUS);
        panel.move_cursor(-1);
        assert_eq!(panel.cursor, ROW_EFFORT);
    }

    #[test]
    fn test_cycle_effort() {
        let cfg = make_config();
        let mut panel = ModelPanel::from_config(&cfg);
        panel.cursor = ROW_EFFORT;

        assert_eq!(panel.buf_thinking_effort, "medium");
        panel.cycle_effort(false);
        assert_eq!(panel.buf_thinking_effort, "high");
        panel.cycle_effort(false);
        assert_eq!(panel.buf_thinking_effort, "low");
        panel.cycle_effort(false);
        assert_eq!(panel.buf_thinking_effort, "medium");

        panel.cycle_effort(true);
        assert_eq!(panel.buf_thinking_effort, "low");
        panel.cycle_effort(true);
        assert_eq!(panel.buf_thinking_effort, "high");
    }

    #[test]
    fn test_cycle_effort_ignored_on_model_rows() {
        let cfg = make_config();
        let mut panel = ModelPanel::from_config(&cfg);
        assert_eq!(panel.cursor, ROW_OPUS);
        panel.cycle_effort(false);
        assert_eq!(panel.buf_thinking_effort, "medium");
    }

    #[test]
    fn test_apply_to_config() {
        let cfg = make_config();
        let mut panel = ModelPanel::from_config(&cfg);
        panel.active_tab = AliasTab::Sonnet;
        panel.buf_thinking_effort = "high".to_string();

        let mut cfg2 = make_config();
        panel.apply_to_config(&mut cfg2);
        assert_eq!(cfg2.config.active_alias, "sonnet");
        assert!(cfg2.config.thinking.as_ref().unwrap().enabled);
        assert_eq!(cfg2.config.thinking.as_ref().unwrap().effort, "high");
    }

    #[test]
    fn test_apply_to_config_creates_thinking_when_none() {
        let mut cfg = PeriConfig {
            config: AppConfig {
                active_alias: "opus".to_string(),
                active_provider_id: "test".to_string(),
                providers: vec![ProviderConfig {
                    id: "test".to_string(),
                    ..Default::default()
                }],
                thinking: None,
                ..Default::default()
            },
        };
        let panel = ModelPanel::from_config(&cfg);
        panel.apply_to_config(&mut cfg);
        let t = cfg.config.thinking.as_ref().unwrap();
        assert!(t.enabled);
        assert_eq!(t.effort, "high");
    }

    #[test]
    fn test_alias_tab_description() {
        assert_eq!(
            AliasTab::Opus.description(),
            "Most capable for complex work"
        );
        assert_eq!(
            AliasTab::Sonnet.description(),
            "Balanced performance and speed"
        );
        assert_eq!(AliasTab::Haiku.description(), "Fastest for quick answers");
    }
}

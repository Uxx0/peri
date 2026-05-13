use crate::app::status_panel::{StatusPanel, STATUS_TAB_CONTEXT, STATUS_TAB_COST};
use crate::app::App;
use crate::ui::theme;
use perihelion_widgets::{tab_bar::TabBar, BorderedPanel};
use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::Paragraph,
    Frame,
};

pub(crate) fn render_status_panel(f: &mut Frame, panel: &StatusPanel, app: &App, area: Rect) {
    let inner = BorderedPanel::new(Span::styled(
        " Status ",
        Style::default()
            .fg(theme::THINKING)
            .add_modifier(Modifier::BOLD),
    ))
    .border_style(Style::default().fg(theme::BORDER))
    .render(f, area);

    // Tab 栏（1 行）
    let tab_height = 1u16;
    let tab_area = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: tab_height,
    };
    let content_area = Rect {
        x: inner.x,
        y: inner.y + tab_height + 1,
        width: inner.width,
        height: inner.height.saturating_sub(tab_height + 1),
    };

    let mut tab_state = panel.tab.clone();
    f.render_stateful_widget(TabBar::new(), tab_area, &mut tab_state);

    match panel.tab.active() {
        STATUS_TAB_COST => {
            let lines = build_cost_lines(app);
            f.render_widget(Paragraph::new(Text::from(lines)), content_area);
        }
        STATUS_TAB_CONTEXT => {
            render_context_tab(f, app, content_area);
        }
        _ => {}
    }
}

fn build_cost_lines(app: &App) -> Vec<Line<'static>> {
    let tracker = &app.session_mgr.sessions[app.session_mgr.active]
        .agent
        .session_token_tracker;
    let mut lines: Vec<Line<'static>> = Vec::new();

    // 会话时长
    let duration_str = match app.session_mgr.sessions[app.session_mgr.active]
        .agent
        .session_start_time
    {
        Some(start) => {
            let s = start.elapsed().as_secs();
            if s >= 3600 {
                format!("{}h{}m{}s", s / 3600, (s % 3600) / 60, s % 60)
            } else if s >= 60 {
                format!("{}m{}s", s / 60, s % 60)
            } else {
                format!("{}s", s)
            }
        }
        None => "N/A".to_string(),
    };
    lines.push(label_value("会话时长", &duration_str));
    lines.push(Line::from(""));

    // Token 消耗
    lines.push(label_value(
        "输入 Tokens",
        &format_number(tracker.total_input_tokens),
    ));
    lines.push(label_value(
        "输出 Tokens",
        &format_number(tracker.total_output_tokens),
    ));
    lines.push(label_value(
        "Cache 创建",
        &format_number(tracker.total_cache_creation_tokens),
    ));
    lines.push(label_value(
        "Cache 读取",
        &format_number(tracker.total_cache_read_tokens),
    ));
    lines.push(Line::from(""));

    // LLM 调用次数
    lines.push(label_value(
        "LLM 调用次数",
        &tracker.llm_call_count.to_string(),
    ));
    lines.push(Line::from(""));

    // 估算费用
    let cost = estimate_cost(app);
    lines.push(label_value("估算费用", &format!("${:.4}", cost)));
    lines.push(Line::from(""));

    // 当前模型
    lines.push(label_value("当前模型", &app.services.model_name));

    lines
}

fn label_value(label: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("  {:<16}", label),
            Style::default().fg(theme::MUTED),
        ),
        Span::styled(
            value.to_string(),
            Style::default()
                .fg(theme::TEXT)
                .add_modifier(Modifier::BOLD),
        ),
    ])
}

fn format_number(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// 基于模型 alias 的简化费用估算
fn estimate_cost(app: &App) -> f64 {
    let tracker = &app.session_mgr.sessions[app.session_mgr.active]
        .agent
        .session_token_tracker;
    let alias = app
        .services
        .peri_config
        .as_ref()
        .map(|c| c.config.active_alias.as_str())
        .unwrap_or("sonnet");

    let (input_price, output_price) = match alias {
        "opus" => (15.0, 75.0),
        "haiku" => (0.80, 4.0),
        _ => (3.0, 15.0), // sonnet default
    };

    let input_cost = (tracker.total_input_tokens as f64 / 1_000_000.0) * input_price;
    let output_cost = (tracker.total_output_tokens as f64 / 1_000_000.0) * output_price;
    input_cost + output_cost
}

/// 计算一个"漂亮"的上界值（向上取整到 1/2/5 × 10^n）
fn nice_ceil(value: u64) -> u64 {
    if value == 0 {
        return 1;
    }
    let magnitude = 10u64.pow(value.ilog10());
    let normalized = value as f64 / magnitude as f64;
    let nice = if normalized <= 1.0 {
        1.0
    } else if normalized <= 2.0 {
        2.0
    } else if normalized <= 5.0 {
        5.0
    } else {
        10.0
    };
    (nice * magnitude as f64) as u64
}

fn build_bar_chart_lines(
    history: &[rust_create_agent::agent::token::RequestRecord],
    chart_width: usize,
    chart_height: usize,
) -> Vec<Line<'static>> {
    use ratatui::style::Style;
    use ratatui::text::Span;

    if history.is_empty() || chart_height == 0 || chart_width == 0 {
        return vec![];
    }

    let start = history.len().saturating_sub(chart_width);
    let visible = &history[start..];

    let max_input = visible
        .iter()
        .map(|r| r.input_tokens as u64)
        .max()
        .unwrap_or(1);
    let y_max = nice_ceil(max_input);

    let mut lines = Vec::with_capacity(chart_height + 1);

    for row in (0..chart_height).rev() {
        let row_bottom = y_max * row as u64 / chart_height as u64;
        let label = format_number(y_max * (row + 1) as u64 / chart_height as u64);

        let mut spans: Vec<Span> = vec![Span::styled(
            format!("{:>6}┤", label),
            Style::default().fg(theme::MUTED),
        )];

        for record in visible {
            let input = record.input_tokens as u64;
            if input < row_bottom {
                spans.push(Span::raw(" "));
            } else {
                let cache_read = record.cache_read_input_tokens as u64;
                let cache_create = record.cache_creation_input_tokens as u64;
                let cache_top = cache_read + cache_create;

                let color = if row_bottom < cache_read {
                    theme::SAGE
                } else if row_bottom < cache_top {
                    theme::WARNING
                } else {
                    theme::ACCENT
                };
                spans.push(Span::styled("█", Style::default().fg(color)));
            }
        }

        lines.push(Line::from(spans));
    }

    // 底部 x 轴线
    let mut axis_spans: Vec<Span> = vec![Span::styled(
        "     0┼".to_string(),
        Style::default().fg(theme::MUTED),
    )];
    for _ in visible {
        axis_spans.push(Span::styled("─", Style::default().fg(theme::DIM)));
    }
    lines.push(Line::from(axis_spans));

    lines
}

fn build_x_axis_labels(
    _total_len: usize,
    visible_start: usize,
    visible_len: usize,
) -> Line<'static> {
    use ratatui::style::Style;
    use ratatui::text::Span;

    let label_every = if visible_len <= 10 {
        1
    } else if visible_len <= 20 {
        2
    } else if visible_len <= 50 {
        5
    } else {
        10
    };

    let mut spans: Vec<Span> = vec![Span::raw("       ")];

    for i in 0..visible_len {
        let req_num = visible_start + i + 1;
        if (req_num - 1).is_multiple_of(label_every) || i == visible_len - 1 {
            let s = if req_num <= 9 {
                format!("{} ", req_num)
            } else {
                req_num.to_string().chars().take(2).collect::<String>()
            };
            spans.push(Span::styled(s, Style::default().fg(theme::MUTED)));
        } else {
            spans.push(Span::raw("  "));
        }
    }

    Line::from(spans)
}

fn build_context_summary(app: &App) -> Line<'static> {
    use ratatui::style::{Modifier, Style};
    use ratatui::text::Span;

    let tracker = &app.session_mgr.sessions[app.session_mgr.active]
        .agent
        .session_token_tracker;
    let context_window = app.session_mgr.sessions[app.session_mgr.active]
        .agent
        .context_window;
    let msg_count = app.session_mgr.sessions[app.session_mgr.active]
        .agent
        .agent_state_messages
        .len();
    let tool_count = app.session_mgr.sessions[app.session_mgr.active]
        .agent
        .tool_call_count;

    let used = tracker.estimated_context_tokens().unwrap_or(0);
    let pct = tracker
        .context_usage_percent(context_window)
        .map(|p| format!("{:.1}%", p))
        .unwrap_or_else(|| "N/A".to_string());

    Line::from(vec![
        Span::styled("  上下文: ", Style::default().fg(theme::MUTED)),
        Span::styled(
            format_number(context_window as u64),
            Style::default()
                .fg(theme::TEXT)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" | 已用: ", Style::default().fg(theme::MUTED)),
        Span::styled(
            format!("{} ({})", format_number(used), pct),
            Style::default()
                .fg(theme::TEXT)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" | 消息: ", Style::default().fg(theme::MUTED)),
        Span::styled(
            msg_count.to_string(),
            Style::default()
                .fg(theme::TEXT)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" | 工具: ", Style::default().fg(theme::MUTED)),
        Span::styled(
            tool_count.to_string(),
            Style::default()
                .fg(theme::TEXT)
                .add_modifier(Modifier::BOLD),
        ),
    ])
}

fn render_context_tab(f: &mut Frame, app: &App, area: Rect) {
    use ratatui::style::Style;
    use ratatui::text::{Line, Span, Text};
    use ratatui::widgets::Paragraph;

    let history = &app.session_mgr.sessions[app.session_mgr.active]
        .agent
        .session_token_tracker
        .request_history;

    if history.is_empty() {
        let mut lines = vec![build_context_summary(app)];
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "  暂无请求数据",
            Style::default().fg(theme::MUTED),
        )));
        f.render_widget(Paragraph::new(Text::from(lines)), area);
        return;
    }

    let summary_h = 2u16;
    let legend_h = 1u16;
    let x_axis_h = 1u16;
    let spark_title_h = 1u16;
    let spark_h = 3u16;
    let blanks = 2u16;

    let chart_h = area
        .height
        .saturating_sub(summary_h + legend_h + x_axis_h + spark_title_h + spark_h + blanks);

    let skip_chart = chart_h < 3;
    let actual_blanks = if skip_chart { 1 } else { blanks };

    let mut y = area.y;

    // 摘要行
    f.render_widget(
        Paragraph::new(Text::from(vec![build_context_summary(app)])),
        Rect {
            x: area.x,
            y,
            width: area.width,
            height: 1,
        },
    );
    y += summary_h;

    if !skip_chart {
        // 图例
        f.render_widget(
            Paragraph::new(Text::from(vec![Line::from(vec![
                Span::styled("  Input Tokens", Style::default().fg(theme::MUTED)),
                Span::styled("  █cache_read", Style::default().fg(theme::SAGE)),
                Span::styled(" █cache_creation", Style::default().fg(theme::WARNING)),
                Span::styled(" █raw", Style::default().fg(theme::ACCENT)),
            ])])),
            Rect {
                x: area.x,
                y,
                width: area.width,
                height: 1,
            },
        );
        y += legend_h;

        // 柱状图
        let chart_width = (area.width as usize).saturating_sub(7);
        let visible_start = history.len().saturating_sub(chart_width);
        let chart_lines = build_bar_chart_lines(history, chart_width, chart_h as usize);
        f.render_widget(
            Paragraph::new(Text::from(chart_lines)),
            Rect {
                x: area.x,
                y,
                width: area.width,
                height: chart_h,
            },
        );
        y += chart_h;

        // x 轴标签
        let visible_len = history.len() - visible_start;
        f.render_widget(
            Paragraph::new(Text::from(vec![build_x_axis_labels(
                history.len(),
                visible_start,
                visible_len,
            )])),
            Rect {
                x: area.x,
                y,
                width: area.width,
                height: 1,
            },
        );
        y += x_axis_h + 1;
    } else {
        y += actual_blanks;
    }

    // Sparkline 标题
    f.render_widget(
        Paragraph::new(Text::from(vec![Line::from(Span::styled(
            "  Cache Hit Rate",
            Style::default().fg(theme::MUTED),
        ))])),
        Rect {
            x: area.x,
            y,
            width: area.width,
            height: 1,
        },
    );
    y += spark_title_h;

    // Sparkline
    let spark_data: Vec<u64> = history
        .iter()
        .map(|r| (r.cache_hit_rate() * 100.0) as u64)
        .collect();
    let sparkline = ratatui::widgets::Sparkline::default()
        .data(spark_data)
        .max(100)
        .style(Style::default().fg(theme::THINKING));
    f.render_widget(
        sparkline,
        Rect {
            x: area.x + 2,
            y,
            width: area.width.saturating_sub(4),
            height: spark_h,
        },
    );
}

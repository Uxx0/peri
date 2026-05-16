use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::HashMap;

/// 顶层包装（与 ~/.peri/settings.json 的 { "config": {...} } 对应）
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PeriConfig {
    #[serde(rename = "$schema", skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    #[serde(default)]
    pub config: AppConfig,
}

/// Provider 内的三级别模型名映射
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProviderModels {
    #[serde(default)]
    pub opus: String,
    #[serde(default)]
    pub sonnet: String,
    #[serde(default)]
    pub haiku: String,
}

impl ProviderModels {
    /// 按 alias 名（大小写不敏感）获取对应模型名
    pub fn get_model(&self, alias: &str) -> Option<&str> {
        match alias.to_lowercase().as_str() {
            "opus" => Some(&self.opus),
            "sonnet" => Some(&self.sonnet),
            "haiku" => Some(&self.haiku),
            _ => None,
        }
    }
}

fn default_alias() -> String {
    "opus".to_string()
}

/// Thinking / 推理模式配置
///
/// 对两个 provider 的映射：
/// - Anthropic → `extended_thinking` + `budget_tokens` + `output_config.effort`
/// - OpenAI    → `reasoning_effort`（直接使用 effort 字段）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThinkingConfig {
    /// 是否启用 thinking
    #[serde(default)]
    pub enabled: bool,
    /// 推理 token 预算（Anthropic 直接使用；OpenAI 按区段转换为 effort 等级）
    /// - OpenAI 映射：0 = "low", 1-7999 = "medium", ≥8000 = "high"
    /// - Anthropic：直接传给 extended_thinking.budget_tokens
    #[serde(default = "default_budget_tokens")]
    pub budget_tokens: u32,
    /// 思考强度 "low" / "medium" / "high"
    /// - Anthropic → `output_config.effort`
    /// - OpenAI → `reasoning_effort`
    #[serde(default = "default_effort")]
    pub effort: String,
    /// 最大输出 token 数（对应 API 的 max_tokens 参数）
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
}

fn default_budget_tokens() -> u32 {
    8000
}

fn default_effort() -> String {
    "high".to_string()
}

fn default_max_tokens() -> u32 {
    32000
}

impl ThinkingConfig {
    /// 将 budget_tokens 映射到 OpenAI reasoning_effort 字符串（已废弃，直接使用 effort 字段）
    pub fn openai_effort(&self) -> &str {
        &self.effort
    }

    /// effort 循环切换：low → medium → high → xhigh → max → low
    pub fn next_effort(&self) -> &'static str {
        match self.effort.as_str() {
            "low" => "medium",
            "medium" => "high",
            "high" => "xhigh",
            "xhigh" => "max",
            _ => "low",
        }
    }

    /// effort 反向循环切换：low → max → xhigh → high → medium → low
    pub fn prev_effort(&self) -> &'static str {
        match self.effort.as_str() {
            "low" => "max",
            "max" => "xhigh",
            "xhigh" => "high",
            "high" => "medium",
            _ => "low",
        }
    }
}

/// 应用配置（只映射用到的字段，其余字段用 extra 保留）
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AppConfig {
    /// 当前激活的模型别名（"opus" | "sonnet" | "haiku"）
    #[serde(default = "default_alias")]
    pub active_alias: String,
    /// 当前激活的 provider ID（直接指向 providers 列表中的某个 Provider）
    #[serde(default)]
    pub active_provider_id: String,
    #[serde(default)]
    pub providers: Vec<ProviderConfig>,
    /// 全局 skills 目录路径
    #[serde(default, alias = "skillsDir")]
    pub skills_dir: Option<String>,
    /// Thinking / 推理模式配置
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<ThinkingConfig>,
    /// 环境变量注入（扁平键值对）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<HashMap<String, String>>,
    /// Compact 系统配置（缺失时使用 CompactConfig::default()）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compact: Option<peri_agent::agent::CompactConfig>,
    /// UI 语言，"auto" 自动探测系统语言
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// 系统提示词 persona 覆盖
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona: Option<String>,
    /// 系统提示词 tone 覆盖
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tone: Option<String>,
    /// CLAUDE.md 排除 glob 模式列表
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_md_excludes: Option<Vec<String>>,
    /// 主动性级别（low/medium/high）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proactiveness: Option<String>,
    /// 保留未知字段，写回时不丢失
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// 单个 Provider 配置
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProviderConfig {
    #[serde(default)]
    pub id: String,
    /// "openai" | "anthropic" 等
    #[serde(rename = "type", default)]
    pub provider_type: String,
    #[serde(rename = "apiKey", default)]
    pub api_key: String,
    #[serde(rename = "baseUrl", default)]
    pub base_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default)]
    pub models: ProviderModels,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl ProviderConfig {
    pub fn display_name(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.id)
    }
}

#[cfg(test)]
#[path = "types_test.rs"]
mod tests;

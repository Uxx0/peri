use async_trait::async_trait;
use rust_create_agent::agent::state::State;
use rust_create_agent::middleware::r#trait::Middleware;
use rust_create_agent::tools::BaseTool;
use serde_json::Value;
use std::net::IpAddr;
use tokio::time::{timeout, Duration};
use url::Url;

/// URL 安全校验，防止 SSRF
fn validate_url(url: &str) -> Result<Url, String> {
    let parsed = Url::parse(url).map_err(|e| format!("无效的 URL: {e}"))?;

    // 仅允许 http/https
    match parsed.scheme() {
        "http" | "https" => {}
        _ => return Err("仅支持 http/https 协议".to_string()),
    }

    // 检查主机名
    match parsed.host() {
        None => return Err("URL 缺少主机名".to_string()),
        Some(url::Host::Domain(_)) => {
            // 域名不做 DNS 解析，直接通过
        }
        Some(url::Host::Ipv4(ip)) => {
            if ip.is_loopback() {
                return Err("禁止访问回环地址".to_string());
            }
            if ip.is_private() {
                return Err("禁止访问私有地址".to_string());
            }
            if ip.is_link_local() {
                return Err("禁止访问链路本地地址".to_string());
            }
            if ip == IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED) {
                return Err("禁止访问未指定地址".to_string());
            }
        }
        Some(url::Host::Ipv6(ip)) => {
            if ip.is_loopback() {
                return Err("禁止访问回环地址".to_string());
            }
            if ip.is_unicast_link_local() {
                return Err("禁止访问链路本地地址".to_string());
            }
            // IPv6 私有地址：fc00::/7 (unique local)
            let segments = ip.segments();
            if (segments[0] & 0xfe00) == 0xfc00 {
                return Err("禁止访问私有地址".to_string());
            }
            if ip == IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED) {
                return Err("禁止访问未指定地址".to_string());
            }
        }
    }

    Ok(parsed)
}

/// HTML 转纯文本（120 列宽度）
fn html_to_text(html: &str) -> String {
    html2text::from_read(html.as_bytes(), 120).unwrap_or_default()
}

/// 按行数截断内容
fn truncate_content(content: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = content.lines().collect();
    if lines.len() <= max_lines {
        content.to_string()
    } else {
        let truncated: String = lines[..max_lines].join("\n");
        format!("{truncated}\n[内容已截断，原始内容共 {} 行]", lines.len())
    }
}

/// 网络来源可信度警告（附在 WebFetch/WebSearch 输出前）
const WEB_CREDIBILITY_WARNING: &str =
    "⚠ Web content may be inaccurate or outdated. Verify critical information before relying on it.\n\n";

/// WebFetch 工具 — 抓取 URL 并返回文本内容
pub struct WebFetchTool;

const WEB_FETCH_DESCRIPTION: &str = r#"Fetches a web page by URL and returns its content as text.

Usage:
- Only http:// and https:// URLs are allowed
- HTML pages are converted to readable text; JSON is pretty-printed; plain text is returned as-is
- Binary content returns only type and size information
- Results are truncated at 2000 lines
- An optional 'prompt' parameter provides guidance for how to use the fetched content

Security:
- Internal/private/loopback IP addresses are blocked
- Maximum response size: 10MB
- Request timeout: 30 seconds
- Maximum redirects: 5"#;

/// 响应体大小上限
const MAX_RESPONSE_BYTES: u64 = 10 * 1024 * 1024; // 10MB

impl WebFetchTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for WebFetchTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl BaseTool for WebFetchTool {
    fn name(&self) -> &str {
        "WebFetch"
    }

    fn description(&self) -> &str {
        WEB_FETCH_DESCRIPTION
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "要抓取的完整 URL（http/https）"
                },
                "prompt": {
                    "type": "string",
                    "description": "可选。提取内容的指导提示，附在结果前供 LLM 参考"
                }
            },
            "required": ["url"]
        })
    }

    async fn invoke(
        &self,
        input: Value,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let url = input["url"].as_str().ok_or("Missing url parameter")?;
        let prompt = input["prompt"].as_str();

        let parsed_url = validate_url(url)?;

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::limited(5))
            .user_agent("perihelion/1.0")
            .build()
            .map_err(|e| format!("Failed to build HTTP client: {e}"))?;

        let resp = client
            .get(parsed_url)
            .send()
            .await
            .map_err(|e| format!("Request failed: {e}"))?;

        // 检查响应体大小
        if let Some(len) = resp.content_length() {
            if len > MAX_RESPONSE_BYTES {
                return Ok(format!("响应体超过 10MB 限制（{len} bytes）"));
            }
        }

        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        // 读取 body（带超时）
        let body = timeout(Duration::from_secs(30), resp.text())
            .await
            .map_err(|_| "读取响应体超时（30秒）")?
            .map_err(|e| format!("读取响应体失败: {e}"))?;

        // 实际大小检查（当 content-length 不可用时）
        if body.len() as u64 > MAX_RESPONSE_BYTES {
            return Ok(format!("响应体超过 10MB 限制（{} bytes）", body.len()));
        }

        let processed = if content_type.contains("text/html") {
            html_to_text(&body)
        } else if content_type.contains("text/plain") {
            body
        } else if content_type.contains("application/json") {
            match serde_json::from_str::<Value>(&body) {
                Ok(v) => serde_json::to_string_pretty(&v).unwrap_or(body),
                Err(_) => body,
            }
        } else {
            format!(
                "Content-Type: {content_type}\nSize: {} bytes\n（不支持的内容类型）",
                body.len()
            )
        };

        let truncated = truncate_content(&processed, 2000);

        let result = match prompt {
            Some(p) => format!("{WEB_CREDIBILITY_WARNING}提示: {p}\n\n{truncated}"),
            None => format!("{WEB_CREDIBILITY_WARNING}{truncated}"),
        };

        Ok(result)
    }
}

/// 搜索结果
struct SearchResult {
    title: String,
    url: String,
    snippet: Option<String>,
}

const WEBSEARCH_DESCRIPTION: &str = r#"Search the web using Bing search engine.

Usage:
- Provide a search query to find relevant web pages
- Returns results as a numbered Markdown list with titles, URLs, and text snippets
- Each result's text is truncated to 500 characters
- No API key required — uses Bing web search directly

IMPORTANT:
- Results may be irrelevant or low quality — always verify information before using it
- If results don't contain the information you need, do NOT fabricate or guess values
- Consider using WebFetch to directly access a specific URL for accurate information

Parameters:
- query (required): Search keywords
- num_results (optional): Number of results, default 10, max 20"#;

/// 单条结果文本截断上限（字符数）
const MAX_RESULT_TEXT_CHARS: usize = 500;

/// WebSearch 工具 — 通过 Bing 搜索网页
pub struct WebSearchTool;

/// Browser-like headers to avoid Bing's anti-bot JS-rendered response.
/// Header names MUST be lowercase — `http::HeaderName::from_static` panics otherwise.
/// Note: `accept-encoding` is omitted — reqwest handles decompression automatically
/// when its `gzip`/`brotli`/`deflate` features are enabled.
const BROWSER_HEADERS: &[(&str, &str)] = &[
    ("user-agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36 Edg/131.0.0.0"),
    ("accept", "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8"),
    ("accept-language", "en-US,en;q=0.9"),
    ("cache-control", "no-cache"),
    ("pragma", "no-cache"),
    ("sec-ch-ua", r#""Microsoft Edge";v="131", "Chromium";v="131", "Not_A Brand";v="24""#),
    ("sec-ch-ua-mobile", "?0"),
    ("sec-ch-ua-platform", r#""macOS""#),
    ("sec-fetch-dest", "document"),
    ("sec-fetch-mode", "navigate"),
    ("sec-fetch-site", "none"),
    ("sec-fetch-user", "?1"),
    ("upgrade-insecure-requests", "1"),
];

impl WebSearchTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for WebSearchTool {
    fn default() -> Self {
        Self::new()
    }
}

/// HTML 实体解码（简化版，覆盖常见实体）
fn decode_html_entities(text: &str) -> String {
    let mut result = text.to_string();
    // 数字实体 &#1234; 和 &#x1F600;
    let re = regex::Regex::new(r"&#(x?[0-9a-fA-F]+);").unwrap();
    result = re
        .replace_all(&result, |caps: &regex::Captures| {
            let s = &caps[1];
            if let Some(hex) = s.strip_prefix('x') {
                u32::from_str_radix(hex, 16)
                    .ok()
                    .and_then(char::from_u32)
                    .map_or(String::new(), |c| c.to_string())
            } else {
                s.parse::<u32>()
                    .ok()
                    .and_then(char::from_u32)
                    .map_or(String::new(), |c| c.to_string())
            }
        })
        .to_string();
    // 命名实体
    result = result.replace("&amp;", "&");
    result = result.replace("&lt;", "<");
    result = result.replace("&gt;", ">");
    result = result.replace("&quot;", "\"");
    result = result.replace("&#39;", "'");
    result = result.replace("&apos;", "'");
    result = result.replace("&nbsp;", " ");
    result
}

/// 从 HTML 块中提取文本（去除所有标签）
fn strip_html_tags(html: &str) -> String {
    let re = regex::Regex::new(r"<[^>]+>").unwrap();
    re.replace_all(html, "").to_string()
}

/// 解析 Bing 重定向 URL，返回实际目标 URL
fn resolve_bing_url(raw_url: &str) -> Option<String> {
    // 跳过相对/锚点链接
    if raw_url.starts_with('/') || raw_url.starts_with('#') {
        return None;
    }

    // 尝试从 Bing 重定向 URL 中提取 u 参数
    // Bing 格式: https://www.bing.com/ck/a?...&u=a1aHR0cHM6Ly9leGFtcGxlLmNvbQ...
    // u 参数前缀: a1=https, a0=http
    if let Some(u_match) = raw_url.find("?u=").or_else(|| raw_url.find("&u=")) {
        let encoded = &raw_url[u_match + 3..];
        let encoded = encoded.split('&').next().unwrap_or(encoded);
        if encoded.len() >= 3 {
            let b64 = &encoded[2..];
            // Base64url decode
            let padded = b64.replace('-', "+").replace('_', "/");
            if let Ok(decoded) =
                base64::Engine::decode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, padded)
            {
                if let Ok(decoded_str) = String::from_utf8(decoded) {
                    if decoded_str.starts_with("http") {
                        return Some(decoded_str);
                    }
                }
            }
        }
    }

    // 直接外部 URL（非 Bing 内部页面）
    if !raw_url.contains("bing.com") {
        return Some(raw_url.to_string());
    }

    None
}

/// 从 Bing HTML 中提取搜索结果
fn extract_bing_results(html: &str) -> Vec<SearchResult> {
    let mut results = Vec::new();

    // 匹配 <li class="b_algo"> 块
    let block_re = regex::Regex::new(r#"(?i)<li\s+class="b_algo"[^>]*>([\s\S]*?)</li>"#).unwrap();

    // 从 <h2><a href="...">...</a></h2> 中提取链接
    let link_re =
        regex::Regex::new(r#"(?i)<h2[^>]*>\s*<a[^>]+href="([^"]+)"[^>]*>([\s\S]*?)</a>"#).unwrap();

    for caps in block_re.captures_iter(html) {
        let block = &caps[1];

        if let Some(link_caps) = link_re.captures(block) {
            let raw_url = decode_html_entities(&link_caps[1]);
            let title_html = &link_caps[2];

            let url = match resolve_bing_url(&raw_url) {
                Some(u) => u,
                None => continue,
            };

            let title = strip_html_tags(title_html).trim().to_string();
            if title.is_empty() {
                continue;
            }

            // 提取摘要：优先 b_lineclamp → b_caption <p> → b_caption fallback
            let snippet = extract_snippet(block);

            results.push(SearchResult {
                title,
                url,
                snippet,
            });
        }
    }

    results
}

fn extract_snippet(block: &str) -> Option<String> {
    // 1. <p class="b_lineclamp...">
    let re1 =
        regex::Regex::new(r#"(?i)<p[^>]*class="b_lineclamp[^"]*"[^>]*>([\s\S]*?)</p>"#).unwrap();
    if let Some(caps) = re1.captures(block) {
        let text = strip_html_tags(&caps[1]).trim().to_string();
        if !text.is_empty() {
            return Some(decode_html_entities(&text));
        }
    }

    // 2. <p> inside b_caption
    let re2 = regex::Regex::new(
        r#"(?i)<div[^>]*class="b_caption[^"]*"[^>]*>[\s\S]*?<p[^>]*>([\s\S]*?)</p>"#,
    )
    .unwrap();
    if let Some(caps) = re2.captures(block) {
        let text = strip_html_tags(&caps[1]).trim().to_string();
        if !text.is_empty() {
            return Some(decode_html_entities(&text));
        }
    }

    // 3. Fallback: any text inside b_caption <div>
    let re3 =
        regex::Regex::new(r#"(?i)<div[^>]*class="b_caption[^"]*"[^>]*>([\s\S]*?)</div>"#).unwrap();
    if let Some(caps) = re3.captures(block) {
        let text = strip_html_tags(&caps[1]).trim().to_string();
        if !text.is_empty() {
            return Some(decode_html_entities(&text));
        }
    }

    None
}

/// 将搜索结果格式化为 Markdown 编号列表
fn format_search_results(results: &[SearchResult]) -> String {
    if results.is_empty() {
        return format!("{WEB_CREDIBILITY_WARNING}No search results found.");
    }

    let mut output = format!("{WEB_CREDIBILITY_WARNING}## Search Results\n\n");
    for (i, r) in results.iter().enumerate() {
        output.push_str(&format!("{}. **{}** ({})\n", i + 1, r.title, r.url));
        if let Some(snippet) = &r.snippet {
            let truncated: String = snippet.chars().take(MAX_RESULT_TEXT_CHARS).collect();
            output.push_str(&format!("   {}\n\n", truncated.trim()));
        } else {
            output.push('\n');
        }
    }
    output
}

#[async_trait]
impl BaseTool for WebSearchTool {
    fn name(&self) -> &str {
        "WebSearch"
    }

    fn description(&self) -> &str {
        WEBSEARCH_DESCRIPTION
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search keywords"
                },
                "num_results": {
                    "type": "integer",
                    "description": "Number of results, default 10, max 20"
                }
            },
            "required": ["query"]
        })
    }

    async fn invoke(
        &self,
        input: Value,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let query = input["query"]
            .as_str()
            .ok_or("Missing required parameter: query")?;
        let num_results = input["num_results"].as_u64().unwrap_or(10).clamp(1, 20) as usize;

        let search_url = format!(
            "https://www.bing.com/search?q={}&setmkt=en-US",
            urlencoding::encode(query)
        );

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| format!("Failed to build HTTP client: {e}"))?;

        let mut headers = reqwest::header::HeaderMap::new();
        for (k, v) in BROWSER_HEADERS {
            headers.insert(
                reqwest::header::HeaderName::from_static(k),
                reqwest::header::HeaderValue::from_static(v),
            );
        }

        let resp = client
            .get(&search_url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| format!("Bing search request failed: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            return Err(format!("Bing search returned HTTP {status}").into());
        }

        let html = resp
            .text()
            .await
            .map_err(|e| format!("Failed to read Bing response: {e}"))?;

        let mut results = extract_bing_results(&html);
        results.truncate(num_results);

        Ok(format_search_results(&results))
    }
}

/// Web 中间件，提供 WebFetch 和 WebSearch 工具
pub struct WebMiddleware;

impl WebMiddleware {
    pub fn new() -> Self {
        Self
    }
}

impl Default for WebMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl<S: State> Middleware<S> for WebMiddleware {
    fn name(&self) -> &str {
        "WebMiddleware"
    }

    fn collect_tools(&self, _cwd: &str) -> Vec<Box<dyn BaseTool>> {
        vec![
            Box::new(WebFetchTool::new()),
            Box::new(WebSearchTool::new()),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_url_rejects_ftp() {
        let err = validate_url("ftp://example.com").unwrap_err();
        assert!(err.contains("仅支持 http/https"), "实际: {err}");
    }

    #[test]
    fn test_validate_url_rejects_localhost() {
        let err = validate_url("http://127.0.0.1/test").unwrap_err();
        assert!(err.contains("回环地址"), "实际: {err}");
    }

    #[test]
    fn test_validate_url_rejects_private_ip() {
        let err = validate_url("http://192.168.1.1/test").unwrap_err();
        assert!(err.contains("私有地址"), "实际: {err}");
    }

    #[test]
    fn test_validate_url_rejects_link_local() {
        let err = validate_url("http://169.254.1.1/test").unwrap_err();
        assert!(err.contains("链路本地"), "实际: {err}");
    }

    #[test]
    fn test_validate_url_accepts_https() {
        assert!(validate_url("https://example.com/page").is_ok());
    }

    #[test]
    fn test_validate_url_rejects_invalid_url() {
        let err = validate_url("not-a-url").unwrap_err();
        assert!(err.contains("无效的 URL"), "实际: {err}");
    }

    #[test]
    fn test_truncate_content_no_truncation() {
        let lines: Vec<String> = (0..10).map(|i| format!("line {i}")).collect();
        let input = lines.join("\n");
        assert_eq!(truncate_content(&input, 2000), input);
    }

    #[test]
    fn test_truncate_content_with_truncation() {
        let lines: Vec<String> = (0..3000).map(|i| format!("line {i}")).collect();
        let input = lines.join("\n");
        let result = truncate_content(&input, 2000);
        assert!(result.contains("[内容已截断，原始内容共 3000 行]"));
        assert!(result.contains("line 0"));
        assert!(result.contains("line 1999"));
        assert!(!result.contains("line 2000"));
    }

    #[test]
    fn test_html_to_text_basic() {
        let result = html_to_text("<p>Hello</p>");
        assert!(result.contains("Hello"), "实际: {result}");
    }

    #[test]
    fn test_tool_name_is_web_fetch() {
        assert_eq!(WebFetchTool::new().name(), "WebFetch");
    }

    #[test]
    fn test_tool_parameters_required_url() {
        let params = WebFetchTool::new().parameters();
        let required = params["required"].as_array().unwrap();
        assert!(required.contains(&Value::String("url".to_string())));
    }

    // --- WebSearchTool tests ---

    #[test]
    fn test_websearch_name() {
        assert_eq!(WebSearchTool::new().name(), "WebSearch");
    }

    #[test]
    fn test_websearch_parameters_required() {
        let params = WebSearchTool::new().parameters();
        let required = params["required"].as_array().unwrap();
        assert!(required.contains(&Value::String("query".to_string())));
    }

    #[test]
    fn test_format_search_results_empty() {
        let result = format_search_results(&[]);
        assert!(result.contains("No search results found."));
        assert!(result.contains("Web content may be inaccurate"));
    }

    #[test]
    fn test_format_search_results_with_snippet() {
        let results = vec![
            SearchResult {
                title: "Test Page".to_string(),
                url: "https://example.com".to_string(),
                snippet: Some("A sample snippet.".to_string()),
            },
            SearchResult {
                title: "Another Page".to_string(),
                url: "https://example.org".to_string(),
                snippet: Some("Another snippet here.".to_string()),
            },
        ];
        let output = format_search_results(&results);
        assert!(output.contains("## Search Results"));
        assert!(output.contains("1. **Test Page** (https://example.com)"));
        assert!(output.contains("2. **Another Page** (https://example.org)"));
        assert!(output.contains("A sample snippet."));
    }

    #[test]
    fn test_format_search_results_text_truncation() {
        let long_text = "x".repeat(600);
        let results = vec![SearchResult {
            title: "Long Text".to_string(),
            url: "https://example.com".to_string(),
            snippet: Some(long_text),
        }];
        let output = format_search_results(&results);
        let snippet_start = output.find("   ").unwrap() + 3;
        let snippet_end = output[snippet_start..].find("\n\n").unwrap();
        let snippet = &output[snippet_start..snippet_start + snippet_end];
        assert_eq!(snippet.chars().count(), 500);
    }

    #[test]
    fn test_format_search_results_no_snippet() {
        let results = vec![SearchResult {
            title: "No Snippet".to_string(),
            url: "https://example.com".to_string(),
            snippet: None,
        }];
        let output = format_search_results(&results);
        assert!(output.contains("**No Snippet** (https://example.com)"));
        assert!(!output.contains("   "));
    }

    #[tokio::test]
    async fn test_websearch_missing_query() {
        let tool = WebSearchTool::new();
        let result = tool.invoke(serde_json::json!({})).await;
        let err = result.unwrap_err();
        assert!(
            err.to_string()
                .contains("Missing required parameter: query"),
            "实际: {err}"
        );
    }

    // --- Bing-specific tests ---

    #[test]
    fn test_resolve_bing_url_direct_external() {
        assert_eq!(
            resolve_bing_url("https://example.com/page"),
            Some("https://example.com/page".to_string())
        );
    }

    #[test]
    fn test_resolve_bing_url_skips_relative() {
        assert_eq!(resolve_bing_url("/search?q=test"), None);
        assert_eq!(resolve_bing_url("#fragment"), None);
    }

    #[test]
    fn test_resolve_bing_url_skips_bing_internal() {
        assert_eq!(resolve_bing_url("https://www.bing.com/search?q=test"), None);
    }

    #[test]
    fn test_resolve_bing_url_redirect() {
        // Build a valid Bing redirect: u=a1 + base64("https://example.com")
        let target = "https://example.com";
        let b64 = base64::Engine::encode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            target.as_bytes(),
        );
        let redirect_url = format!("https://www.bing.com/ck/a?u=a1{b64}");
        assert_eq!(resolve_bing_url(&redirect_url), Some(target.to_string()));
    }

    #[test]
    fn test_decode_html_entities() {
        assert_eq!(decode_html_entities("&amp;test&lt;"), "&test<");
        assert_eq!(decode_html_entities("&#39;hello&#39;"), "'hello'");
        assert_eq!(decode_html_entities("normal text"), "normal text");
    }

    #[test]
    fn test_extract_bing_results_from_html() {
        let html = r#"<ol id="b_results"><li class="b_algo"><h2><a href="https://example.com/test">Example Title</a></h2><div class="b_caption"><p class="b_lineclamp">This is a test snippet.</p></div></li><li class="b_algo"><h2><a href="https://other.org/page">Other Title</a></h2><div class="b_caption"><p>Another snippet.</p></div></li></ol>"#;
        let results = extract_bing_results(html);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "Example Title");
        assert_eq!(results[0].url, "https://example.com/test");
        assert_eq!(
            results[0].snippet.as_deref(),
            Some("This is a test snippet.")
        );
        assert_eq!(results[1].title, "Other Title");
        assert_eq!(results[1].url, "https://other.org/page");
    }

    #[test]
    fn test_extract_bing_results_empty() {
        let html = "<html><body>No results here</body></html>";
        let results = extract_bing_results(html);
        assert!(results.is_empty());
    }
}

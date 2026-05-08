use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::Utc;
use std::path::PathBuf;
use tokio::fs;
use tokio::io::AsyncWriteExt;

use crate::messages::BaseMessage;
use crate::thread::{ThreadId, ThreadMeta, ThreadStore};

/// 基于文件系统的 ThreadStore 实现
///
/// 目录结构：
/// ```text
/// <base_dir>/
///   index.json                 # 所有 thread 的摘要索引
///   <thread_id>/
///     meta.json                # 单个 thread 的完整元数据
///     messages.jsonl           # 消息流，每行一条 JSON
/// ```
pub struct FilesystemThreadStore {
    base_dir: PathBuf,
}

impl FilesystemThreadStore {
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self {
            base_dir: base_dir.into(),
        }
    }

    /// 使用默认路径 `~/.peri-core/threads/` 创建
    pub fn default_path() -> Result<Self> {
        let dir = dirs_next::home_dir()
            .context("无法获取 home 目录")?
            .join(".peri-core")
            .join("threads");
        Ok(Self::new(dir))
    }

    fn thread_dir(&self, id: &ThreadId) -> PathBuf {
        self.base_dir.join(id)
    }

    fn meta_path(&self, id: &ThreadId) -> PathBuf {
        self.thread_dir(id).join("meta.json")
    }

    fn messages_path(&self, id: &ThreadId) -> PathBuf {
        self.thread_dir(id).join("messages.jsonl")
    }

    fn index_path(&self) -> PathBuf {
        self.base_dir.join("index.json")
    }

    /// 读取全局 index，不存在时返回空列表
    async fn read_index(&self) -> Result<Vec<ThreadMeta>> {
        let path = self.index_path();
        if !path.exists() {
            return Ok(vec![]);
        }
        let raw = fs::read_to_string(&path).await?;
        let metas: Vec<ThreadMeta> = serde_json::from_str(&raw)?;
        Ok(metas)
    }

    /// 将 metas 写入 index.json
    async fn write_index(&self, metas: &[ThreadMeta]) -> Result<()> {
        fs::create_dir_all(&self.base_dir).await?;
        let json = serde_json::to_string_pretty(metas)?;
        fs::write(self.index_path(), json).await?;
        Ok(())
    }

    /// 在 index 中更新或插入一条摘要
    async fn upsert_index(&self, meta: &ThreadMeta) -> Result<()> {
        let mut metas = self.read_index().await?;
        if let Some(pos) = metas.iter().position(|m| m.id == meta.id) {
            metas[pos] = meta.clone();
        } else {
            metas.push(meta.clone());
        }
        // 按 updated_at 降序排列
        metas.sort_by_key(|b| std::cmp::Reverse(b.updated_at));
        self.write_index(&metas).await
    }
}

#[async_trait]
impl ThreadStore for FilesystemThreadStore {
    async fn create_thread(&self, meta: ThreadMeta) -> Result<ThreadId> {
        let id = meta.id.clone();
        fs::create_dir_all(self.thread_dir(&id)).await?;
        let json = serde_json::to_string_pretty(&meta)?;
        fs::write(self.meta_path(&id), json).await?;
        // 创建空的 messages.jsonl
        fs::write(self.messages_path(&id), b"").await?;
        self.upsert_index(&meta).await?;
        Ok(id)
    }

    async fn append_messages(&self, id: &ThreadId, msgs: &[BaseMessage]) -> Result<()> {
        if msgs.is_empty() {
            return Ok(());
        }
        let path = self.messages_path(id);
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await
            .with_context(|| format!("打开 messages.jsonl 失败: {}", path.display()))?;

        for msg in msgs {
            let mut line = serde_json::to_string(msg)?;
            line.push('\n');
            file.write_all(line.as_bytes()).await?;
        }
        file.flush().await?;

        // 更新 meta 的 message_count 和 updated_at
        let mut meta = self.load_meta(id).await?;
        meta.message_count += msgs.len();
        meta.updated_at = Utc::now();
        // 如果还没有标题，用第一条 Human 消息的前 50 字符作为标题
        if meta.title.is_none() {
            if let Some(title) = extract_title(msgs) {
                meta.title = Some(title);
            }
        }
        self.update_meta(id, meta).await
    }

    async fn load_messages(&self, id: &ThreadId) -> Result<Vec<BaseMessage>> {
        let path = self.messages_path(id);
        if !path.exists() {
            return Ok(vec![]);
        }
        let raw = fs::read_to_string(&path).await?;
        let mut msgs = Vec::new();
        for line in raw.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let msg: BaseMessage =
                serde_json::from_str(line).with_context(|| format!("反序列化消息失败: {line}"))?;
            msgs.push(msg);
        }
        Ok(msgs)
    }

    async fn load_meta(&self, id: &ThreadId) -> Result<ThreadMeta> {
        let path = self.meta_path(id);
        let raw = fs::read_to_string(&path)
            .await
            .with_context(|| format!("读取 meta.json 失败: {}", path.display()))?;
        let meta: ThreadMeta = serde_json::from_str(&raw)?;
        Ok(meta)
    }

    async fn update_meta(&self, id: &ThreadId, meta: ThreadMeta) -> Result<()> {
        let json = serde_json::to_string_pretty(&meta)?;
        fs::write(self.meta_path(id), json).await?;
        self.upsert_index(&meta).await
    }

    async fn list_threads(&self) -> Result<Vec<ThreadMeta>> {
        let mut metas = self.read_index().await?;
        metas.sort_by_key(|b| std::cmp::Reverse(b.updated_at));
        // 计算 content_size（从 messages.jsonl 文件大小）
        for meta in &mut metas {
            let msg_path = self.messages_path(&meta.id);
            if msg_path.exists() {
                if let Ok(file_meta) = tokio::fs::metadata(&msg_path).await {
                    meta.content_size = file_meta.len();
                }
            }
        }
        Ok(metas)
    }

    async fn delete_thread(&self, id: &ThreadId) -> Result<()> {
        let dir = self.thread_dir(id);
        if dir.exists() {
            fs::remove_dir_all(&dir).await?;
        }
        let mut metas = self.read_index().await?;
        metas.retain(|m| m.id != *id);
        self.write_index(&metas).await
    }
}

/// 从消息列表中提取标题（取第一条 Human 消息的前 50 字符）
fn extract_title(msgs: &[BaseMessage]) -> Option<String> {
    use crate::messages::{ContentBlock, MessageContent};

    for msg in msgs {
        if let BaseMessage::Human { content, .. } = msg {
            let text = match content {
                MessageContent::Text(t) => t.clone(),
                MessageContent::Blocks(blocks) => blocks
                    .iter()
                    .filter_map(|b| {
                        if let ContentBlock::Text { text } = b {
                            Some(text.as_str())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" "),
                MessageContent::Raw(_) => continue,
            };
            let title: String = text.chars().take(50).collect();
            if !title.is_empty() {
                return Some(title);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn make_meta(cwd: &str) -> ThreadMeta {
        ThreadMeta::new(cwd)
    }

    #[tokio::test]
    async fn test_create_and_load_thread() {
        let dir = tempdir().unwrap();
        let store = FilesystemThreadStore::new(dir.path());
        let meta = make_meta("/test");

        let id = store.create_thread(meta.clone()).await.unwrap();
        assert_eq!(id, meta.id);

        let loaded = store.load_meta(&id).await.unwrap();
        assert_eq!(loaded.id, meta.id);
        assert_eq!(loaded.cwd, "/test");
    }

    #[tokio::test]
    async fn test_append_and_load_messages() {
        let dir = tempdir().unwrap();
        let store = FilesystemThreadStore::new(dir.path());
        let meta = make_meta("/test");
        let id = store.create_thread(meta).await.unwrap();

        let msgs = vec![BaseMessage::human("Hello"), BaseMessage::ai("World")];
        store.append_messages(&id, &msgs).await.unwrap();

        let loaded = store.load_messages(&id).await.unwrap();
        assert_eq!(loaded.len(), 2);
    }

    #[tokio::test]
    async fn test_append_empty_messages_noop() {
        let dir = tempdir().unwrap();
        let store = FilesystemThreadStore::new(dir.path());
        let meta = make_meta("/test");
        let id = store.create_thread(meta).await.unwrap();

        store.append_messages(&id, &[]).await.unwrap();
        let loaded = store.load_messages(&id).await.unwrap();
        assert!(loaded.is_empty());
    }

    #[tokio::test]
    async fn test_message_count_updates() {
        let dir = tempdir().unwrap();
        let store = FilesystemThreadStore::new(dir.path());
        let meta = make_meta("/test");
        let id = store.create_thread(meta).await.unwrap();

        let msgs = vec![BaseMessage::human("msg1")];
        store.append_messages(&id, &msgs).await.unwrap();

        let loaded = store.load_meta(&id).await.unwrap();
        assert_eq!(loaded.message_count, 1);
    }

    #[tokio::test]
    async fn test_title_extracted_from_first_human() {
        let dir = tempdir().unwrap();
        let store = FilesystemThreadStore::new(dir.path());
        let meta = make_meta("/test");
        let id = store.create_thread(meta).await.unwrap();

        let msgs = vec![BaseMessage::human("This is my question about Rust")];
        store.append_messages(&id, &msgs).await.unwrap();

        let loaded = store.load_meta(&id).await.unwrap();
        assert_eq!(
            loaded.title.as_deref(),
            Some("This is my question about Rust")
        );
    }

    #[tokio::test]
    async fn test_list_threads_sorted_by_updated_at() {
        let dir = tempdir().unwrap();
        let store = FilesystemThreadStore::new(dir.path());

        let meta1 = make_meta("/a");
        let id1 = meta1.id.clone();
        store.create_thread(meta1).await.unwrap();

        let meta2 = make_meta("/b");
        let id2 = meta2.id.clone();
        store.create_thread(meta2).await.unwrap();

        let list = store.list_threads().await.unwrap();
        assert_eq!(list.len(), 2);
        // Second created should be first (most recent updated_at)
        assert_eq!(list[0].id, id2);
        assert_eq!(list[1].id, id1);
    }

    #[tokio::test]
    async fn test_delete_thread() {
        let dir = tempdir().unwrap();
        let store = FilesystemThreadStore::new(dir.path());
        let meta = make_meta("/test");
        let id = store.create_thread(meta).await.unwrap();

        store.delete_thread(&id).await.unwrap();

        let list = store.list_threads().await.unwrap();
        assert!(list.is_empty());
    }

    #[tokio::test]
    async fn test_update_meta() {
        let dir = tempdir().unwrap();
        let store = FilesystemThreadStore::new(dir.path());
        let meta = make_meta("/test");
        let id = store.create_thread(meta).await.unwrap();

        let mut updated = store.load_meta(&id).await.unwrap();
        updated.title = Some("new title".into());
        store.update_meta(&id, updated.clone()).await.unwrap();

        let loaded = store.load_meta(&id).await.unwrap();
        assert_eq!(loaded.title.as_deref(), Some("new title"));
    }

    #[tokio::test]
    async fn test_content_size_in_list() {
        let dir = tempdir().unwrap();
        let store = FilesystemThreadStore::new(dir.path());
        let meta = make_meta("/test");
        let id = store.create_thread(meta).await.unwrap();

        let msgs = vec![BaseMessage::human("Hello world")];
        store.append_messages(&id, &msgs).await.unwrap();

        let list = store.list_threads().await.unwrap();
        assert_eq!(list.len(), 1);
        assert!(list[0].content_size > 0);
    }

    #[tokio::test]
    async fn test_load_messages_nonexistent_thread() {
        let dir = tempdir().unwrap();
        let store = FilesystemThreadStore::new(dir.path());
        let msgs = store
            .load_messages(&"nonexistent".to_string())
            .await
            .unwrap();
        assert!(msgs.is_empty());
    }

    #[test]
    fn test_extract_title_from_text() {
        let msgs = vec![BaseMessage::human("Hello world")];
        assert_eq!(extract_title(&msgs), Some("Hello world".to_string()));
    }

    #[test]
    fn test_extract_title_truncates_50_chars() {
        let long: String = "a".repeat(100);
        let msgs = vec![BaseMessage::human(long.as_str())];
        let title = extract_title(&msgs).unwrap();
        assert_eq!(title.chars().count(), 50);
    }

    #[test]
    fn test_extract_title_empty_messages() {
        let msgs: Vec<BaseMessage> = vec![];
        assert!(extract_title(&msgs).is_none());
    }
}

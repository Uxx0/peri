use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use std::path::PathBuf;

use crate::messages::BaseMessage;
use crate::thread::{ThreadId, ThreadMeta, ThreadStore};

/// 基于 SQLite 的 ThreadStore 实现
///
/// 使用 WAL 模式提升并发读性能，sqlx SqlitePool 连接池管理并发。
pub struct SqliteThreadStore {
    pool: SqlitePool,
}

impl SqliteThreadStore {
    /// 使用指定路径打开（或创建）数据库，并初始化 Schema
    pub async fn new(db_path: impl Into<PathBuf>) -> Result<Self> {
        let db_path = db_path.into();
        // 确保父目录存在
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("创建目录失败: {}", parent.display()))?;
        }
        let options = SqliteConnectOptions::new()
            .filename(&db_path)
            .create_if_missing(true)
            .pragma("journal_mode", "WAL")
            .pragma("synchronous", "NORMAL")
            .pragma("foreign_keys", "ON");
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await?;
        let store = Self { pool };
        store.init_schema().await?;
        Ok(store)
    }

    /// 使用默认路径 `~/.peri-core/threads/threads.db` 创建
    pub async fn default_path() -> Result<Self> {
        let db_path = dirs_next::home_dir()
            .context("无法获取 home 目录")?
            .join(".peri-core")
            .join("threads")
            .join("threads.db");
        Self::new(db_path).await
    }

    /// 初始化 Schema（幂等，可重复调用）
    async fn init_schema(&self) -> Result<()> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS threads (
                id          TEXT PRIMARY KEY,
                title       TEXT,
                cwd         TEXT NOT NULL DEFAULT '',
                created_at  TEXT NOT NULL,
                updated_at  TEXT NOT NULL,
                message_count INTEGER NOT NULL DEFAULT 0
            )",
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS messages (
                message_id  TEXT PRIMARY KEY,
                thread_id   TEXT NOT NULL,
                role        TEXT NOT NULL,
                content     TEXT NOT NULL,
                FOREIGN KEY (thread_id) REFERENCES threads(id) ON DELETE CASCADE
            )",
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_messages_thread_id ON messages (thread_id ASC)",
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }
}

// ── 辅助函数 ──────────────────────────────────────────────────────────────────

fn role_of(msg: &BaseMessage) -> &'static str {
    match msg {
        BaseMessage::Human { .. } => "user",
        BaseMessage::Ai { .. } => "assistant",
        BaseMessage::System { .. } => "system",
        BaseMessage::Tool { .. } => "tool",
    }
}

fn meta_from_row(
    id: String,
    title: Option<String>,
    cwd: String,
    created_at: String,
    updated_at: String,
    message_count: i64,
    content_size: i64,
) -> Result<ThreadMeta> {
    Ok(ThreadMeta {
        id,
        title,
        cwd,
        created_at: created_at.parse::<DateTime<Utc>>()?,
        updated_at: updated_at.parse::<DateTime<Utc>>()?,
        message_count: message_count as usize,
        content_size: content_size as u64,
    })
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

// ── ThreadStore impl ───────────────────────────────────────────────────────────

#[async_trait]
impl ThreadStore for SqliteThreadStore {
    async fn create_thread(&self, meta: ThreadMeta) -> Result<ThreadId> {
        let id = meta.id.clone();
        sqlx::query(
            "INSERT INTO threads (id, title, cwd, created_at, updated_at, message_count)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )
        .bind(&meta.id)
        .bind(&meta.title)
        .bind(&meta.cwd)
        .bind(meta.created_at.to_rfc3339())
        .bind(meta.updated_at.to_rfc3339())
        .bind(meta.message_count as i64)
        .execute(&self.pool)
        .await?;
        Ok(id)
    }

    async fn append_messages(&self, id: &ThreadId, msgs: &[BaseMessage]) -> Result<()> {
        if msgs.is_empty() {
            return Ok(());
        }
        let mut tx = self.pool.begin().await?;
        for msg in msgs {
            let message_id = msg.id().as_uuid().to_string();
            let role = role_of(msg);
            let content = serde_json::to_string(msg)?;
            sqlx::query(
                "INSERT OR IGNORE INTO messages (message_id, thread_id, role, content)
                 VALUES (?1, ?2, ?3, ?4)",
            )
            .bind(&message_id)
            .bind(id.as_str())
            .bind(role)
            .bind(&content)
            .execute(&mut *tx)
            .await?;
        }
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "UPDATE threads SET updated_at = ?1,
                message_count = (SELECT COUNT(*) FROM messages WHERE thread_id = ?2)
             WHERE id = ?2",
        )
        .bind(&now)
        .bind(id.as_str())
        .execute(&mut *tx)
        .await?;

        if let Some(title) = extract_title(msgs) {
            sqlx::query("UPDATE threads SET title = ?1 WHERE id = ?2 AND title IS NULL")
                .bind(&title)
                .bind(id.as_str())
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn load_messages(&self, id: &ThreadId) -> Result<Vec<BaseMessage>> {
        let rows: Vec<(String,)> =
            sqlx::query_as("SELECT content FROM messages WHERE thread_id = ?1 ORDER BY rowid")
                .bind(id.as_str())
                .fetch_all(&self.pool)
                .await?;

        rows.into_iter()
            .map(|(content,)| serde_json::from_str(&content).map_err(Into::into))
            .collect()
    }

    async fn load_meta(&self, id: &ThreadId) -> Result<ThreadMeta> {
        let row: (String, Option<String>, String, String, String, i64, i64) = sqlx::query_as(
            "SELECT t.id, t.title, t.cwd, t.created_at, t.updated_at, t.message_count,
                    (SELECT COALESCE(SUM(LENGTH(m.content)), 0) FROM messages m WHERE m.thread_id = t.id) as content_size
             FROM threads t WHERE t.id = ?1"
        )
        .bind(id.as_str())
        .fetch_one(&self.pool)
        .await?;

        meta_from_row(row.0, row.1, row.2, row.3, row.4, row.5, row.6)
    }

    async fn update_meta(&self, id: &ThreadId, meta: ThreadMeta) -> Result<()> {
        sqlx::query(
            "UPDATE threads SET title = ?1, cwd = ?2, updated_at = ?3, message_count = ?4 WHERE id = ?5"
        )
        .bind(&meta.title)
        .bind(&meta.cwd)
        .bind(meta.updated_at.to_rfc3339())
        .bind(meta.message_count as i64)
        .bind(id.as_str())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn list_threads(&self) -> Result<Vec<ThreadMeta>> {
        let rows: Vec<(String, Option<String>, String, String, String, i64, i64)> = sqlx::query_as(
            "SELECT t.id, t.title, t.cwd, t.created_at, t.updated_at, t.message_count,
                    (SELECT COALESCE(SUM(LENGTH(m.content)), 0) FROM messages m WHERE m.thread_id = t.id) as content_size
             FROM threads t ORDER BY t.updated_at DESC"
        )
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter()
            .map(|row| meta_from_row(row.0, row.1, row.2, row.3, row.4, row.5, row.6))
            .collect()
    }

    async fn delete_thread(&self, id: &ThreadId) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM threads WHERE id = ?1")
            .bind(id.as_str())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    async fn make_store() -> (SqliteThreadStore, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let store = SqliteThreadStore::new(dir.path().join("test.db"))
            .await
            .unwrap();
        (store, dir)
    }

    #[tokio::test]
    async fn test_create_append_load() {
        let (store, _dir) = make_store().await;
        let meta = ThreadMeta::new("/tmp");
        let id = store.create_thread(meta).await.unwrap();

        let msgs = vec![BaseMessage::human("Hello"), BaseMessage::ai("Hi there")];
        store.append_messages(&id, &msgs).await.unwrap();

        let loaded = store.load_messages(&id).await.unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].content(), "Hello");
        assert_eq!(loaded[1].content(), "Hi there");
    }

    #[tokio::test]
    async fn test_list_threads_order() {
        let (store, _dir) = make_store().await;

        let m1 = ThreadMeta::new("/a");
        let id1 = store.create_thread(m1).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;

        let m2 = ThreadMeta::new("/b");
        let id2 = store.create_thread(m2).await.unwrap();

        // 给 id2 追加消息，更新 updated_at
        store
            .append_messages(&id2, &[BaseMessage::human("msg")])
            .await
            .unwrap();

        let list = store.list_threads().await.unwrap();
        assert_eq!(list.len(), 2);
        // id2 updated_at 更新，应排在第一位
        assert_eq!(list[0].id, id2);
        assert_eq!(list[1].id, id1);
    }

    #[tokio::test]
    async fn test_delete_thread_cascade() {
        let (store, _dir) = make_store().await;
        let meta = ThreadMeta::new("/tmp");
        let id = store.create_thread(meta).await.unwrap();
        store
            .append_messages(&id, &[BaseMessage::human("msg")])
            .await
            .unwrap();

        store.delete_thread(&id).await.unwrap();

        // 消息应该被级联删除
        let msgs = store.load_messages(&id).await;
        // 线程不存在时 load_messages 应返回空（因为 SELECT 无结果）
        assert!(msgs.unwrap().is_empty());

        // 元数据应不存在
        let meta_result = store.load_meta(&id).await;
        assert!(meta_result.is_err());
    }

    #[tokio::test]
    async fn test_message_order_after_multiple_appends() {
        let (store, _dir) = make_store().await;
        let meta = ThreadMeta::new("/tmp");
        let id = store.create_thread(meta).await.unwrap();

        store
            .append_messages(&id, &[BaseMessage::human("msg1")])
            .await
            .unwrap();
        store
            .append_messages(&id, &[BaseMessage::ai("reply1")])
            .await
            .unwrap();
        store
            .append_messages(&id, &[BaseMessage::human("msg2")])
            .await
            .unwrap();

        let loaded = store.load_messages(&id).await.unwrap();
        assert_eq!(loaded.len(), 3);
        assert_eq!(loaded[0].content(), "msg1");
        assert_eq!(loaded[1].content(), "reply1");
        assert_eq!(loaded[2].content(), "msg2");
    }

    #[tokio::test]
    async fn test_title_auto_set() {
        let (store, _dir) = make_store().await;
        let meta = ThreadMeta::new("/tmp");
        let id = store.create_thread(meta).await.unwrap();

        store
            .append_messages(&id, &[BaseMessage::human("这是一条测试消息")])
            .await
            .unwrap();

        let loaded_meta = store.load_meta(&id).await.unwrap();
        assert!(loaded_meta.title.is_some());
        assert!(loaded_meta.title.unwrap().contains("这是一条测试消息"));
    }
}

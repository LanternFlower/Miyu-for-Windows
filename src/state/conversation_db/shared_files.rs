//! WebUI 文件分享清单的存取。
//!
//! 只存元数据，不存字节：reference 模式指向原文件，snapshot 模式指向
//! data/shared/ 托管副本。文件系统一致性（校验/删除副本）由 state 层包装
//! 负责，这里是纯库操作。

use crate::state::conversation_db::*;

#[derive(Debug, Clone, PartialEq)]
pub struct SharedFile {
    pub share_id: String,
    pub file_name: String,
    pub title: String,
    /// `reference` | `snapshot`
    pub mode: String,
    pub source_path: String,
    /// 下载时实际打开的路径：reference 模式等于 `source_path`，snapshot
    /// 模式是托管副本。
    pub stored_path: String,
    pub size_bytes: u64,
    /// 分享那一刻 stored_path 的 mtime（unix 秒）；reference 模式下载前校验。
    pub mtime_unix: i64,
    pub mime: String,
    /// `video` | `audio` | `image` | `other`，前端据此决定内联预览。
    pub kind: String,
    pub created_at: String,
}

fn map_shared_file_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SharedFile> {
    Ok(SharedFile {
        share_id: row.get(0)?,
        file_name: row.get(1)?,
        title: row.get(2)?,
        mode: row.get(3)?,
        source_path: row.get(4)?,
        stored_path: row.get(5)?,
        size_bytes: row.get::<_, i64>(6)?.max(0) as u64,
        mtime_unix: row.get(7)?,
        mime: row.get(8)?,
        kind: row.get(9)?,
        created_at: row.get(10)?,
    })
}

const SHARED_FILE_COLUMNS: &str = "share_id, file_name, title, mode, source_path, stored_path,
     size_bytes, mtime_unix, mime, kind, created_at";

impl ConversationDb {
    pub fn insert_shared_file(&self, record: &SharedFile) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO shared_files
                (share_id, file_name, title, mode, source_path, stored_path,
                 size_bytes, mtime_unix, mime, kind, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                record.share_id,
                record.file_name,
                record.title,
                record.mode,
                record.source_path,
                record.stored_path,
                record.size_bytes as i64,
                record.mtime_unix,
                record.mime,
                record.kind,
                record.created_at,
            ],
        )?;
        Ok(())
    }

    pub fn list_shared_files(&self) -> Result<Vec<SharedFile>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!(
            "SELECT {SHARED_FILE_COLUMNS} FROM shared_files ORDER BY created_at DESC, share_id"
        ))?;
        let records = stmt
            .query_map([], map_shared_file_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(records)
    }

    pub fn load_shared_file(&self, share_id: &str) -> Result<Option<SharedFile>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            &format!("SELECT {SHARED_FILE_COLUMNS} FROM shared_files WHERE share_id = ?1"),
            params![share_id],
            map_shared_file_row,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn delete_shared_file(&self, share_id: &str) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let deleted = conn.execute(
            "DELETE FROM shared_files WHERE share_id = ?1",
            params![share_id],
        )?;
        Ok(deleted > 0)
    }
}

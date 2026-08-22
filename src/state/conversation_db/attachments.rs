//! 附件与资产的存取。
//!
//! 用户附件有「暂存 → 被回合占用 → 释放」的生命周期
//! （`reserve_user_attachments` / `release_user_attachments_for_run`）：上传了但
//! 没发出去的必须能被回收，否则每次取消发送都在库里留一份垃圾
//! （`purge_stale_user_attachments` 兜底）。
//!
//! 图片与 artifact 是**回合产出**，走另一条路：它们随回合一起可见、一起删除。

use crate::state::conversation_db::*;

impl ConversationDb {
    pub fn insert_user_attachment(
        &self,
        session_id: &str,
        attachment: &UserAttachment,
        data: &[u8],
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO user_attachments
                (attachment_id, session_id, file_name, mime, kind, size_bytes,
                 width, height, data, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                attachment.attachment_id,
                session_id,
                attachment.file_name,
                attachment.mime,
                attachment.kind,
                attachment.size_bytes as i64,
                i64::from(attachment.width),
                i64::from(attachment.height),
                data,
                attachment.created_at,
            ],
        )?;
        Ok(())
    }

    pub fn load_user_attachment(
        &self,
        session_id: &str,
        attachment_id: &str,
    ) -> Result<Option<UserAttachmentData>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT attachment_id, file_name, mime, kind, size_bytes, width,
                    height, created_at, data
             FROM user_attachments WHERE session_id = ?1 AND attachment_id = ?2",
            params![session_id, attachment_id],
            |row| {
                Ok(UserAttachmentData {
                    attachment: map_user_attachment_row(row)?,
                    bytes: row.get(8)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn load_user_attachment_by_id(
        &self,
        attachment_id: &str,
    ) -> Result<Option<UserAttachmentData>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT attachment_id, file_name, mime, kind, size_bytes, width,
                    height, created_at, data
             FROM user_attachments WHERE attachment_id = ?1",
            params![attachment_id],
            |row| {
                Ok(UserAttachmentData {
                    attachment: map_user_attachment_row(row)?,
                    bytes: row.get(8)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn load_user_attachment_data_for_turn(
        &self,
        session_id: &str,
        turn_id: &str,
    ) -> Result<Vec<UserAttachmentData>> {
        self.load_bound_user_attachment_data(session_id, "turn_id", turn_id)
    }

    pub fn load_user_attachment_data_for_prompt(
        &self,
        session_id: &str,
        prompt_id: &str,
    ) -> Result<Vec<UserAttachmentData>> {
        self.load_bound_user_attachment_data(session_id, "prompt_id", prompt_id)
    }

    pub(crate) fn load_bound_user_attachment_data(
        &self,
        session_id: &str,
        field: &'static str,
        value: &str,
    ) -> Result<Vec<UserAttachmentData>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!(
            "SELECT attachment_id, file_name, mime, kind, size_bytes, width,
                    height, created_at, data
             FROM user_attachments
             WHERE session_id = ?1 AND {field} = ?2
             ORDER BY created_at, attachment_id"
        ))?;
        let attachments = stmt
            .query_map(params![session_id, value], |row| {
                Ok(UserAttachmentData {
                    attachment: map_user_attachment_row(row)?,
                    bytes: row.get(8)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(attachments)
    }

    pub fn load_user_attachments(
        &self,
        session_id: &str,
        attachment_ids: &[String],
    ) -> Result<Vec<UserAttachmentData>> {
        let conn = self.conn.lock().unwrap();
        let mut attachments = Vec::with_capacity(attachment_ids.len());
        for attachment_id in attachment_ids {
            let attachment = conn
                .query_row(
                    "SELECT attachment_id, file_name, mime, kind, size_bytes, width,
                            height, created_at, data
                     FROM user_attachments
                     WHERE session_id = ?1 AND attachment_id = ?2
                       AND turn_id IS NULL AND prompt_id IS NULL AND run_id IS NULL",
                    params![session_id, attachment_id],
                    |row| {
                        Ok(UserAttachmentData {
                            attachment: map_user_attachment_row(row)?,
                            bytes: row.get(8)?,
                        })
                    },
                )
                .optional()?;
            let Some(attachment) = attachment else {
                bail!("attachment is unavailable: {attachment_id}");
            };
            attachments.push(attachment);
        }
        Ok(attachments)
    }

    pub fn reserve_user_attachments(
        &self,
        session_id: &str,
        attachment_ids: &[String],
        run_id: &str,
    ) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for attachment_id in attachment_ids {
            let affected = tx.execute(
                "UPDATE user_attachments SET run_id = ?1
                 WHERE session_id = ?2 AND attachment_id = ?3
                   AND turn_id IS NULL AND prompt_id IS NULL AND run_id IS NULL",
                params![run_id, session_id, attachment_id],
            )?;
            if affected != 1 {
                bail!("attachment changed before it could be submitted: {attachment_id}");
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn release_user_attachments_for_run(&self, run_id: &str) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.execute(
            "UPDATE user_attachments SET run_id = NULL WHERE run_id = ?1",
            params![run_id],
        )?)
    }

    pub fn delete_staged_user_attachment(
        &self,
        session_id: &str,
        attachment_id: &str,
    ) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.execute(
            "DELETE FROM user_attachments
             WHERE session_id = ?1 AND attachment_id = ?2
               AND turn_id IS NULL AND prompt_id IS NULL AND run_id IS NULL",
            params![session_id, attachment_id],
        )? == 1)
    }

    pub fn purge_stale_user_attachments(&self) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.execute(
            "DELETE FROM user_attachments
             WHERE turn_id IS NULL AND prompt_id IS NULL AND run_id IS NULL
               AND datetime(created_at) < datetime('now', '-1 day')",
            [],
        )?)
    }

    pub fn insert_image_asset(&self, asset: &ImageAsset, data: &[u8]) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO image_assets
                (asset_id, turn_id, tool_id, mime, width, height, alt, data, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                asset.asset_id,
                asset.turn_id,
                asset.tool_id,
                asset.mime,
                i64::from(asset.width),
                i64::from(asset.height),
                asset.alt,
                data,
                asset.created_at,
            ],
        )?;
        Ok(())
    }

    pub fn load_image_assets(&self, session_id: &str) -> Result<Vec<ImageAsset>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT a.asset_id, a.turn_id, a.tool_id, a.mime, a.width, a.height, a.alt, a.created_at
             FROM image_assets a
             INNER JOIN turns t ON t.turn_id = a.turn_id
             WHERE t.session_id = ?1
             ORDER BY a.turn_id ASC, a.created_at ASC, a.asset_id ASC",
        )?;
        let assets = stmt
            .query_map(params![session_id], map_image_asset_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(assets)
    }

    pub fn load_image_asset(&self, asset_id: &str) -> Result<Option<ImageAssetData>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT asset_id, turn_id, tool_id, mime, width, height, alt, created_at, data
             FROM image_assets WHERE asset_id = ?1",
            params![asset_id],
            |row| {
                Ok(ImageAssetData {
                    asset: map_image_asset_row(row)?,
                    bytes: row.get(8)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn upsert_artifact_asset(&self, asset: &ArtifactAsset, data: &[u8]) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO artifact_assets
                (asset_id, turn_id, tool_id, source_key, file_name, mime, kind,
                 size_bytes, data, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
              ON CONFLICT(turn_id, source_key) DO UPDATE SET
                tool_id = excluded.tool_id,
                file_name = excluded.file_name,
                mime = excluded.mime,
                kind = excluded.kind,
                size_bytes = excluded.size_bytes,
                data = excluded.data,
                created_at = excluded.created_at,
                updated_at = excluded.updated_at
              ON CONFLICT(asset_id) DO UPDATE SET
                turn_id = excluded.turn_id,
                tool_id = excluded.tool_id,
                source_key = excluded.source_key,
                file_name = excluded.file_name,
                mime = excluded.mime,
                kind = excluded.kind,
                size_bytes = excluded.size_bytes,
                data = excluded.data,
                created_at = excluded.created_at,
                updated_at = excluded.updated_at",
            params![
                asset.asset_id,
                asset.turn_id,
                asset.tool_id,
                asset.source_key,
                asset.file_name,
                asset.mime,
                asset.kind,
                asset.size_bytes as i64,
                data,
                asset.created_at,
                asset.updated_at,
            ],
        )?;
        Ok(())
    }

    pub fn load_artifact_assets(&self, session_id: &str) -> Result<Vec<ArtifactAsset>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT a.asset_id, a.turn_id, a.tool_id, a.source_key, a.file_name,
                    a.mime, a.kind, a.size_bytes, a.created_at, a.updated_at
             FROM artifact_assets a
             INNER JOIN turns t ON t.turn_id = a.turn_id
             WHERE t.session_id = ?1
             ORDER BY a.turn_id, a.updated_at, a.asset_id",
        )?;
        let assets = stmt
            .query_map(params![session_id], map_artifact_asset_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(assets)
    }

    pub fn load_artifact_asset(&self, asset_id: &str) -> Result<Option<ArtifactAssetData>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT asset_id, turn_id, tool_id, source_key, file_name, mime, kind,
                    size_bytes, created_at, updated_at, data
             FROM artifact_assets WHERE asset_id = ?1",
            params![asset_id],
            |row| {
                Ok(ArtifactAssetData {
                    asset: map_artifact_asset_row(row)?,
                    bytes: row.get(10)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn load_artifact_asset_data_for_turn(
        &self,
        turn_id: &str,
    ) -> Result<Vec<ArtifactAssetData>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT asset_id, turn_id, tool_id, source_key, file_name, mime, kind,
                    size_bytes, created_at, updated_at, data
             FROM artifact_assets WHERE turn_id = ?1 ORDER BY updated_at, asset_id",
        )?;
        let assets = stmt
            .query_map(params![turn_id], |row| {
                Ok(ArtifactAssetData {
                    asset: map_artifact_asset_row(row)?,
                    bytes: row.get(10)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(assets)
    }
}

use rusqlite::Row;

use super::db::IngestAssetMetaInput;

pub struct IngestAssetRow {
    pub source_id: String,
    pub clip_id: String,
    pub source_path: String,
    pub original_path: String,
    pub proxy_path: String,
    pub project_proxy_path: String,
    pub card_thumb_path: String,
    pub file_extension: String,
    pub read_from_card: i64,
    pub card_locked: i64,
    pub poster_source: String,
    /// Last selected column: `import_status` or `thumb_status`.
    pub status: String,
}

impl IngestAssetRow {
    pub fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            source_id: row.get(0)?,
            clip_id: row.get(1)?,
            source_path: row.get(2)?,
            original_path: row.get(3)?,
            proxy_path: row.get(4)?,
            project_proxy_path: row.get(5)?,
            card_thumb_path: row.get(6)?,
            file_extension: row.get(7)?,
            read_from_card: row.get(8)?,
            card_locked: row.get(9)?,
            poster_source: row.get(10)?,
            status: row.get(11)?,
        })
    }

    pub fn meta_input(&self) -> IngestAssetMetaInput {
        self.meta_input_with_project_proxy(&self.project_proxy_path)
    }

    pub fn meta_input_without_project_proxy(&self) -> IngestAssetMetaInput {
        self.meta_input_with_project_proxy("")
    }

    fn meta_input_with_project_proxy(&self, project_proxy_path: &str) -> IngestAssetMetaInput {
        IngestAssetMetaInput {
            source_path: self.source_path.clone(),
            original_path: self.original_path.clone(),
            proxy_path: self.proxy_path.clone(),
            project_proxy_path: project_proxy_path.to_string(),
            card_thumb_path: self.card_thumb_path.clone(),
            file_extension: self.file_extension.clone(),
            read_from_card: self.read_from_card != 0,
            card_locked: self.card_locked != 0,
            poster_source: self.poster_source.clone(),
        }
    }
}

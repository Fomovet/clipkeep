use blake3::Hasher;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

pub const PREVIEW_CHARS: usize = 400;
const SCHEMA_VERSION: i64 = 4;
const LIST_LIMIT: i64 = 1000;

/// Favorites, pinned, and grouped items are kept forever and don't count toward the size budget;
/// only loose history is auto-trimmed.
const KEEP_FOREVER: &str = "(favorite = 1 OR pinned = 1 OR group_id IS NOT NULL)";
/// Pinned first, then newest first, in every view.
const ORDER: &str = "ORDER BY pinned DESC, pinned_at DESC, updated_at DESC";

pub const KIND_TEXT: &str = "text";
pub const KIND_IMAGE: &str = "image";

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ClipKind {
    Text,
    Image,
    Hex,
}

impl ClipKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Text => KIND_TEXT,
            Self::Image => KIND_IMAGE,
            Self::Hex => "hex",
        }
    }

    fn classify(kind: &str, content: &str) -> Self {
        if kind == KIND_IMAGE {
            return Self::Image;
        }
        if let Some(hex) = content.trim().strip_prefix('#')
            && matches!(hex.len(), 3 | 4 | 6 | 8)
            && hex.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Self::Hex;
        }
        Self::Text
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Clip {
    pub id: i64,
    pub kind: String,
    pub content_type: String,
    pub preview: String,
    /// Stored size in bytes (text UTF-8 length, or PNG file size).
    pub size: i64,
    /// Text longer than the preview.
    pub truncated: bool,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub favorite: bool,
    pub pinned: bool,
    pub updated_at: i64,
    pub source: Option<String>,
    pub group_id: Option<i64>,
    #[serde(skip)]
    pub file: Option<String>,
    pub thumb: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Group {
    pub id: i64,
    pub name: String,
    pub sort_order: i64,
    pub auto_types: Vec<ClipKind>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct StorageUsage {
    pub ordinary_bytes: i64,
    pub protected_bytes: i64,
}

pub struct NewClip<'a> {
    pub kind: &'a str,
    pub content: &'a str,
    pub hash: &'a str,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub size: i64,
    pub source: Option<&'a str>,
}

/// A full row for export/import; content is the text or the image file name.
pub struct RawClip {
    pub kind: String,
    pub content: String,
    pub hash: String,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub size: i64,
    pub favorite: bool,
    pub pinned: bool,
    pub group_id: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

pub enum Scope {
    All,
    Favorites,
    Group(i64),
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub fn text_hash(text: &str) -> String {
    let mut hasher = Hasher::new();
    hasher.update(b"text\0");
    hasher.update(text.as_bytes());
    hasher.finalize().to_hex().to_string()
}

fn preview_of(kind: &str, content: &str) -> Option<String> {
    (kind == KIND_TEXT).then(|| content.chars().take(PREVIEW_CHARS).collect())
}

pub fn open(path: &Path) -> rusqlite::Result<Connection> {
    let mut conn = Connection::open(path)?;
    conn.execute_batch("PRAGMA journal_mode = WAL;")?;
    // Fails fast on a damaged file so the caller can move it aside instead of crashing later.
    let check: String = conn.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
    if check != "ok" {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CORRUPT),
            Some(check),
        ));
    }
    migrate(&mut conn)?;
    Ok(conn)
}

fn migrate(conn: &mut Connection) -> rusqlite::Result<()> {
    let mut version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version > SCHEMA_VERSION {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_SCHEMA),
            Some(format!(
                "database schema {version} is newer than supported {SCHEMA_VERSION}"
            )),
        ));
    }
    while version < SCHEMA_VERSION {
        let tx = conn.transaction()?;
        match version {
            0 => migrate_v1(&tx)?,
            1 => migrate_v2(&tx)?,
            2 => migrate_v3(&tx)?,
            3 => migrate_v4(&tx)?,
            _ => unreachable!(),
        }
        version += 1;
        tx.pragma_update(None, "user_version", version)?;
        tx.commit()?;
    }
    Ok(())
}

fn migrate_v1(tx: &rusqlite::Transaction) -> rusqlite::Result<()> {
    let had_v0: bool = tx.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'clips'",
        [],
        |r| r.get::<_, i64>(0),
    )? > 0;
    if had_v0 {
        tx.execute_batch("ALTER TABLE clips RENAME TO clips_v0;")?;
    }
    tx.execute_batch(
        "CREATE TABLE clips (
             id INTEGER PRIMARY KEY AUTOINCREMENT, kind TEXT NOT NULL, content TEXT NOT NULL,
             hash TEXT NOT NULL UNIQUE, width INTEGER, height INTEGER,
             favorite INTEGER NOT NULL DEFAULT 0, fav_at INTEGER,
             pinned INTEGER NOT NULL DEFAULT 0, pinned_at INTEGER,
             source TEXT, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL, use_count INTEGER NOT NULL DEFAULT 1
         );",
    )?;
    if had_v0 {
        let old: Vec<(String, i64, Option<i64>, i64, i64, i64)> = {
            let mut stmt = tx.prepare(
                "SELECT content, favorite, fav_at, created_at, updated_at, use_count FROM clips_v0",
            )?;
            stmt.query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            })?
            .collect::<rusqlite::Result<_>>()?
        };
        for (content, favorite, fav_at, created_at, updated_at, use_count) in old {
            tx.execute(
                "INSERT OR IGNORE INTO clips (kind, content, hash, favorite, fav_at, created_at, updated_at, use_count)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![KIND_TEXT, content, text_hash(&content), favorite, fav_at, created_at, updated_at, use_count],
            )?;
        }
        tx.execute_batch("DROP TABLE clips_v0;")?;
    }
    tx.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_clips_order ON clips(pinned DESC, pinned_at DESC, updated_at DESC);",
    )
}

fn migrate_v2(tx: &rusqlite::Transaction) -> rusqlite::Result<()> {
    // Image sizes start as the filename length; the app backfills real file sizes after open.
    tx.execute_batch(
        "CREATE TABLE clipboard_groups (
             id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL,
             sort_order INTEGER NOT NULL DEFAULT 0, created_at INTEGER NOT NULL
         );
         ALTER TABLE clips ADD COLUMN size INTEGER NOT NULL DEFAULT 0;
         ALTER TABLE clips ADD COLUMN group_id INTEGER;
         UPDATE clips SET size = length(CAST(content AS BLOB));",
    )
}

fn migrate_v3(tx: &rusqlite::Transaction) -> rusqlite::Result<()> {
    // A stored preview lets the list skip reading (possibly multi-MB) full texts.
    tx.execute_batch(&format!(
        "ALTER TABLE clips ADD COLUMN preview TEXT;
         UPDATE clips SET preview = substr(content, 1, {PREVIEW_CHARS}) WHERE kind = 'text';
         CREATE INDEX IF NOT EXISTS idx_clips_loose ON clips(updated_at) WHERE NOT {KEEP_FOREVER};"
    ))
}

fn migrate_v4(tx: &rusqlite::Transaction) -> rusqlite::Result<()> {
    tx.execute_batch(
        "CREATE TABLE group_auto_types (
             group_id INTEGER NOT NULL,
             kind TEXT NOT NULL CHECK (kind IN ('text', 'image', 'hex')),
             PRIMARY KEY (group_id, kind)
         );
         CREATE INDEX idx_group_auto_types_kind ON group_auto_types(kind, group_id);
         ALTER TABLE clips ADD COLUMN content_type TEXT NOT NULL DEFAULT 'text';
         UPDATE clips SET content_type = 'image' WHERE kind = 'image';",
    )?;
    // Classify existing text once on upgrade, retaining physical text storage and paste behavior.
    let hex_ids = {
        let mut stmt = tx.prepare("SELECT id, content FROM clips WHERE kind = 'text'")?;
        let mut ids: Vec<i64> = Vec::new();
        for row in stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))? {
            let (id, content) = row?;
            if ClipKind::classify(KIND_TEXT, &content) == ClipKind::Hex {
                ids.push(id);
            }
        }
        ids
    };
    for id in hex_ids {
        tx.execute("UPDATE clips SET content_type = 'hex' WHERE id = ?1", [id])?;
    }
    Ok(())
}

pub fn referenced_files(conn: &Connection) -> rusqlite::Result<HashSet<String>> {
    let mut stmt = conn.prepare("SELECT content FROM clips WHERE kind = 'image'")?;
    stmt.query_map([], |r| r.get(0))?.collect()
}

pub fn is_file_referenced(conn: &Connection, file: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM clips WHERE kind = 'image' AND content = ?1)",
        params![file],
        |r| r.get(0),
    )
}

/// Inserts or bumps an item to the top, then trims loose history over the byte budget
/// (0 = unlimited). Returns image file names of any rows removed.
pub fn upsert(conn: &Connection, clip: &NewClip, max_bytes: i64) -> rusqlite::Result<Vec<String>> {
    conn.execute(
        "INSERT INTO clips (kind, content, preview, hash, width, height, size, source, created_at, updated_at, content_type, group_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9, ?10,
             (SELECT g.id FROM clipboard_groups g JOIN group_auto_types a ON a.group_id = g.id
              WHERE a.kind = ?10 ORDER BY g.sort_order, g.id LIMIT 1))
         ON CONFLICT(hash) DO UPDATE SET updated_at = excluded.updated_at, use_count = use_count + 1,
             group_id = COALESCE(clips.group_id, excluded.group_id)",
        params![
            clip.kind,
            clip.content,
            preview_of(clip.kind, clip.content),
            clip.hash,
            clip.width,
            clip.height,
            clip.size,
            clip.source,
            now_ms(),
            ClipKind::classify(clip.kind, clip.content).as_str()
        ],
    )?;
    enforce_storage(conn, max_bytes)
}

pub fn enforce_storage(conn: &Connection, max_bytes: i64) -> rusqlite::Result<Vec<String>> {
    if max_bytes <= 0 {
        return Ok(Vec::new());
    }
    let total: i64 = conn.query_row(
        &format!("SELECT COALESCE(SUM(size), 0) FROM clips WHERE NOT {KEEP_FOREVER}"),
        [],
        |r| r.get(0),
    )?;
    if total <= max_bytes {
        return Ok(Vec::new());
    }
    // Walk oldest-first reading only id/size (and the file name for images), and stop as soon as
    // enough is freed — never loading the bodies of the texts being dropped.
    let (mut ids, mut files, mut freed) = (Vec::new(), Vec::new(), 0i64);
    {
        let mut stmt = conn.prepare(&format!(
            "SELECT id, size, CASE WHEN kind = 'image' THEN content END FROM clips
             WHERE NOT {KEEP_FOREVER} ORDER BY updated_at ASC, id ASC"
        ))?;
        let mut rows = stmt.query([])?;
        while total - freed > max_bytes {
            let Some(row) = rows.next()? else { break };
            freed += row.get::<_, i64>(1)?;
            ids.push(row.get::<_, i64>(0)?);
            if let Some(file) = row.get::<_, Option<String>>(2)? {
                files.push(file);
            }
        }
    }
    let tx = conn.unchecked_transaction()?;
    for id in ids {
        tx.execute("DELETE FROM clips WHERE id = ?1", params![id])?;
    }
    tx.commit()?;
    Ok(files)
}

/// Uses the same eligibility rule as automatic cleanup, so the displayed quota matches trimming.
pub fn storage_usage(conn: &Connection) -> rusqlite::Result<StorageUsage> {
    conn.query_row(
        &format!(
            "SELECT COALESCE(SUM(CASE WHEN {KEEP_FOREVER} THEN 0 ELSE size END), 0),
                    COALESCE(SUM(CASE WHEN {KEEP_FOREVER} THEN size ELSE 0 END), 0)
             FROM clips"
        ),
        [],
        |row| {
            Ok(StorageUsage {
                ordinary_bytes: row.get(0)?,
                protected_bytes: row.get(1)?,
            })
        },
    )
}

fn delete_where(
    conn: &Connection,
    cond: &str,
    args: impl rusqlite::Params + Copy,
) -> rusqlite::Result<Vec<String>> {
    let files: Vec<String> = {
        let mut stmt = conn.prepare(&format!(
            "SELECT content FROM clips WHERE kind = 'image' AND ({cond})"
        ))?;
        stmt.query_map(args, |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    conn.execute(&format!("DELETE FROM clips WHERE {cond}"), args)?;
    Ok(files)
}

pub fn list(conn: &Connection, scope: Scope, query: &str) -> rusqlite::Result<Vec<Clip>> {
    let mut filters: Vec<String> = Vec::new();
    match scope {
        Scope::All => {}
        Scope::Favorites => filters.push("favorite = 1".into()),
        Scope::Group(id) => filters.push(format!("group_id = {id}")),
    }
    if !query.is_empty() {
        filters.push("(kind = 'text' AND content LIKE ?1 ESCAPE '\\')".into());
    }
    let where_clause = if filters.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", filters.join(" AND "))
    };
    let sql = format!(
        "SELECT id, kind, CASE WHEN kind = 'image' OR content_type = 'hex' THEN content ELSE COALESCE(preview, '') END,
                size, length(CAST(preview AS BLOB)) < size, width, height, favorite, pinned, updated_at, source, group_id, content_type
         FROM clips {where_clause} {ORDER} LIMIT {LIST_LIMIT}"
    );
    let pattern = format!(
        "%{}%",
        query
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let map = |r: &rusqlite::Row| {
        let kind: String = r.get(1)?;
        let body: String = r.get(2)?;
        let is_image = kind == KIND_IMAGE;
        let content_type: String = r.get(12)?;
        let is_hex = content_type == "hex";
        Ok(Clip {
            id: r.get(0)?,
            preview: if is_image {
                String::new()
            } else if is_hex {
                body.trim().to_string()
            } else {
                body.clone()
            },
            file: is_image.then_some(body),
            kind,
            content_type,
            size: r.get(3)?,
            truncated: !is_image && !is_hex && r.get::<_, Option<bool>>(4)?.unwrap_or(false),
            width: r.get(5)?,
            height: r.get(6)?,
            favorite: r.get::<_, i64>(7)? != 0,
            pinned: r.get::<_, i64>(8)? != 0,
            updated_at: r.get(9)?,
            source: r.get(10)?,
            group_id: r.get(11)?,
            thumb: None,
        })
    };
    let rows = if query.is_empty() {
        stmt.query_map([], map)?
    } else {
        stmt.query_map(params![pattern], map)?
    };
    rows.collect()
}

pub fn get(conn: &Connection, id: i64) -> rusqlite::Result<Option<(String, String)>> {
    conn.query_row(
        "SELECT kind, content FROM clips WHERE id = ?1",
        params![id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()
}

/// Where a clip came from; for a copied image file, its original file name.
pub fn source(conn: &Connection, id: i64) -> rusqlite::Result<Option<String>> {
    conn.query_row("SELECT source FROM clips WHERE id = ?1", params![id], |r| {
        r.get(0)
    })
    .optional()
    .map(Option::flatten)
}

/// Edits a text row in place, preserving its ID, favorite, pin and group.
pub fn edit_text(
    conn: &Connection,
    id: i64,
    text: &str,
    max_bytes: i64,
) -> Result<Vec<String>, String> {
    let row: Option<(String, bool, bool, bool)> = conn
        .query_row(
            "SELECT kind, favorite, pinned, group_id IS NOT NULL FROM clips WHERE id = ?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    match row.as_ref().map(|r| r.0.as_str()) {
        Some(KIND_TEXT) => {}
        Some(_) => return Err("只能编辑文本条目".into()),
        None => return Err("条目不存在".into()),
    }
    let (_, favorite, pinned, grouped) = row.unwrap();
    if max_bytes > 0 && text.len() as i64 > max_bytes && !favorite && !pinned && !grouped {
        return Err("文本超过普通历史容量上限，请先收藏或调高阈值".into());
    }
    let hash = text_hash(text);
    let duplicate: Option<i64> = conn
        .query_row(
            "SELECT id FROM clips WHERE hash = ?1 AND id != ?2",
            params![hash, id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    if duplicate.is_some() {
        return Err("相同文本已存在于历史记录".into());
    }
    conn.execute(
        "UPDATE clips SET content = ?2, preview = ?3, hash = ?4, size = ?5, updated_at = ?6, content_type = ?7 WHERE id = ?1",
        params![id, text, preview_of(KIND_TEXT, text), hash, text.len() as i64, now_ms(), ClipKind::classify(KIND_TEXT, text).as_str()],
    )
    .map_err(|e| e.to_string())?;
    enforce_storage(conn, max_bytes).map_err(|e| e.to_string())
}

pub fn touch(conn: &Connection, id: i64) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE clips SET updated_at = ?2, use_count = use_count + 1 WHERE id = ?1",
        params![id, now_ms()],
    )?;
    Ok(())
}

fn toggle(conn: &Connection, id: i64, flag: &str, stamp: &str) -> rusqlite::Result<bool> {
    conn.execute(
        &format!("UPDATE clips SET {flag} = 1 - {flag}, {stamp} = CASE WHEN {flag} = 0 THEN ?2 ELSE NULL END WHERE id = ?1"),
        params![id, now_ms()],
    )?;
    conn.query_row(
        &format!("SELECT {flag} FROM clips WHERE id = ?1"),
        params![id],
        |r| r.get::<_, i64>(0),
    )
    .map(|v| v != 0)
}

pub fn toggle_favorite(conn: &Connection, id: i64) -> rusqlite::Result<bool> {
    toggle(conn, id, "favorite", "fav_at")
}

pub fn toggle_pinned(conn: &Connection, id: i64) -> rusqlite::Result<bool> {
    toggle(conn, id, "pinned", "pinned_at")
}

pub fn set_group(conn: &Connection, id: i64, group_id: Option<i64>) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE clips SET group_id = ?2 WHERE id = ?1",
        params![id, group_id],
    )?;
    Ok(())
}

pub fn delete(conn: &Connection, id: i64) -> rusqlite::Result<Vec<String>> {
    delete_where(conn, "id = ?1", [id])
}

/// Clears loose history; the flags extend it to pinned/grouped, or wipe everything.
pub fn clear_data(
    conn: &Connection,
    include_pinned: bool,
    include_grouped: bool,
    include_all: bool,
) -> rusqlite::Result<Vec<String>> {
    if include_all {
        return delete_where(conn, "1 = 1", []);
    }
    // Favorites survive every option except "all".
    let mut parts = vec![format!("NOT {KEEP_FOREVER}")];
    if include_pinned {
        parts.push("(pinned = 1 AND favorite = 0)".into());
    }
    if include_grouped {
        parts.push("(group_id IS NOT NULL AND favorite = 0)".into());
    }
    let cond = format!("({})", parts.join(" OR "));
    delete_where(conn, &cond, [])
}

pub fn list_groups(conn: &Connection) -> rusqlite::Result<Vec<Group>> {
    let mut stmt =
        conn.prepare("SELECT id, name, sort_order FROM clipboard_groups ORDER BY sort_order, id")?;
    stmt.query_map([], |r| {
        Ok(Group {
            id: r.get(0)?,
            name: r.get(1)?,
            sort_order: r.get(2)?,
            auto_types: group_auto_types(conn, r.get(0)?)?,
        })
    })?
    .collect()
}

fn group_auto_types(conn: &Connection, id: i64) -> rusqlite::Result<Vec<ClipKind>> {
    let mut stmt =
        conn.prepare("SELECT kind FROM group_auto_types WHERE group_id = ?1 ORDER BY kind DESC")?;
    stmt.query_map([id], |r| match r.get::<_, String>(0)?.as_str() {
        KIND_TEXT => Ok(ClipKind::Text),
        KIND_IMAGE => Ok(ClipKind::Image),
        "hex" => Ok(ClipKind::Hex),
        _ => Err(rusqlite::Error::InvalidQuery),
    })?
    .collect()
}

/// The caller wraps group metadata, rules and optional backfill in one transaction.
pub fn create_group(
    conn: &Connection,
    name: &str,
    auto_types: &[ClipKind],
    include_existing: bool,
) -> rusqlite::Result<Group> {
    let next: i64 = conn.query_row(
        "SELECT COALESCE(MAX(sort_order), -1) + 1 FROM clipboard_groups",
        [],
        |r| r.get(0),
    )?;
    conn.execute(
        "INSERT INTO clipboard_groups (name, sort_order, created_at) VALUES (?1, ?2, ?3)",
        params![name, next, now_ms()],
    )?;
    let id = conn.last_insert_rowid();
    set_group_auto_types(conn, id, auto_types, include_existing)?;
    Ok(Group {
        id,
        name: name.to_string(),
        sort_order: next,
        auto_types: group_auto_types(conn, id)?,
    })
}

fn set_group_auto_types(
    conn: &Connection,
    id: i64,
    auto_types: &[ClipKind],
    include_existing: bool,
) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM group_auto_types WHERE group_id = ?1", [id])?;
    for kind in auto_types {
        conn.execute(
            "INSERT OR IGNORE INTO group_auto_types (group_id, kind) VALUES (?1, ?2)",
            params![id, kind.as_str()],
        )?;
    }
    if include_existing {
        // Backfill only loose records, using the same priority as live capture.
        conn.execute(
            "UPDATE clips SET group_id = (
                 SELECT g.id FROM clipboard_groups g JOIN group_auto_types a ON a.group_id = g.id
                 WHERE a.kind = clips.content_type ORDER BY g.sort_order, g.id LIMIT 1
             ) WHERE group_id IS NULL
               AND content_type IN (SELECT kind FROM group_auto_types WHERE group_id = ?1)",
            [id],
        )?;
    }
    Ok(())
}

pub fn update_group(
    conn: &Connection,
    id: i64,
    name: &str,
    auto_types: &[ClipKind],
    include_existing: bool,
) -> rusqlite::Result<()> {
    if conn.execute(
        "UPDATE clipboard_groups SET name = ?2 WHERE id = ?1",
        params![id, name],
    )? == 0
    {
        return Err(rusqlite::Error::QueryReturnedNoRows);
    }
    set_group_auto_types(conn, id, auto_types, include_existing)
}

pub fn rename_group(conn: &Connection, id: i64, name: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE clipboard_groups SET name = ?2 WHERE id = ?1",
        params![id, name],
    )?;
    Ok(())
}

/// Removes a group; its clips are deleted (when delete_items) or returned to loose history.
pub fn delete_group(
    conn: &Connection,
    id: i64,
    delete_items: bool,
) -> rusqlite::Result<Vec<String>> {
    let files = if delete_items {
        delete_where(conn, "group_id = ?1", [id])?
    } else {
        conn.execute(
            "UPDATE clips SET group_id = NULL WHERE group_id = ?1",
            params![id],
        )?;
        Vec::new()
    };
    conn.execute("DELETE FROM group_auto_types WHERE group_id = ?1", [id])?;
    conn.execute("DELETE FROM clipboard_groups WHERE id = ?1", params![id])?;
    Ok(files)
}

pub fn reorder_groups(conn: &Connection, ids: &[i64]) -> rusqlite::Result<()> {
    let tx = conn.unchecked_transaction()?;
    for (order, id) in ids.iter().enumerate() {
        tx.execute(
            "UPDATE clipboard_groups SET sort_order = ?2 WHERE id = ?1",
            params![id, order as i64],
        )?;
    }
    tx.commit()
}

/// All clip ids, oldest first; export fetches each row separately so the lock is never held long.
pub fn clip_ids(conn: &Connection) -> rusqlite::Result<Vec<i64>> {
    let mut stmt = conn.prepare("SELECT id FROM clips ORDER BY created_at, id")?;
    stmt.query_map([], |r| r.get(0))?.collect()
}

pub fn export_one(conn: &Connection, id: i64) -> rusqlite::Result<Option<RawClip>> {
    conn.query_row(
        "SELECT kind, content, hash, width, height, size, favorite, pinned, group_id, created_at, updated_at
         FROM clips WHERE id = ?1",
        params![id],
        |r| {
            Ok(RawClip {
                kind: r.get(0)?,
                content: r.get(1)?,
                hash: r.get(2)?,
                width: r.get(3)?,
                height: r.get(4)?,
                size: r.get(5)?,
                favorite: r.get::<_, i64>(6)? != 0,
                pinned: r.get::<_, i64>(7)? != 0,
                group_id: r.get(8)?,
                created_at: r.get(9)?,
                updated_at: r.get(10)?,
            })
        },
    )
    .optional()
}

/// Inserts an imported row, preserving timestamps and flags; merges onto an existing hash.
pub fn import_clip(conn: &Connection, c: &RawClip) -> rusqlite::Result<()> {
    let stamp = now_ms();
    let fav_at = c.favorite.then_some(stamp);
    let pinned_at = c.pinned.then_some(stamp);
    conn.execute(
        "INSERT INTO clips (kind, content, preview, hash, width, height, size, favorite, fav_at, pinned, pinned_at, group_id, created_at, updated_at, content_type)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
         ON CONFLICT(hash) DO UPDATE SET
             favorite = MAX(favorite, excluded.favorite),
             fav_at = COALESCE(fav_at, excluded.fav_at),
             pinned = MAX(pinned, excluded.pinned),
             pinned_at = COALESCE(pinned_at, excluded.pinned_at),
             group_id = COALESCE(group_id, excluded.group_id),
             updated_at = MAX(updated_at, excluded.updated_at)",
        params![
            c.kind, c.content, preview_of(&c.kind, &c.content), c.hash, c.width, c.height, c.size,
            c.favorite as i64, fav_at, c.pinned as i64, pinned_at, c.group_id, c.created_at, c.updated_at,
            ClipKind::classify(&c.kind, &c.content).as_str()
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn).unwrap();
        conn
    }

    #[test]
    fn refuses_newer_schema_without_changing_data() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE sentinel(value TEXT); INSERT INTO sentinel VALUES ('keep'); PRAGMA user_version = 99;").unwrap();
        assert!(migrate(&mut conn).is_err());
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        let value: String = conn
            .query_row("SELECT value FROM sentinel", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 99);
        assert_eq!(value, "keep");
    }

    fn add(conn: &Connection, text: &str, size: i64) -> i64 {
        let hash = text_hash(text);
        let clip = NewClip {
            kind: KIND_TEXT,
            content: text,
            hash: &hash,
            width: None,
            height: None,
            size,
            source: None,
        };
        upsert(conn, &clip, 0).unwrap();
        conn.query_row("SELECT id FROM clips WHERE hash = ?1", [&hash], |r| {
            r.get(0)
        })
        .unwrap()
    }

    fn texts(conn: &Connection) -> Vec<String> {
        let mut stmt = conn
            .prepare("SELECT content FROM clips ORDER BY id")
            .unwrap();
        stmt.query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    }

    fn add_image(conn: &Connection, file: &str, max_bytes: i64) -> i64 {
        upsert(
            conn,
            &NewClip {
                kind: KIND_IMAGE,
                content: file,
                hash: file,
                width: Some(100),
                height: Some(50),
                size: 500,
                source: None,
            },
            max_bytes,
        )
        .unwrap();
        conn.query_row("SELECT id FROM clips WHERE hash = ?1", [file], |r| r.get(0))
            .unwrap()
    }

    fn assigned_group(conn: &Connection, id: i64) -> Option<i64> {
        conn.query_row("SELECT group_id FROM clips WHERE id = ?1", [id], |r| {
            r.get(0)
        })
        .unwrap()
    }

    #[test]
    fn hex_classification_requires_one_complete_prefixed_css_color() {
        for color in [
            "#fff",
            "#AbCd",
            "#2563eb",
            "#AaBbCcDD",
            " \t#123456\r\n",
            "\u{3000}#123\u{3000}",
        ] {
            assert_eq!(
                ClipKind::classify(KIND_TEXT, color),
                ClipKind::Hex,
                "{color:?}"
            );
        }
        for text in [
            "",
            "#",
            "#12",
            "#12345",
            "#1234567",
            "#123456789",
            "#12x",
            "#ＦＦＦ",
            "FFFFFF",
            "0xFFFFFF",
            "color: #fff",
            "#fff #000",
            "#fff\n#000",
            "https://example.com/#fff",
            "#12 3",
        ] {
            assert_eq!(
                ClipKind::classify(KIND_TEXT, text),
                ClipKind::Text,
                "{text:?}"
            );
        }
        assert_eq!(ClipKind::classify(KIND_IMAGE, "#fff"), ClipKind::Image);
    }

    #[test]
    fn hex_is_a_separate_auto_type_and_keeps_original_text_for_copying() {
        let conn = mem();
        let text_group = create_group(&conn, "文字优先", &[ClipKind::Text], false).unwrap();
        let hex_group = create_group(&conn, "色板", &[ClipKind::Hex], false).unwrap();
        let original = " \t#AaBbCcDD\r\n";
        let id = add(&conn, original, original.len() as i64);
        assert_eq!(assigned_group(&conn, id), Some(hex_group.id));
        let color = list(&conn, Scope::Group(hex_group.id), "")
            .unwrap()
            .remove(0);
        assert_eq!(color.content_type, "hex");
        assert_eq!(color.kind, KIND_TEXT);
        assert_eq!(color.preview, "#AaBbCcDD");
        assert!(!color.truncated);
        assert_eq!(get(&conn, id).unwrap().unwrap().1, original);
        assert_eq!(add(&conn, original, original.len() as i64), id);
        let prose = add(&conn, "背景色是 #fff", 17);
        assert_eq!(assigned_group(&conn, prose), Some(text_group.id));
        assert_eq!(
            list(&conn, Scope::Group(text_group.id), "").unwrap()[0].content_type,
            "text"
        );
    }

    #[test]
    fn hex_backfill_and_edits_update_classification_without_moving_existing_groups() {
        let conn = mem();
        let hex = add(&conn, "#f0a8", 5);
        let text = add(&conn, "普通文本", 12);
        let image = add_image(&conn, "hex-backfill.png", 0);
        let group = create_group(&conn, "HEX", &[ClipKind::Hex], true).unwrap();
        assert_eq!(assigned_group(&conn, hex), Some(group.id));
        assert_eq!(assigned_group(&conn, text), None);
        assert_eq!(assigned_group(&conn, image), None);
        edit_text(&conn, hex, "现在是文本", 0).unwrap();
        let row = list(&conn, Scope::Group(group.id), "").unwrap().remove(0);
        assert_eq!(row.content_type, "text");
        assert_eq!(row.group_id, Some(group.id));
        edit_text(&conn, text, "#123ABC", 0).unwrap();
        let row = list(&conn, Scope::All, "123ABC").unwrap().remove(0);
        assert_eq!(row.content_type, "hex");
        assert_eq!(row.group_id, None);
    }

    #[test]
    fn imported_hex_text_is_classified_and_remains_editable_text() {
        let conn = mem();
        let content = "#12345678";
        import_clip(
            &conn,
            &RawClip {
                kind: KIND_TEXT.into(),
                content: content.into(),
                hash: text_hash(content),
                width: None,
                height: None,
                size: content.len() as i64,
                favorite: false,
                pinned: false,
                group_id: None,
                created_at: 1,
                updated_at: 1,
            },
        )
        .unwrap();
        let row = list(&conn, Scope::All, "").unwrap().remove(0);
        assert_eq!(row.content_type, "hex");
        assert_eq!(row.kind, KIND_TEXT);
        edit_text(&conn, row.id, "#abc", 0).unwrap();
        assert_eq!(get(&conn, row.id).unwrap().unwrap().1, "#abc");
    }

    #[test]
    fn v3_migration_preserves_groups_and_starts_without_rules() {
        let mut conn = mem();
        let group = create_group(&conn, "旧分组", &[], false).unwrap();
        let id = add(&conn, "旧记录", 9);
        let hex = add(&conn, "\u{3000}#AABBCC\u{3000}", 13);
        set_group(&conn, id, Some(group.id)).unwrap();
        conn.execute_batch("DROP TABLE group_auto_types; ALTER TABLE clips DROP COLUMN content_type; PRAGMA user_version = 3;")
            .unwrap();
        migrate(&mut conn).unwrap();
        let groups = list_groups(&conn).unwrap();
        assert_eq!(groups[0].name, "旧分组");
        assert!(groups[0].auto_types.is_empty());
        assert_eq!(assigned_group(&conn, id), Some(group.id));
        let color = list(&conn, Scope::All, "AABBCC").unwrap().remove(0);
        assert_eq!(color.id, hex);
        assert_eq!(color.content_type, "hex");
        assert_eq!(color.preview, "#AABBCC");
        assert_eq!(assigned_group(&conn, add(&conn, "新记录", 9)), None);
    }

    #[test]
    fn automatically_groups_both_types_and_protects_them_from_trimming() {
        let conn = mem();
        let manual = create_group(&conn, "手动", &[], false).unwrap();
        let text = create_group(&conn, "文字", &[ClipKind::Text], false).unwrap();
        let image = create_group(&conn, "图片", &[ClipKind::Image], false).unwrap();
        let text_id = add(&conn, "自动分类", 12);
        let image_id = add_image(&conn, "auto.png", 1);
        assert_eq!(assigned_group(&conn, text_id), Some(text.id));
        assert_eq!(assigned_group(&conn, image_id), Some(image.id));
        assert!(list(&conn, Scope::Group(manual.id), "").unwrap().is_empty());
        assert_eq!(list(&conn, Scope::Group(image.id), "").unwrap().len(), 1);
        assert!(enforce_storage(&conn, 1).unwrap().is_empty());
        assert_eq!(list(&conn, Scope::All, "").unwrap().len(), 2);
        let groups = list_groups(&conn).unwrap();
        assert_eq!(groups[1].auto_types, [ClipKind::Text]);
        assert_eq!(groups[2].auto_types, [ClipKind::Image]);
    }

    #[test]
    fn multi_type_rule_uses_order_and_reordering_only_affects_future_captures() {
        let conn = mem();
        let first = create_group(&conn, "第一", &[ClipKind::Text, ClipKind::Image], false).unwrap();
        let second = create_group(&conn, "第二", &[ClipKind::Text], false).unwrap();
        let before = add(&conn, "排序之前", 12);
        assert_eq!(assigned_group(&conn, before), Some(first.id));
        assert_eq!(
            assigned_group(&conn, add_image(&conn, "first.png", 0)),
            Some(first.id)
        );
        reorder_groups(&conn, &[second.id, first.id]).unwrap();
        let after = add(&conn, "排序之后", 12);
        assert_eq!(assigned_group(&conn, after), Some(second.id));
        add(&conn, "排序之前", 12);
        assert_eq!(assigned_group(&conn, before), Some(first.id));
        assert_eq!(
            assigned_group(&conn, add_image(&conn, "second.png", 0)),
            Some(first.id)
        );
    }

    #[test]
    fn recopy_classifies_loose_history_but_preserves_manual_group_and_flags() {
        let conn = mem();
        let loose = add(&conn, "历史文本", 12);
        let manual = create_group(&conn, "手动", &[], false).unwrap();
        let grouped = add(&conn, "手动文本", 12);
        set_group(&conn, grouped, Some(manual.id)).unwrap();
        toggle_favorite(&conn, grouped).unwrap();
        toggle_pinned(&conn, grouped).unwrap();
        let auto = create_group(&conn, "自动", &[ClipKind::Text], false).unwrap();
        assert_eq!(assigned_group(&conn, loose), None);
        assert_eq!(add(&conn, "历史文本", 12), loose);
        assert_eq!(assigned_group(&conn, loose), Some(auto.id));
        assert_eq!(add(&conn, "手动文本", 12), grouped);
        assert_eq!(assigned_group(&conn, grouped), Some(manual.id));
        let clip = list(&conn, Scope::Group(manual.id), "").unwrap().remove(0);
        assert!(clip.favorite && clip.pinned);
        assert_eq!(list(&conn, Scope::All, "").unwrap().len(), 2);
    }

    #[test]
    fn optional_backfill_honors_priority_selected_types_and_existing_groups() {
        let conn = mem();
        let loose = add(&conn, "已有文字", 12);
        let image = add_image(&conn, "existing.png", 0);
        let manual = create_group(&conn, "手动", &[], false).unwrap();
        let grouped = add(&conn, "已分组", 9);
        set_group(&conn, grouped, Some(manual.id)).unwrap();
        toggle_favorite(&conn, loose).unwrap();
        toggle_pinned(&conn, loose).unwrap();
        let first = create_group(&conn, "优先文字", &[ClipKind::Text], false).unwrap();
        let later = create_group(&conn, "次序文字", &[ClipKind::Text], true).unwrap();
        assert_eq!(assigned_group(&conn, loose), Some(first.id));
        assert_eq!(assigned_group(&conn, grouped), Some(manual.id));
        assert_eq!(assigned_group(&conn, image), None);
        assert!(list(&conn, Scope::Group(later.id), "").unwrap().is_empty());
        let clip = list(&conn, Scope::Group(first.id), "").unwrap().remove(0);
        assert!(clip.favorite && clip.pinned);
        update_group(&conn, later.id, "次序图片", &[ClipKind::Image], true).unwrap();
        assert_eq!(assigned_group(&conn, image), Some(later.id));
        assert_eq!(list_groups(&conn).unwrap()[2].auto_types, [ClipKind::Image]);
    }

    #[test]
    fn editing_or_disabling_rules_keeps_existing_items_and_can_backfill_later() {
        let conn = mem();
        let group = create_group(&conn, "文字", &[ClipKind::Text], false).unwrap();
        let old = add(&conn, "保留归属", 12);
        update_group(&conn, group.id, "手动整理", &[], true).unwrap();
        let loose = add(&conn, "不再自动", 12);
        assert_eq!(assigned_group(&conn, old), Some(group.id));
        assert_eq!(assigned_group(&conn, loose), None);
        update_group(&conn, group.id, "恢复自动", &[ClipKind::Text], false).unwrap();
        assert_eq!(assigned_group(&conn, loose), None);
        update_group(&conn, group.id, "归入历史", &[ClipKind::Text], true).unwrap();
        assert_eq!(assigned_group(&conn, loose), Some(group.id));
        assert!(update_group(&conn, -1, "不存在", &[ClipKind::Image], true).is_err());
    }

    #[test]
    fn deleting_auto_group_removes_rules_and_retains_history() {
        let conn = mem();
        let group = create_group(&conn, "自动", &[ClipKind::Text], false).unwrap();
        let id = add(&conn, "保留记录", 12);
        delete_group(&conn, group.id, false).unwrap();
        assert_eq!(assigned_group(&conn, id), None);
        assert!(group_auto_types(&conn, group.id).unwrap().is_empty());
        assert_eq!(assigned_group(&conn, add(&conn, "后续记录", 12)), None);
        assert!(get(&conn, id).unwrap().is_some());
    }

    #[test]
    fn rule_types_reject_unknown_values_and_deduplicate_selections() {
        assert!(serde_json::from_str::<ClipKind>("\"file\"").is_err());
        let conn = mem();
        let group = create_group(
            &conn,
            "去重",
            &[ClipKind::Image, ClipKind::Text, ClipKind::Text],
            false,
        )
        .unwrap();
        assert_eq!(group.auto_types, [ClipKind::Text, ClipKind::Image]);
        let json = serde_json::to_value(list_groups(&conn).unwrap()).unwrap();
        assert_eq!(json[0]["autoTypes"], serde_json::json!(["text", "image"]));
    }

    #[test]
    fn auto_rules_survive_reopening_the_database() {
        let path = std::env::temp_dir().join(format!(
            "clipkeep-auto-rules-{}-{}.db",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let group_id = {
            let conn = open(&path).unwrap();
            create_group(
                &conn,
                "持久规则",
                &[ClipKind::Text, ClipKind::Image, ClipKind::Hex],
                false,
            )
            .unwrap()
            .id
        };
        {
            let conn = open(&path).unwrap();
            let groups = list_groups(&conn).unwrap();
            assert_eq!(
                groups[0].auto_types,
                [ClipKind::Text, ClipKind::Image, ClipKind::Hex]
            );
            assert_eq!(
                assigned_group(&conn, add(&conn, "重启后文字", 15)),
                Some(group_id)
            );
            assert_eq!(
                assigned_group(&conn, add_image(&conn, "restart.png", 0)),
                Some(group_id)
            );
            assert_eq!(
                assigned_group(&conn, add(&conn, "#f008", 5)),
                Some(group_id)
            );
        }
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn group_rules_and_backfill_roll_back_together() {
        let mut conn = mem();
        let id = add(&conn, "事务记录", 12);
        {
            let tx = conn.transaction().unwrap();
            let group = create_group(&tx, "临时分组", &[ClipKind::Text], true).unwrap();
            assert_eq!(assigned_group(&tx, id), Some(group.id));
            // Dropping the uncommitted transaction rolls back metadata, rules and membership.
        }
        assert!(list_groups(&conn).unwrap().is_empty());
        assert_eq!(assigned_group(&conn, id), None);
        assert_eq!(assigned_group(&conn, add(&conn, "回滚之后", 12)), None);
    }

    #[test]
    fn migrates_v0_rows_to_latest() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE clips (id INTEGER PRIMARY KEY AUTOINCREMENT, content TEXT NOT NULL UNIQUE,
                 favorite INTEGER NOT NULL DEFAULT 0, fav_at INTEGER, created_at INTEGER NOT NULL,
                 updated_at INTEGER NOT NULL, use_count INTEGER NOT NULL DEFAULT 1);
             CREATE INDEX idx_clips_updated ON clips(updated_at DESC);
             INSERT INTO clips (content, favorite, created_at, updated_at) VALUES ('旧数据', 1, 1, 1);",
        )
        .unwrap();
        migrate(&mut conn).unwrap();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        let (hash, size, fav, preview): (String, i64, i64, String) = conn
            .query_row(
                "SELECT hash, size, favorite, preview FROM clips WHERE content = '旧数据'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(hash, text_hash("旧数据"));
        assert_eq!(size, "旧数据".len() as i64);
        assert_eq!(fav, 1);
        assert_eq!(preview, "旧数据");
    }

    #[test]
    fn list_uses_stored_preview_and_flags_truncation() {
        let conn = mem();
        let long: String = "字".repeat(PREVIEW_CHARS + 50);
        add(&conn, &long, long.len() as i64);
        add(&conn, "short", 5);
        let clips = list(&conn, Scope::All, "").unwrap();
        let long_clip = clips.iter().find(|c| c.size == long.len() as i64).unwrap();
        assert_eq!(long_clip.preview.chars().count(), PREVIEW_CHARS);
        assert!(long_clip.truncated);
        assert!(
            !clips
                .iter()
                .find(|c| c.preview == "short")
                .unwrap()
                .truncated
        );
        // Search still matches text beyond the preview.
        let tail_hit = list(&conn, Scope::All, &"字".repeat(PREVIEW_CHARS + 10)).unwrap();
        assert_eq!(tail_hit.len(), 1);
    }

    #[test]
    fn edit_text_preserves_identity_and_flags_and_rejects_duplicates() {
        let conn = mem();
        let id = add(&conn, "first", 5);
        let other = add(&conn, "already here", 12);
        toggle_favorite(&conn, id).unwrap();
        toggle_pinned(&conn, id).unwrap();
        let group = create_group(&conn, "常用", &[], false).unwrap();
        set_group(&conn, id, Some(group.id)).unwrap();
        let long = "新内容".repeat(PREVIEW_CHARS);
        edit_text(&conn, id, &long, 0).unwrap();
        let row = list(&conn, Scope::Group(group.id), "").unwrap().remove(0);
        assert_eq!(row.id, id);
        assert!(row.favorite && row.pinned && row.truncated);
        assert_eq!(row.preview.chars().count(), PREVIEW_CHARS);
        assert_eq!(get(&conn, id).unwrap().unwrap().1, long);
        assert!(
            edit_text(&conn, id, "already here", 0)
                .unwrap_err()
                .contains("相同文本")
        );
        assert_eq!(get(&conn, other).unwrap().unwrap().1, "already here");
        assert_eq!(get(&conn, id).unwrap().unwrap().1, long);
    }

    #[test]
    fn editing_text_reports_trimmed_image_files() {
        let conn = mem();
        upsert(
            &conn,
            &NewClip {
                kind: KIND_IMAGE,
                content: "old.png",
                hash: "image-old",
                width: Some(1),
                height: Some(1),
                size: 50,
                source: None,
            },
            0,
        )
        .unwrap();
        let id = add(&conn, "small", 5);
        let removed = edit_text(&conn, id, &"x".repeat(100), 120).unwrap();
        assert_eq!(removed, vec!["old.png"]);
        assert_eq!(get(&conn, id).unwrap().unwrap().1.len(), 100);
        assert!(!is_file_referenced(&conn, "old.png").unwrap());
        assert!(edit_text(&conn, id, &"y".repeat(121), 120).is_err());
    }

    #[test]
    fn storage_limit_trims_oldest_loose_items_only() {
        let conn = mem();
        let fav = add(&conn, "fav", 100);
        toggle_favorite(&conn, fav).unwrap();
        add(&conn, "a", 10);
        add(&conn, "b", 10);
        add(&conn, "c", 10);
        // Loose total 30 > 25: only the oldest loose one ("a") goes; the 100-byte favorite isn't counted.
        enforce_storage(&conn, 25).unwrap();
        assert_eq!(texts(&conn), vec!["fav", "b", "c"]);
        // 0 means unlimited.
        add(&conn, "d", 1_000_000);
        enforce_storage(&conn, 0).unwrap();
        assert_eq!(texts(&conn).len(), 4);
    }

    #[test]
    fn storage_usage_matches_the_cleanup_boundary() {
        let conn = mem();
        assert_eq!(
            storage_usage(&conn).unwrap(),
            StorageUsage {
                ordinary_bytes: 0,
                protected_bytes: 0,
            }
        );

        add(&conn, "loose", 10);
        let favorite = add(&conn, "favorite", 20);
        toggle_favorite(&conn, favorite).unwrap();
        let pinned = add(&conn, "pinned", 30);
        toggle_pinned(&conn, pinned).unwrap();
        let grouped = add(&conn, "grouped", 40);
        let group = create_group(&conn, "work", &[], false).unwrap();
        set_group(&conn, grouped, Some(group.id)).unwrap();
        upsert(
            &conn,
            &NewClip {
                kind: KIND_IMAGE,
                content: "loose.png",
                hash: "loose-image",
                width: Some(1),
                height: Some(1),
                size: 50,
                source: None,
            },
            0,
        )
        .unwrap();

        assert_eq!(
            storage_usage(&conn).unwrap(),
            StorageUsage {
                ordinary_bytes: 60,
                protected_bytes: 90,
            }
        );
        enforce_storage(&conn, 55).unwrap();
        assert_eq!(
            storage_usage(&conn).unwrap(),
            StorageUsage {
                ordinary_bytes: 50,
                protected_bytes: 90,
            }
        );
    }

    #[test]
    fn storage_trim_reports_image_files() {
        let conn = mem();
        let clip = NewClip {
            kind: KIND_IMAGE,
            content: "abc.png",
            hash: "abc",
            width: Some(1),
            height: Some(1),
            size: 50,
            source: None,
        };
        upsert(&conn, &clip, 0).unwrap();
        add(&conn, "newer", 10);
        assert_eq!(enforce_storage(&conn, 20).unwrap(), vec!["abc.png"]);
        assert!(!is_file_referenced(&conn, "abc.png").unwrap());
    }

    #[test]
    fn clear_data_respects_each_option() {
        let setup = || {
            let conn = mem();
            add(&conn, "loose", 1);
            let p = add(&conn, "pinned", 1);
            toggle_pinned(&conn, p).unwrap();
            let pf = add(&conn, "pinned+fav", 1);
            toggle_pinned(&conn, pf).unwrap();
            toggle_favorite(&conn, pf).unwrap();
            let g = create_group(&conn, "工作", &[], false).unwrap();
            let gi = add(&conn, "grouped", 1);
            set_group(&conn, gi, Some(g.id)).unwrap();
            let f = add(&conn, "fav", 1);
            toggle_favorite(&conn, f).unwrap();
            conn
        };

        let conn = setup();
        clear_data(&conn, false, false, false).unwrap();
        assert_eq!(texts(&conn), vec!["pinned", "pinned+fav", "grouped", "fav"]);

        let conn = setup();
        clear_data(&conn, true, false, false).unwrap();
        assert_eq!(texts(&conn), vec!["pinned+fav", "grouped", "fav"]);

        let conn = setup();
        clear_data(&conn, true, true, false).unwrap();
        assert_eq!(texts(&conn), vec!["pinned+fav", "fav"]);

        let conn = setup();
        clear_data(&conn, false, false, true).unwrap();
        assert!(texts(&conn).is_empty());
    }

    #[test]
    fn groups_reorder_and_delete_returns_items_to_history() {
        let conn = mem();
        let a = create_group(&conn, "A", &[], false).unwrap();
        let b = create_group(&conn, "B", &[], false).unwrap();
        reorder_groups(&conn, &[b.id, a.id]).unwrap();
        let names: Vec<String> = list_groups(&conn)
            .unwrap()
            .into_iter()
            .map(|g| g.name)
            .collect();
        assert_eq!(names, vec!["B", "A"]);

        let id = add(&conn, "in-a", 1);
        set_group(&conn, id, Some(a.id)).unwrap();
        assert_eq!(list(&conn, Scope::Group(a.id), "").unwrap().len(), 1);

        delete_group(&conn, a.id, false).unwrap();
        let gid: Option<i64> = conn
            .query_row("SELECT group_id FROM clips WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(gid, None);
        assert_eq!(list_groups(&conn).unwrap().len(), 1);
    }

    #[test]
    fn import_merges_flags_onto_existing_item() {
        let conn = mem();
        add(&conn, "same", 4);
        let raw = RawClip {
            kind: KIND_TEXT.into(),
            content: "same".into(),
            hash: text_hash("same"),
            width: None,
            height: None,
            size: 4,
            favorite: true,
            pinned: false,
            group_id: None,
            created_at: 1,
            updated_at: 1,
        };
        import_clip(&conn, &raw).unwrap();
        let (count, fav): (i64, i64) = conn
            .query_row("SELECT COUNT(*), MAX(favorite) FROM clips", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((count, fav), (1, 1));
    }
}

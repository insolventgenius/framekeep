use crate::library::{Adjustments, Decision, Format, Photo, PhotoEdits, Review};
use serde::Deserialize;
use std::{
    collections::{HashMap, HashSet},
    ffi::{CStr, CString, OsString, c_char, c_int},
    fs::File,
    io,
    os::unix::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
    ptr, slice,
};

enum Sqlite {}
enum SqliteStatement {}

const OK: c_int = 0;
const ROW: c_int = 100;
const DONE: c_int = 101;
const OPEN_READWRITE_CREATE: c_int = 0x02 | 0x04;

unsafe extern "C" {
    fn sqlite3_open_v2(
        path: *const c_char,
        db: *mut *mut Sqlite,
        flags: c_int,
        vfs: *const c_char,
    ) -> c_int;
    fn sqlite3_close(db: *mut Sqlite) -> c_int;
    fn sqlite3_errmsg(db: *mut Sqlite) -> *const c_char;
    fn sqlite3_busy_timeout(db: *mut Sqlite, milliseconds: c_int) -> c_int;
    fn sqlite3_prepare_v2(
        db: *mut Sqlite,
        sql: *const c_char,
        len: c_int,
        statement: *mut *mut SqliteStatement,
        tail: *mut *const c_char,
    ) -> c_int;
    fn sqlite3_finalize(statement: *mut SqliteStatement) -> c_int;
    fn sqlite3_step(statement: *mut SqliteStatement) -> c_int;
    fn sqlite3_bind_null(statement: *mut SqliteStatement, index: c_int) -> c_int;
    fn sqlite3_bind_int64(statement: *mut SqliteStatement, index: c_int, value: i64) -> c_int;
    fn framekeep_sqlite_bind_text(
        statement: *mut SqliteStatement,
        index: c_int,
        text: *const c_char,
        len: c_int,
    ) -> c_int;
    fn sqlite3_column_bytes(statement: *mut SqliteStatement, index: c_int) -> c_int;
    fn sqlite3_column_text(statement: *mut SqliteStatement, index: c_int) -> *const u8;
    fn sqlite3_column_int64(statement: *mut SqliteStatement, index: c_int) -> i64;
    fn sqlite3_last_insert_rowid(db: *mut Sqlite) -> i64;
}

pub struct Store(*mut Sqlite);

// Only the app's mutex owner uses the connection. Statements cannot outlive a borrow of it.
unsafe impl Send for Store {}

struct Statement<'db> {
    db: &'db Store,
    raw: *mut SqliteStatement,
}

impl Drop for Store {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { sqlite3_close(self.0) };
        }
    }
}

impl Drop for Statement<'_> {
    fn drop(&mut self) {
        unsafe { sqlite3_finalize(self.raw) };
    }
}

impl Statement<'_> {
    fn text(&self, index: c_int, value: Option<&[u8]>) -> Result<(), String> {
        let code = match value {
            Some(bytes) => {
                let length = c_int::try_from(bytes.len()).map_err(|error| error.to_string())?;
                unsafe {
                    framekeep_sqlite_bind_text(self.raw, index, bytes.as_ptr().cast(), length)
                }
            }
            None => unsafe { sqlite3_bind_null(self.raw, index) },
        };
        self.db.check(code)
    }

    fn integer(&self, index: c_int, value: i64) -> Result<(), String> {
        self.db
            .check(unsafe { sqlite3_bind_int64(self.raw, index, value) })
    }

    fn step(&mut self) -> Result<bool, String> {
        match unsafe { sqlite3_step(self.raw) } {
            ROW => Ok(true),
            DONE => Ok(false),
            _ => Err(self.db.error()),
        }
    }

    fn bytes(&self, index: c_int) -> Option<&[u8]> {
        let data = unsafe { sqlite3_column_text(self.raw, index) };
        if data.is_null() {
            None
        } else {
            let length = unsafe { sqlite3_column_bytes(self.raw, index) };
            Some(unsafe { slice::from_raw_parts(data, length as usize) })
        }
    }

    fn number(&self, index: c_int) -> i64 {
        unsafe { sqlite3_column_int64(self.raw, index) }
    }
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct LegacyPreferences {
    folder: Option<PathBuf>,
    photos: HashMap<PathBuf, LegacyDecision>,
}

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(default)]
struct LegacyDecision {
    format: Option<Format>,
    revision: i64,
    kept: bool,
    review: Option<Review>,
    favorite: bool,
    png_edits: Adjustments,
    raf_edits: Adjustments,
}

impl LegacyDecision {
    fn decision(self) -> Decision {
        let review = self.review.unwrap_or(if self.kept {
            Review::Keep
        } else {
            Review::Unreviewed
        });
        Decision {
            format: self.format,
            developed: self.format.filter(|&format| format != Format::Raf),
            review,
            favorite: review == Review::Keep && (self.favorite || self.kept),
            edits: PhotoEdits {
                png: self.png_edits,
                raf: self.raf_edits,
                ..PhotoEdits::default()
            },
        }
    }
}

const PHOTO_COLUMNS: &str =
    "id, category, favorite, preferred_format, developed_format, edits_json, revision";

struct Record {
    id: i64,
    decision: Decision,
    revision: i64,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self, String> {
        let path = CString::new(path.as_os_str().as_bytes()).map_err(|error| error.to_string())?;
        let mut db = ptr::null_mut();
        let code =
            unsafe { sqlite3_open_v2(path.as_ptr(), &mut db, OPEN_READWRITE_CREATE, ptr::null()) };
        let store = Self(db);
        store.check(code)?;
        store.check(unsafe { sqlite3_busy_timeout(db, 5000) })?;
        store.execute("PRAGMA foreign_keys = ON")?;
        store.transaction(|db| {
            db.execute("CREATE TABLE IF NOT EXISTS photos (
                id INTEGER PRIMARY KEY,
                category TEXT NOT NULL DEFAULT 'unreviewed' CHECK (category IN ('unreviewed', 'keep', 'delete')),
                favorite INTEGER NOT NULL DEFAULT 0 CHECK (favorite = 0 OR (favorite = 1 AND category = 'keep')),
                preferred_format TEXT CHECK (preferred_format IN ('png', 'raf', 'hif', 'jpg')),
                developed_format TEXT CHECK (developed_format IN ('png', 'hif', 'jpg')),
                edits_json TEXT CHECK (json_valid(edits_json) OR edits_json IS NULL),
                revision INTEGER NOT NULL DEFAULT 0
            )")?;
            db.execute("CREATE INDEX IF NOT EXISTS photos_revision ON photos(revision)")?;
            db.execute("CREATE TABLE IF NOT EXISTS photo_paths (
                path TEXT PRIMARY KEY NOT NULL,
                photo_id INTEGER NOT NULL REFERENCES photos(id)
            )")?;
            db.execute("CREATE INDEX IF NOT EXISTS photo_paths_photo_id ON photo_paths(photo_id)")?;
            db.execute("CREATE TABLE IF NOT EXISTS app_settings (key TEXT PRIMARY KEY NOT NULL, value TEXT NOT NULL)")?;
            Ok(())
        })?;
        Ok(store)
    }

    fn error(&self) -> String {
        if self.0.is_null() {
            return "Could not allocate a SQLite connection".into();
        }
        unsafe { CStr::from_ptr(sqlite3_errmsg(self.0)) }
            .to_string_lossy()
            .into_owned()
    }

    fn check(&self, code: c_int) -> Result<(), String> {
        if code == OK {
            Ok(())
        } else {
            Err(self.error())
        }
    }

    fn prepare(&self, sql: &str) -> Result<Statement<'_>, String> {
        let length = c_int::try_from(sql.len()).map_err(|error| error.to_string())?;
        let mut raw = ptr::null_mut();
        self.check(unsafe {
            sqlite3_prepare_v2(
                self.0,
                sql.as_ptr().cast(),
                length,
                &mut raw,
                ptr::null_mut(),
            )
        })?;
        Ok(Statement { db: self, raw })
    }

    fn execute(&self, sql: &str) -> Result<(), String> {
        self.prepare(sql)?.step().map(|_| ())
    }

    pub fn transaction<T>(
        &self,
        action: impl FnOnce(&Self) -> Result<T, String>,
    ) -> Result<T, String> {
        self.execute("BEGIN IMMEDIATE")?;
        let result = action(self).and_then(|value| self.execute("COMMIT").map(|()| value));
        if result.is_err() {
            let _ = self.execute("ROLLBACK");
        }
        result
    }

    fn setting(&self, key: &str) -> Result<Option<Vec<u8>>, String> {
        let mut query = self.prepare("SELECT value FROM app_settings WHERE key = ?1")?;
        query.text(1, Some(key.as_bytes()))?;
        Ok(if query.step()? {
            query.bytes(0).map(ToOwned::to_owned)
        } else {
            None
        })
    }

    fn set_setting(&self, key: &str, value: &[u8]) -> Result<(), String> {
        let mut statement = self.prepare(
            "INSERT INTO app_settings (key, value) VALUES (?1, ?2)
            ON CONFLICT(key) DO UPDATE SET value = excluded.value WHERE value <> excluded.value",
        )?;
        statement.text(1, Some(key.as_bytes()))?;
        statement.text(2, Some(value))?;
        statement.step()?;
        Ok(())
    }

    pub fn last_folder(&self) -> Result<Option<PathBuf>, String> {
        Ok(self
            .setting("last_folder")?
            .map(|folder| PathBuf::from(OsString::from_vec(folder))))
    }

    pub fn migrate(&self, path: &Path) -> Result<(), String> {
        if self.setting("legacy_import_complete")?.is_some() {
            return Ok(());
        }
        let legacy: LegacyPreferences = match File::open(path) {
            Ok(file) => serde_json::from_reader(file).map_err(|error| error.to_string())?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => LegacyPreferences::default(),
            Err(error) => return Err(error.to_string()),
        };
        self.transaction(|db| {
            if db.setting("legacy_import_complete")?.is_some() {
                return Ok(());
            }
            if db.last_folder()?.is_none()
                && let Some(folder) = legacy.folder
            {
                db.set_setting("last_folder", folder.as_os_str().as_bytes())?;
            }
            for (path, old) in legacy.photos {
                if db.by_path(&path)?.is_none() {
                    let id = db.insert(old.decision(), old.revision)?;
                    db.set_path(&path, id)?;
                }
            }
            db.set_setting("legacy_import_complete", b"1")
        })
    }

    fn record(query: &Statement<'_>) -> Result<Record, String> {
        let review = match query.bytes(1) {
            Some(b"unreviewed") => Review::Unreviewed,
            Some(b"keep") => Review::Keep,
            Some(b"delete") => Review::Delete,
            _ => return Err("Unknown review category in database".into()),
        };
        let format = |column| -> Result<Option<Format>, String> {
            query
                .bytes(column)
                .map(|value| {
                    Format::from_bytes(value)
                        .ok_or_else(|| "Unknown photo format in database".to_owned())
                })
                .transpose()
        };
        let edits = query
            .bytes(5)
            .map(serde_json::from_slice)
            .transpose()
            .map_err(|error| error.to_string())?
            .unwrap_or_default();
        Ok(Record {
            id: query.number(0),
            decision: Decision {
                format: format(3)?,
                developed: format(4)?,
                review,
                favorite: query.number(2) != 0,
                edits,
            },
            revision: query.number(6),
        })
    }

    fn by_path(&self, path: &Path) -> Result<Option<Record>, String> {
        let sql = format!(
            "SELECT {PHOTO_COLUMNS} FROM photos WHERE id = (SELECT photo_id FROM photo_paths WHERE path = ?1)"
        );
        let mut query = self.prepare(&sql)?;
        query.text(1, Some(path.as_os_str().as_bytes()))?;
        if query.step()? {
            Self::record(&query).map(Some)
        } else {
            Ok(None)
        }
    }

    pub fn decision(&self, id: i64) -> Result<Decision, String> {
        let sql = format!("SELECT {PHOTO_COLUMNS} FROM photos WHERE id = ?1");
        let mut query = self.prepare(&sql)?;
        query.integer(1, id)?;
        if query.step()? {
            Ok(Self::record(&query)?.decision)
        } else {
            Err("This photo has no database record.".into())
        }
    }

    fn bind_decision(statement: &Statement<'_>, decision: Decision) -> Result<(), String> {
        let review = match decision.review {
            Review::Unreviewed => b"unreviewed".as_slice(),
            Review::Keep => b"keep",
            Review::Delete => b"delete",
        };
        statement.text(1, Some(review))?;
        statement.integer(2, i64::from(decision.favorite))?;
        statement.text(3, decision.format.map(|format| format.as_str().as_bytes()))?;
        statement.text(
            4,
            decision.developed.map(|format| format.as_str().as_bytes()),
        )?;
        let json = (!decision.edits.is_default())
            .then(|| serde_json::to_vec(&decision.edits))
            .transpose()
            .map_err(|error| error.to_string())?;
        statement.text(5, json.as_deref())?;
        Ok(())
    }

    fn insert(&self, decision: Decision, revision: i64) -> Result<i64, String> {
        let mut statement = self.prepare("INSERT INTO photos
            (category, favorite, preferred_format, developed_format, edits_json, revision) VALUES (?1, ?2, ?3, ?4, ?5, ?6)")?;
        Self::bind_decision(&statement, decision)?;
        statement.integer(6, revision)?;
        statement.step()?;
        Ok(unsafe { sqlite3_last_insert_rowid(self.0) })
    }

    pub fn write_decision(&self, id: i64, decision: Decision) -> Result<(), String> {
        let mut statement = self.prepare("UPDATE photos SET category = ?1, favorite = ?2, preferred_format = ?3,
            developed_format = ?4, edits_json = ?5, revision = (SELECT MAX(revision) + 1 FROM photos) WHERE id = ?6")?;
        Self::bind_decision(&statement, decision)?;
        statement.integer(6, id)?;
        statement.step()?;
        Ok(())
    }

    pub fn set_path(&self, path: &Path, id: i64) -> Result<(), String> {
        let mut statement = self.prepare("INSERT INTO photo_paths (path, photo_id) VALUES (?1, ?2)
            ON CONFLICT(path) DO UPDATE SET photo_id = excluded.photo_id WHERE photo_id <> excluded.photo_id")?;
        statement.text(1, Some(path.as_os_str().as_bytes()))?;
        statement.integer(2, id)?;
        statement.step()?;
        Ok(())
    }

    pub fn register_new(&self, path: &Path) -> Result<i64, String> {
        let id = self.insert(Decision::default(), 0)?;
        self.set_path(path, id)?;
        Ok(id)
    }

    pub fn register_folder(&self, root: &Path, photos: &mut [Photo]) -> Result<(), String> {
        self.transaction(|db| {
            let mut assigned = HashSet::new();
            for photo in photos {
                let mut record: Option<Record> = None;
                for file in photo.files() {
                    if let Some(found) = db.by_path(&file.path)?
                        && record.as_ref().is_none_or(|other| found.revision > other.revision)
                    {
                        record = Some(found);
                    }
                }
                let mut record = match record {
                    Some(record) => record,
                    None => Record { id: db.insert(Decision::default(), 0)?, decision: Decision::default(), revision: 0 },
                };
                if assigned.contains(&record.id) {
                    record.id = db.insert(record.decision, record.revision)?;
                }
                assigned.insert(record.id);
                for file in photo.files() {
                    db.set_path(&file.path, record.id)?;
                }
                photo.database_id = record.id;
                photo.decision = record.decision;
            }
            db.execute("DELETE FROM photos WHERE NOT EXISTS (SELECT 1 FROM photo_paths WHERE photo_id = photos.id)")?;
            db.set_setting("last_folder", root.as_os_str().as_bytes())
        })
    }
}

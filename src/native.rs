use crate::{
    export::{self, StagedImage},
    library::{self, Adjustments, Decision, Format, Original, Photo, Review},
    lut,
    store::Store,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    ffi::{CStr, CString, OsStr, c_char},
    fs, io,
    os::unix::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
    ptr,
    sync::{Mutex, OnceLock},
};

unsafe extern "C" {
    fn framekeep_run(folder: *const c_char, ui: *const c_char);
    fn framekeep_trash(path: *const c_char, destination: *mut *mut c_char) -> *mut c_char;
    fn framekeep_restore(path: *const c_char, destination: *const c_char) -> *mut c_char;
    fn framekeep_raw_trash_progress(processed: usize, total: usize);
    fn free(pointer: *mut std::ffi::c_void);
}

struct TrashedFile {
    format: Format,
    original: Original,
    destination: PathBuf,
}

struct TrashAction {
    id: usize,
    files: Vec<TrashedFile>,
}

enum UndoAction {
    Review {
        id: usize,
        review: Review,
        favorite: bool,
    },
    Trash(Vec<TrashAction>),
}

struct App {
    catalog: Catalog,
    store: Result<Store, String>,
    migration_warning: Option<String>,
    undo: Vec<UndoAction>,
}

#[derive(Default)]
struct Catalog {
    root: PathBuf,
    photos: Vec<Photo>,
    generation: u32,
}

static APP: OnceLock<Mutex<App>> = OnceLock::new();

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
enum Command {
    Startup {
        folder: Option<PathBuf>,
    },
    Open {
        folder: PathBuf,
    },
    Refresh,
    LutOptions,
    Review {
        generation: u32,
        id: usize,
        review: Review,
    },
    Favorite {
        generation: u32,
        id: usize,
        favorite: bool,
    },
    Format {
        generation: u32,
        id: usize,
        format: Format,
    },
    Adjust {
        generation: u32,
        id: usize,
        format: Format,
        edits: Adjustments,
    },
    SaveNewPhoto {
        generation: u32,
        id: usize,
        format: Format,
    },
    RecycleAll {
        generation: u32,
    },
    TrashKeptRaws {
        generation: u32,
    },
    Undo,
    UndoTrash,
}

impl App {
    fn load(database: &Path, legacy: &Path) -> Self {
        let store = Store::open(database)
            .map_err(|error| format!("Could not open {}: {error}", database.display()));
        let migration_warning = store
            .as_ref()
            .ok()
            .and_then(|db| db.migrate(legacy).err())
            .map(|error| format!("Could not import the previous library: {error}"));
        Self {
            catalog: Catalog::default(),
            store,
            migration_warning,
            undo: Vec::new(),
        }
    }

    fn open(&mut self, path: &Path) -> Result<Value, String> {
        let root = path
            .canonicalize()
            .map_err(|error| format!("Could not open {}: {error}", path.display()))?;
        let (mut photos, skipped) = library::scan(&root)
            .map_err(|error| format!("Could not scan {}: {error}", root.display()))?;
        self.store
            .as_ref()
            .map_err(Clone::clone)?
            .register_folder(&root, &mut photos)
            .map_err(|error| format!("Could not register photos in SQLite: {error}"))?;
        self.catalog = Catalog {
            root,
            photos,
            generation: self.catalog.generation.wrapping_add(1),
        };
        self.undo.clear();

        let root = &self.catalog.root;
        let photos: Vec<_> = self
            .catalog
            .photos
            .iter()
            .enumerate()
            .map(|(id, photo)| photo.view(id, root))
            .collect();
        Ok(json!({
            "root": root.to_string_lossy(),
            "name": root.file_name().unwrap_or(root.as_os_str()).to_string_lossy(),
            "generation": self.catalog.generation, "photos": photos, "skipped": skipped,
            "warning": self.migration_warning,
        }))
    }

    fn photo(&self, generation: u32, id: usize) -> Result<&Photo, String> {
        if generation != self.catalog.generation {
            return Err("The folder has changed. Select the photo again.".into());
        }
        self.catalog
            .photos
            .get(id)
            .filter(|photo| photo.files().next().is_some())
            .ok_or_else(|| "This photo is no longer in the library.".into())
    }

    fn change(
        &mut self,
        generation: u32,
        id: usize,
        format: Option<Format>,
        update: impl FnOnce(&mut Decision),
    ) -> Result<Value, String> {
        let photo = self.photo(generation, id)?;
        if format.is_some_and(|format| photo.file(format).is_none()) {
            return Err("This version is unavailable.".into());
        }
        let previous = self.save_decision(id, update)?;
        let current = self.catalog.photos[id].decision;
        if (previous.review, previous.favorite) != (current.review, current.favorite) {
            self.undo.push(UndoAction::Review {
                id,
                review: previous.review,
                favorite: previous.favorite,
            });
        }
        let mut result = self.change_view(id);
        result["canUndo"] = json!(!self.undo.is_empty());
        Ok(result)
    }

    fn save_decision(
        &mut self,
        id: usize,
        update: impl FnOnce(&mut Decision),
    ) -> Result<Decision, String> {
        let database_id = self.catalog.photos[id].database_id;
        let store = self.store.as_ref().map_err(Clone::clone)?;
        let (previous, decision) = store
            .transaction(|db| {
                let previous = db.decision(database_id)?;
                let mut decision = previous;
                update(&mut decision);
                db.write_decision(database_id, decision)?;
                Ok((previous, decision))
            })
            .map_err(|error| format!("Could not save your changes: {error}"))?;
        self.catalog.photos[id].decision = decision;
        Ok(previous)
    }

    fn change_view(&self, id: usize) -> Value {
        let photo = &self.catalog.photos[id];
        let view = photo
            .files()
            .next()
            .map(|_| photo.view(id, &self.catalog.root));
        json!({ "id": id, "photo": view })
    }

    fn batch_response(&self, ids: Vec<usize>, count: usize, mut errors: Vec<String>) -> Value {
        let changes: Vec<_> = ids.into_iter().map(|id| self.change_view(id)).collect();
        let error_count = errors.len();
        errors.truncate(5);
        json!({
            "changes": changes, "count": count, "errors": errors,
            "errorCount": error_count, "canUndo": !self.undo.is_empty(),
            "canUndoTrash": self.undo.iter().any(|action| matches!(action, UndoAction::Trash(_)))
        })
    }

    fn create_png(
        &mut self,
        id: usize,
        select_png: bool,
        renderer: impl FnOnce(&Path, Format, Adjustments, Format, &Path) -> Result<(), String>,
    ) -> Result<(), String> {
        let photo = &self.catalog.photos[id];
        if photo.developed_files().next().is_some() {
            return Err("This photo already has a developed version.".into());
        }
        let source = photo.raf.as_ref().ok_or("This photo has no RAF version.")?;
        let store = self.store.as_ref().map_err(Clone::clone)?;
        let destination = source.path.with_extension("png");
        let database_id = photo.database_id;
        let staged = StagedImage::render(
            source,
            Format::Raf,
            Adjustments::default(),
            Format::Png,
            renderer,
        )?;
        let mut output = None;
        let result = store.transaction(|db| {
            let mut decision = db.decision(database_id)?;
            if select_png {
                decision.select_format(Format::Png);
                db.write_decision(database_id, decision)?;
            }
            output = Some(staged.publish(&destination).map_err(|error| {
                if error.kind() == io::ErrorKind::AlreadyExists {
                    "A PNG with this name already exists. Rescan the folder to find it.".to_owned()
                } else {
                    format!("Could not save {}: {error}", destination.display())
                }
            })?);
            db.set_path(&destination, database_id)?;
            Ok(decision)
        });
        match result {
            Ok(decision) => {
                self.catalog.photos[id].png = output;
                self.catalog.photos[id].decision = decision;
                Ok(())
            }
            Err(error) => {
                if let Some(file) = output {
                    export::rollback(&file);
                }
                Err(error)
            }
        }
    }

    fn save_new_photo(
        &mut self,
        generation: u32,
        id: usize,
        format: Format,
        date: &str,
        renderer: impl FnOnce(&Path, Format, Adjustments, Format, &Path) -> Result<(), String>,
    ) -> Result<Value, String> {
        let photo = self.photo(generation, id)?;
        let database_id = photo.database_id;
        let source = photo
            .file(format)
            .as_ref()
            .ok_or("This version is unavailable.")?;
        let store = self.store.as_ref().map_err(Clone::clone)?;
        let edits = store.decision(database_id)?.edits(format);
        if edits.is_default() {
            return Err("Apply an adjustment before saving an edited photo.".into());
        }
        let staged = StagedImage::render(source, format, edits, Format::Jpg, renderer)?;
        let mut output = None;
        let result = store.transaction(|db| {
            let mut decision = db.decision(database_id)?;
            if decision.edits(format) != edits {
                return Err("These adjustments changed. Reopen the editor before saving.".into());
            }
            decision.set_edits(format, Adjustments::default());
            db.write_decision(database_id, decision)?;
            let file = staged
                .publish_edited(&source.path, date)
                .map_err(|error| format!("Could not save the edited JPEG: {error}"))?;
            let path = file.path.clone();
            output = Some(file);
            let copy_id = db.register_new(&path)?;
            Ok((decision, copy_id))
        });
        let (decision, copy_id) = match result {
            Ok(value) => value,
            Err(error) => {
                if let Some(file) = output {
                    export::rollback(&file);
                }
                return Err(error);
            }
        };
        let output = output.expect("a committed export has a file");
        let filename = output
            .path
            .file_name()
            .unwrap_or(output.path.as_os_str())
            .to_string_lossy()
            .into_owned();
        self.catalog.photos[id].decision = decision;
        let copy_index = self.catalog.photos.len();
        self.catalog.photos.push(Photo {
            jpg: Some(output),
            database_id: copy_id,
            ..Photo::default()
        });
        Ok(json!({
            "source": self.change_view(id),
            "copy": self.change_view(copy_index),
            "filename": filename
        }))
    }

    fn recycle_all(
        &mut self,
        generation: u32,
        kept_raws: bool,
        mut move_to_trash: impl FnMut(&Path) -> Result<Option<PathBuf>, String>,
        mut renderer: impl FnMut(&Path, Format, Adjustments, Format, &Path) -> Result<(), String>,
    ) -> Result<Value, String> {
        if generation != self.catalog.generation {
            return Err("The selected folder has changed.".into());
        }
        let store = self.store.as_ref().map_err(Clone::clone)?;
        let decisions = store.transaction(|db| {
            self.catalog
                .photos
                .iter()
                .map(|photo| db.decision(photo.database_id))
                .collect::<Result<Vec<_>, _>>()
        })?;
        let target = if kept_raws {
            Review::Keep
        } else {
            Review::Delete
        };
        let formats: &[Format] = if kept_raws {
            &[Format::Raf]
        } else {
            &[Format::Png, Format::Raf, Format::Hif, Format::Jpg]
        };
        let raw_total = if kept_raws {
            decisions
                .iter()
                .zip(&self.catalog.photos)
                .filter(|(decision, photo)| decision.review == Review::Keep && photo.raf.is_some())
                .count()
        } else {
            0
        };
        let mut raw_processed = 0;
        let mut batch = Vec::new();
        let mut changed = Vec::new();
        let mut count = 0;
        let mut errors = Vec::new();
        for (id, decision) in decisions.into_iter().enumerate() {
            self.catalog.photos[id].decision = decision;
            if decision.review != target || (kept_raws && self.catalog.photos[id].raf.is_none()) {
                continue;
            }
            let mut created = false;
            if kept_raws {
                unsafe { framekeep_raw_trash_progress(raw_processed, raw_total) };
                raw_processed += 1;
                let photo = &self.catalog.photos[id];
                let raw_path = &photo.raf.as_ref().unwrap().path;
                if photo.developed_files().next().is_some() {
                    if !photo.developed_files().any(|file| file.verify().is_ok()) {
                        errors.push(format!(
                            "{}: could not verify a developed version. Rescan the folder and try again.",
                            raw_path.display()
                        ));
                        continue;
                    }
                } else {
                    let raw_path = raw_path.clone();
                    match self.create_png(id, false, &mut renderer) {
                        Ok(()) => created = true,
                        Err(error) => {
                            errors.push(format!("{}: {error}", raw_path.display()));
                            continue;
                        }
                    }
                }
            }
            let photo = &mut self.catalog.photos[id];
            let mut files = Vec::new();
            let previous_count = count;
            for &format in formats {
                let slot = photo.file_mut(format);
                let Some(original) = slot.as_ref() else {
                    continue;
                };
                match original
                    .verify()
                    .map_err(|error| error.to_string())
                    .and_then(|()| move_to_trash(&original.path))
                {
                    Ok(destination) => {
                        let original = slot.take().unwrap();
                        count += 1;
                        if let Some(destination) = destination {
                            files.push(TrashedFile {
                                format,
                                original,
                                destination,
                            });
                        } else {
                            errors.push(format!(
                                "{} moved to Trash. Restore it through Finder.",
                                original.path.display()
                            ));
                        }
                    }
                    Err(error) => errors.push(format!("{}: {error}", original.path.display())),
                }
            }
            if count > previous_count || created {
                changed.push(id);
            }
            if !files.is_empty() {
                batch.push(TrashAction { id, files });
            }
        }
        if kept_raws {
            unsafe { framekeep_raw_trash_progress(raw_processed, raw_total) };
        }
        if !batch.is_empty() {
            self.undo.push(UndoAction::Trash(batch));
        }
        Ok(self.batch_response(changed, count, errors))
    }

    fn undo(
        &mut self,
        trash_only: bool,
        mut restore: impl FnMut(&Path, &Path) -> Result<(), String>,
    ) -> Result<Value, String> {
        let index = if trash_only {
            self.undo
                .iter()
                .rposition(|action| matches!(action, UndoAction::Trash(_)))
        } else {
            self.undo.len().checked_sub(1)
        }
        .ok_or("There is no recent action to undo.")?;
        let mut batch = match self.undo.remove(index) {
            UndoAction::Trash(batch) => batch,
            UndoAction::Review {
                id,
                review,
                favorite,
            } => {
                if let Err(error) = self.save_decision(id, |decision| {
                    decision.review = review;
                    decision.favorite = favorite;
                }) {
                    self.undo.insert(
                        index,
                        UndoAction::Review {
                            id,
                            review,
                            favorite,
                        },
                    );
                    return Err(error);
                }
                return Ok(
                    json!({ "review": self.change_view(id), "canUndo": !self.undo.is_empty() }),
                );
            }
        };
        let mut changed = Vec::new();
        let mut count = 0;
        let mut errors = Vec::new();
        batch.retain_mut(|action| {
            let photo = &mut self.catalog.photos[action.id];
            let previous_count = count;
            action.files.retain(|file| {
                let slot = photo.file_mut(file.format);
                let result = if slot.is_some() {
                    Err("that version is already in the library".into())
                } else {
                    file.original.verify_at(&file.destination).map_err(|error| error.to_string())
                        .and_then(|()| restore(&file.destination, &file.original.path))
                };
                match result {
                    Ok(()) => {
                        *slot = Some(file.original.clone());
                        count += 1;
                        false
                    }
                    Err(error) => {
                        errors.push(format!("{}: {error}", file.original.path.display()));
                        !matches!(fs::symlink_metadata(&file.destination), Err(error) if error.kind() == io::ErrorKind::NotFound)
                    }
                }
            });
            if count > previous_count {
                changed.push(action.id);
            }
            !action.files.is_empty()
        });
        if !batch.is_empty() {
            self.undo.insert(index, UndoAction::Trash(batch));
        }
        Ok(self.batch_response(changed, count, errors))
    }

    fn execute(&mut self, command: Command) -> Result<Value, String> {
        match command {
            Command::Startup { folder } => {
                let store = self.store.as_ref().map_err(Clone::clone)?;
                match folder.or(store.last_folder()?) {
                    Some(folder) => self.open(&folder),
                    None => Ok(json!({ "warning": self.migration_warning })),
                }
            }
            Command::Open { folder } => self.open(&folder),
            Command::LutOptions => Ok(lut::options()),
            Command::Refresh => self.open(&self.catalog.root.clone()),
            Command::Review {
                generation,
                id,
                review,
            } => self.change(generation, id, None, |decision| decision.set_review(review)),
            Command::Favorite {
                generation,
                id,
                favorite,
            } => self.change(generation, id, None, |decision| {
                decision.set_favorite(favorite)
            }),
            Command::Format {
                generation,
                id,
                format,
            } => {
                let photo = self.photo(generation, id)?;
                if format == Format::Png && photo.png.is_none() {
                    self.create_png(id, true, export::render)?;
                    Ok(self.change_view(id))
                } else {
                    self.change(generation, id, Some(format), |decision| {
                        decision.select_format(format)
                    })
                }
            }
            Command::Adjust {
                generation,
                id,
                format,
                edits,
            } => self.change(generation, id, Some(format), |decision| {
                decision.set_edits(format, edits)
            }),
            Command::SaveNewPhoto {
                generation,
                id,
                format,
            } => self.save_new_photo(
                generation,
                id,
                format,
                &export::local_date()?,
                export::render,
            ),
            Command::RecycleAll { generation } => {
                self.recycle_all(generation, false, trash_file, export::render)
            }
            Command::TrashKeptRaws { generation } => {
                self.recycle_all(generation, true, trash_file, export::render)
            }
            Command::Undo => self.undo(false, restore_file),
            Command::UndoTrash => self.undo(true, restore_file),
        }
    }
}

fn path_string(path: &Path) -> Result<CString, String> {
    CString::new(path.as_os_str().as_bytes()).map_err(|error| error.to_string())
}

fn native_error(error: *mut c_char) -> Result<(), String> {
    if error.is_null() {
        return Ok(());
    }
    // The native bridge returns a strdup allocation on error.
    let message = unsafe { CStr::from_ptr(error) }
        .to_string_lossy()
        .into_owned();
    unsafe { free(error.cast()) };
    Err(message)
}

fn trash_file(path: &Path) -> Result<Option<PathBuf>, String> {
    let path = path_string(path)?;
    let mut destination = ptr::null_mut();
    native_error(unsafe { framekeep_trash(path.as_ptr(), &mut destination) })?;
    if destination.is_null() {
        return Ok(None);
    }
    let bytes = unsafe { CStr::from_ptr(destination) }.to_bytes().to_vec();
    unsafe { free(destination.cast()) };
    Ok(Some(PathBuf::from(std::ffi::OsString::from_vec(bytes))))
}

fn restore_file(path: &Path, destination: &Path) -> Result<(), String> {
    let path = path_string(path)?;
    let destination = path_string(destination)?;
    native_error(unsafe { framekeep_restore(path.as_ptr(), destination.as_ptr()) })
}

pub fn run(folder: Option<&OsStr>) {
    let lut_directory = std::env::var_os("FRAMEKEEP_LUT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace_path().join("vendor/x-t50-3d-lut-v100"));
    lut::set_directory(lut_directory);
    let folder =
        folder.map(|folder| CString::new(folder.as_bytes()).expect("folder has no NUL bytes"));
    let ui = path_string(&Path::new(env!("FRAMEKEEP_SOURCE")).join("ui")).expect("valid UI path");
    unsafe {
        framekeep_run(
            folder.as_ref().map_or(ptr::null(), |s| s.as_ptr()),
            ui.as_ptr(),
        )
    };
}

fn database_path() -> PathBuf {
    std::env::var_os("FRAMEKEEP_DATABASE")
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace_path().join("framekeep.sqlite3"))
}

fn workspace_path() -> PathBuf {
    let executable = std::env::current_exe().ok();
    let bundle_parent = executable.as_deref().and_then(|exe| {
        exe.ancestors()
            .find(|path| path.extension().is_some_and(|extension| extension == "app"))?
            .parent()
    });
    if let Some(directory) = bundle_parent
        && directory.file_name().is_some_and(|name| name == "dist")
        && let Some(project) = directory
            .parent()
            .filter(|path| path.join("Cargo.toml").is_file())
    {
        return project.to_path_buf();
    }
    let working = std::env::current_dir()
        .ok()
        .filter(|path| path.parent().is_some());
    working
        .as_deref()
        .or(bundle_parent)
        .unwrap_or(Path::new(env!("FRAMEKEEP_SOURCE")))
        .to_path_buf()
}

#[unsafe(no_mangle)]
extern "C" fn framekeep_init(preferences: *const c_char) {
    let path = Path::new(OsStr::from_bytes(
        unsafe { CStr::from_ptr(preferences) }.to_bytes(),
    ));
    let _ = APP.set(Mutex::new(App::load(&database_path(), path)));
}

#[unsafe(no_mangle)]
extern "C" fn framekeep_dispatch(command: *const c_char) -> *mut c_char {
    let result = (|| {
        let command = unsafe { CStr::from_ptr(command) };
        let args = serde_json::from_slice(command.to_bytes()).map_err(|error| error.to_string())?;
        APP.get()
            .unwrap()
            .lock()
            .map_err(|error| error.to_string())?
            .execute(args)
    })();
    let reply = match result {
        Ok(value) => json!({ "ok": true, "value": value }),
        Err(error) => json!({ "ok": false, "error": error }),
    };
    CString::new(reply.to_string()).unwrap().into_raw()
}

#[unsafe(no_mangle)]
extern "C" fn framekeep_path(generation: u32, id: usize, format: *const c_char) -> *mut c_char {
    let Some(state) = APP.get() else {
        return ptr::null_mut();
    };
    let Some(format) = Format::from_bytes(unsafe { CStr::from_ptr(format) }.to_bytes()) else {
        return ptr::null_mut();
    };
    let original = state
        .lock()
        .ok()
        .and_then(|app| app.photo(generation, id).ok()?.file(format).clone());
    original
        .and_then(|original| {
            original.verify().ok()?;
            path_string(&original.path).ok()
        })
        .map_or(ptr::null_mut(), CString::into_raw)
}

#[unsafe(no_mangle)]
extern "C" fn framekeep_free_string(string: *mut c_char) {
    if !string.is_null() {
        drop(unsafe { CString::from_raw(string) });
    }
}

#[unsafe(no_mangle)]
extern "C" fn framekeep_raf_jpeg(path: *const c_char, length: *mut usize) -> *mut u8 {
    let path = Path::new(OsStr::from_bytes(
        unsafe { CStr::from_ptr(path) }.to_bytes(),
    ));
    match library::raf_jpeg(path) {
        Ok(data) => {
            unsafe { *length = data.len() };
            Box::into_raw(data.into_boxed_slice()).cast()
        }
        Err(_) => ptr::null_mut(),
    }
}

#[unsafe(no_mangle)]
extern "C" fn framekeep_free_bytes(bytes: *mut u8, length: usize) {
    if !bytes.is_null() {
        drop(unsafe { Box::from_raw(ptr::slice_from_raw_parts_mut(bytes, length)) });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::Grade;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn reviews_versions_and_recycles_only_the_delete_queue()
    -> Result<(), Box<dyn std::error::Error>> {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let workspace = std::env::temp_dir().join(format!("framekeep-queue-{suffix}"));
        let photos = workspace.join("photos");
        let trash = workspace.join("trash");
        fs::create_dir_all(&photos)?;
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(workspace.clone());
        fs::create_dir(&trash)?;
        for name in ["a.png", "a.RAF", "b.png", "c.png"] {
            fs::write(photos.join(name), name)?;
        }

        let prefs = workspace.join("preferences.json");
        fs::write(
            &prefs,
            json!({
                "folder": photos,
                "photos": { photos.canonicalize()?.join("b.png").to_string_lossy(): { "kept": true } }
            }).to_string(),
        )?;
        let database = workspace.join("framekeep.sqlite3");
        let mut app = App::load(&database, &prefs);
        let initial = app.open(&photos)?;
        let generation = app.catalog.generation;
        assert_eq!(initial["photos"][1]["review"], "keep");
        assert_eq!(initial["photos"][1]["favorite"], true);
        assert_eq!(initial["photos"][2]["review"], "unreviewed");
        let no_heart = app.execute(Command::Favorite {
            generation,
            id: 1,
            favorite: false,
        })?;
        assert_eq!(no_heart["photo"]["review"], "keep");
        assert_eq!(no_heart["photo"]["favorite"], false);
        let grade = Adjustments {
            exposure: 125,
            grade: Grade::WarmFilm,
            strength: 70,
            ..Adjustments::default()
        };
        app.execute(Command::Adjust {
            generation,
            id: 0,
            format: Format::Raf,
            edits: grade,
        })?;
        app.execute(Command::Format {
            generation,
            id: 0,
            format: Format::Raf,
        })?;
        let queued = app.execute(Command::Review {
            generation,
            id: 0,
            review: Review::Delete,
        })?;
        assert_eq!(queued["photo"]["review"], "delete");
        assert_eq!(queued["photo"]["format"], "raf");
        assert_eq!(queued["photo"]["edits"]["raf"]["grade"], "warm_film");
        assert!(queued["photo"]["edits"].get("png").is_none());
        assert!(photos.join("a.png").exists());

        // Fail one operation and verify the surviving version remains queued.
        let partial = app.recycle_all(
            generation,
            false,
            |source| {
                if source.extension().is_some_and(|ext| ext == "RAF") {
                    return Err("simulated permission error".into());
                }
                let destination = trash.join(source.file_name().unwrap());
                fs::rename(source, &destination).map_err(|error| error.to_string())?;
                Ok(Some(destination))
            },
            export::render,
        )?;
        assert_eq!(partial["count"], 1);
        assert_eq!(partial["errorCount"], 1);
        assert_eq!(partial["canUndoTrash"], true);
        assert_eq!(partial["changes"][0]["photo"]["review"], "delete");
        assert!(partial["changes"][0]["photo"]["png"].is_null());
        assert!(photos.join("b.png").exists() && photos.join("c.png").exists());

        app.execute(Command::Review {
            generation,
            id: 0,
            review: Review::Keep,
        })?;
        let restored = app.undo(true, |from, to| {
            fs::rename(from, to).map_err(|error| error.to_string())
        })?;
        assert_eq!(restored["changes"][0]["photo"]["review"], "keep");
        assert_eq!(restored["changes"][0]["photo"]["png"], "a.png");
        assert_eq!(restored["canUndo"], true);
        assert_eq!(restored["canUndoTrash"], false);
        let reopened = App::load(&database, &prefs).open(&photos)?;
        assert_eq!(reopened["photos"][0]["review"], "keep");
        assert_eq!(reopened["photos"][0]["edits"]["raf"]["strength"], 70);

        app.execute(Command::Review {
            generation,
            id: 0,
            review: Review::Delete,
        })?;
        let recycled = app.recycle_all(
            generation,
            false,
            |source| {
                let destination = trash.join(source.file_name().unwrap());
                fs::rename(source, &destination).map_err(|error| error.to_string())?;
                Ok(Some(destination))
            },
            export::render,
        )?;
        assert_eq!(recycled["count"], 2);
        assert!(recycled["changes"][0]["photo"].is_null());
        let restored = app.undo(false, |from, to| {
            fs::rename(from, to).map_err(|error| error.to_string())
        })?;
        assert_eq!(restored["count"], 2);
        assert_eq!(restored["changes"][0]["photo"]["review"], "delete");

        app.execute(Command::Format {
            generation,
            id: 0,
            format: Format::Png,
        })?;
        for review in ["keep", "delete", "unreviewed"] {
            let undone = app.execute(Command::Undo)?;
            assert_eq!(undone["review"]["photo"]["review"], review);
            assert_eq!(undone["review"]["photo"]["format"], "png");
            assert_eq!(undone["review"]["photo"]["edits"]["raf"]["strength"], 70);
        }
        let undone = app.execute(Command::Undo)?;
        assert_eq!(undone["review"]["photo"]["review"], "keep");
        assert_eq!(undone["review"]["photo"]["favorite"], true);
        assert_eq!(undone["canUndo"], false);
        let reopened = App::load(&database, &prefs).open(&photos)?;
        assert_eq!(reopened["photos"][0]["review"], "unreviewed");
        assert_eq!(reopened["photos"][1]["favorite"], true);
        Ok(())
    }

    #[test]
    fn jpeg_export_and_raw_cleanup_preserve_sources_and_sqlite_state()
    -> Result<(), Box<dyn std::error::Error>> {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let workspace = std::env::temp_dir().join(format!("framekeep-export-{suffix}"));
        let photos = workspace.join("photos");
        let trash = workspace.join("trash");
        fs::create_dir_all(&photos)?;
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(workspace.clone());
        fs::create_dir(&trash)?;
        for name in ["a.RAF", "a.HIF", "b.RAF", "c.RAF", "d.RAF", "d.PNG"] {
            fs::write(photos.join(name), name)?;
        }
        let db = workspace.join("framekeep.sqlite3");
        let legacy = workspace.join("unused.json");
        let mut app = App::load(&db, &legacy);
        let initial = app.open(&photos)?;
        let generation = app.catalog.generation;
        assert_eq!(initial["photos"].as_array().unwrap().len(), 4);
        assert_eq!(initial["photos"][0]["format"], "hif");
        assert_eq!(initial["photos"][0]["raf"], "a.RAF");
        let render = |source: &Path, input, edits: Adjustments, output, target: &Path| {
            let marker = if output == Format::Png {
                assert_eq!(input, Format::Raf);
                assert_eq!(source.file_name().unwrap(), "b.RAF");
                b"PNG"
            } else {
                b"JPG"
            };
            let mut bytes = marker.to_vec();
            bytes.extend(fs::read(source).map_err(|error| error.to_string())?);
            bytes.extend(edits.exposure.to_le_bytes());
            fs::write(target, bytes).map_err(|error| error.to_string())
        };

        let refused = app.execute(Command::Format {
            generation,
            id: 0,
            format: Format::Png,
        });
        assert!(refused.is_err());
        assert_eq!(app.change_view(0)["photo"]["format"], "hif");
        assert!(!photos.join("a.png").exists());
        app.execute(Command::Format {
            generation,
            id: 0,
            format: Format::Raf,
        })?;
        assert_eq!(app.change_view(0)["photo"]["developed"], "hif");
        let edits = Adjustments {
            exposure: 125,
            ..Adjustments::default()
        };
        app.execute(Command::Adjust {
            generation,
            id: 0,
            format: Format::Raf,
            edits,
        })?;
        app.execute(Command::Adjust {
            generation,
            id: 0,
            format: Format::Hif,
            edits,
        })?;
        let reopened = App::load(&db, &legacy).open(&photos)?;
        assert_eq!(reopened["photos"][0]["edits"]["raf"]["exposure"], 125);

        let existing = photos.join("a_edited_0_11-23-2026.jpg");
        fs::write(&existing, b"existing file")?;
        let failed =
            app.save_new_photo(generation, 0, Format::Raf, "11-23-2026", |_, _, _, _, _| {
                Err("could not render".into())
            });
        assert!(failed.is_err());
        assert_eq!(app.change_view(0)["photo"]["edits"]["raf"]["exposure"], 125);
        let saved = app.save_new_photo(generation, 0, Format::Raf, "11-23-2026", render)?;
        assert_eq!(saved["filename"], "a_edited_1_11-23-2026.jpg");
        assert_eq!(saved["copy"]["photo"]["format"], "jpg");
        let copy = saved["copy"]["id"].as_u64().unwrap() as usize;
        app.catalog.photos[copy].jpg.as_ref().unwrap().verify()?;
        assert!(saved["source"]["photo"]["edits"].get("raf").is_none());
        assert_eq!(saved["source"]["photo"]["edits"]["hif"]["exposure"], 125);
        assert_eq!(fs::read(&existing)?, b"existing file");
        assert_eq!(fs::read(photos.join("a.RAF"))?, b"a.RAF");
        assert!(fs::read(photos.join("a_edited_1_11-23-2026.jpg"))?.starts_with(b"JPG"));

        for id in [0, 1, 3] {
            app.execute(Command::Review {
                generation,
                id,
                review: Review::Keep,
            })?;
        }
        let recycled = app.recycle_all(
            generation,
            true,
            |source| {
                let target = trash.join(source.file_name().unwrap());
                fs::rename(source, &target).map_err(|error| error.to_string())?;
                Ok(Some(target))
            },
            render,
        )?;
        assert_eq!(recycled["count"], 3);
        assert!(!photos.join("a.png").exists());
        assert_eq!(fs::read(photos.join("a.HIF"))?, b"a.HIF");
        app.catalog.photos[1].png.as_ref().unwrap().verify()?;
        assert_eq!(fs::read(photos.join("d.PNG"))?, b"d.PNG");
        assert!(photos.join("c.RAF").exists());
        for name in ["a.RAF", "b.RAF", "d.RAF"] {
            assert!(!photos.join(name).exists());
        }
        assert_eq!(recycled["changes"][0]["photo"]["format"], "hif");
        assert_eq!(recycled["changes"][1]["photo"]["review"], "keep");
        app.undo(true, |from, to| {
            fs::rename(from, to).map_err(|error| error.to_string())
        })?;
        assert_eq!(fs::read(photos.join("b.RAF"))?, b"b.RAF");

        let reopened = App::load(&db, &legacy).open(&photos)?;
        assert_eq!(reopened["photos"][0]["review"], "keep");
        assert!(reopened["photos"][0]["edits"].get("raf").is_none());
        assert!(
            reopened["photos"]
                .as_array()
                .unwrap()
                .iter()
                .any(|photo| photo["jpg"] == "a_edited_1_11-23-2026.jpg")
        );
        assert_eq!(&fs::read(db)?[..16], b"SQLite format 3\0");
        Ok(())
    }
}

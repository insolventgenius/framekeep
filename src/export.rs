use crate::library::{Adjustments, Format, Original};
use std::{
    ffi::{CStr, CString, OsStr, OsString, c_char, c_int, c_void},
    fs::{self, File, OpenOptions},
    io,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

unsafe extern "C" {
    fn framekeep_render_image(
        source: *const c_char,
        raw: bool,
        jpeg: bool,
        edits: *const c_char,
        output: *const c_char,
    ) -> *mut c_char;
    fn framekeep_rename_exclusive(
        source: *const c_char,
        output: *const c_char,
        copy_needed: *mut bool,
    ) -> c_int;
    fn framekeep_export_date(output: *mut c_char) -> bool;
    fn free(pointer: *mut c_void);
}

static TEMP_ID: AtomicU64 = AtomicU64::new(0);

pub struct StagedImage(PathBuf);

impl Drop for StagedImage {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn c_path(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes()).map_err(io::Error::other)
}

impl StagedImage {
    pub fn render(
        source: &Original,
        format: Format,
        edits: Adjustments,
        output_format: Format,
        renderer: impl FnOnce(&Path, Format, Adjustments, Format, &Path) -> Result<(), String>,
    ) -> Result<Self, String> {
        source.verify().map_err(|error| error.to_string())?;
        let parent = source
            .path
            .parent()
            .ok_or("The source has no parent folder.")?;
        let stage = loop {
            let id = TEMP_ID.fetch_add(1, Ordering::Relaxed);
            let path = parent.join(format!(".framekeep-{}-{id}.tmp", std::process::id()));
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(_) => break Self(path),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(format!(
                        "Could not prepare the image in {}: {error}",
                        parent.display()
                    ));
                }
            }
        };
        renderer(&source.path, format, edits, output_format, &stage.0)?;
        File::open(&stage.0)
            .and_then(|file| file.sync_all())
            .map_err(|error| error.to_string())?;
        source.verify().map_err(|error| error.to_string())?;
        Ok(stage)
    }

    pub fn publish(&self, destination: &Path) -> io::Result<Original> {
        let from = c_path(&self.0)?;
        let to = c_path(destination)?;
        let mut source = File::open(&self.0)?;
        let mut copy_needed = false;
        let code =
            unsafe { framekeep_rename_exclusive(from.as_ptr(), to.as_ptr(), &mut copy_needed) };
        if code == 0 {
            return published_file(destination, &source);
        }
        if !copy_needed {
            return Err(io::Error::from_raw_os_error(code));
        }
        // Volumes without exclusive rename can still reserve a new name atomically.
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(destination)?;
        let written = io::copy(&mut source, &mut output).and_then(|_| output.sync_all());
        // exFAT can change the inode when the first write allocates data for an empty file.
        let original = published_file(destination, &output);
        if let Err(error) = written {
            if let Ok(file) = original {
                rollback(&file);
            }
            return Err(error);
        }
        original
    }

    pub fn publish_edited(&self, source: &Path, date: &str) -> io::Result<Original> {
        let stem = source.file_stem().unwrap_or(OsStr::new("Photo"));
        for index in 0..=u32::MAX {
            let mut filename = OsString::from(stem);
            filename.push(format!("_edited_{index}_{date}.jpg"));
            match self.publish(&source.with_file_name(filename)) {
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                result => return result,
            }
        }
        Err(io::Error::other("No available filename for this edit"))
    }
}

fn published_file(destination: &Path, file: &File) -> io::Result<Original> {
    let original = Original::new(destination.to_path_buf(), &file.metadata()?);
    original.verify()?;
    Ok(original)
}

pub fn rollback(file: &Original) {
    if file.verify().is_ok() {
        let _ = fs::remove_file(&file.path);
    }
}

pub fn render(
    source: &Path,
    format: Format,
    edits: Adjustments,
    output_format: Format,
    output: &Path,
) -> Result<(), String> {
    let source = c_path(source).map_err(|error| error.to_string())?;
    let output = c_path(output).map_err(|error| error.to_string())?;
    let edits =
        CString::new(serde_json::to_vec(&edits).map_err(|error| error.to_string())?).unwrap();
    let error = unsafe {
        framekeep_render_image(
            source.as_ptr(),
            format == Format::Raf,
            output_format == Format::Jpg,
            edits.as_ptr(),
            output.as_ptr(),
        )
    };
    if error.is_null() {
        return Ok(());
    }
    let message = unsafe { CStr::from_ptr(error) }
        .to_string_lossy()
        .into_owned();
    unsafe { free(error.cast()) };
    Err(message)
}

pub fn local_date() -> Result<String, String> {
    let mut date = [0; 11];
    if !unsafe { framekeep_export_date(date.as_mut_ptr()) } {
        return Err("Could not format today’s date.".into());
    }
    Ok(unsafe { CStr::from_ptr(date.as_ptr()) }
        .to_string_lossy()
        .into_owned())
}

use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, File, Metadata},
    io::{self, Read, Seek, SeekFrom},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Png,
    Raf,
    Hif,
    Jpg,
}

impl Format {
    fn of(path: &Path) -> Option<Self> {
        let extension = path.extension()?.to_str()?.to_ascii_lowercase();
        let format = Self::from_bytes(extension.as_bytes())?;
        if format == Self::Jpg && !edited_stem(path.file_stem()?.to_str()?) {
            return None;
        }
        Some(format)
    }

    pub fn from_bytes(value: &[u8]) -> Option<Self> {
        match value {
            b"png" => Some(Self::Png),
            b"raf" => Some(Self::Raf),
            b"hif" => Some(Self::Hif),
            b"jpg" => Some(Self::Jpg),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Raf => "raf",
            Self::Hif => "hif",
            Self::Jpg => "jpg",
        }
    }
}

fn edited_stem(stem: &str) -> bool {
    let Some((_, suffix)) = stem.rsplit_once("_edited_") else {
        return false;
    };
    let Some((index, date)) = suffix.split_once('_') else {
        return false;
    };
    index.parse::<u32>().is_ok()
        && date.len() == 10
        && date.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 2 | 5) {
                byte == b'-'
            } else {
                byte.is_ascii_digit()
            }
        })
}

#[derive(Clone)]
pub struct Original {
    pub path: PathBuf,
    device: u64,
    inode: u64,
}

impl Original {
    pub fn new(path: PathBuf, metadata: &Metadata) -> Self {
        Self {
            path,
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }

    pub fn verify(&self) -> io::Result<()> {
        self.verify_at(&self.path)
    }

    pub fn verify_at(&self, path: &Path) -> io::Result<()> {
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.is_file() || metadata.dev() != self.device || metadata.ino() != self.inode {
            return Err(io::Error::other(
                "the file changed since the folder was scanned",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Default)]
pub struct Photo {
    pub png: Option<Original>,
    pub raf: Option<Original>,
    pub hif: Option<Original>,
    pub jpg: Option<Original>,
    pub database_id: i64,
    pub decision: Decision,
}

impl Photo {
    pub fn file(&self, format: Format) -> &Option<Original> {
        match format {
            Format::Png => &self.png,
            Format::Raf => &self.raf,
            Format::Hif => &self.hif,
            Format::Jpg => &self.jpg,
        }
    }

    pub fn file_mut(&mut self, format: Format) -> &mut Option<Original> {
        match format {
            Format::Png => &mut self.png,
            Format::Raf => &mut self.raf,
            Format::Hif => &mut self.hif,
            Format::Jpg => &mut self.jpg,
        }
    }

    pub fn files(&self) -> impl Iterator<Item = &Original> {
        [&self.png, &self.raf, &self.hif, &self.jpg]
            .into_iter()
            .flatten()
    }

    pub fn developed_files(&self) -> impl Iterator<Item = &Original> {
        self.png.iter().chain(&self.hif).chain(&self.jpg)
    }

    fn first(&self) -> &Original {
        self.files().next().expect("photos have at least one file")
    }

    pub fn view(&self, id: usize, root: &Path) -> PhotoView {
        let preference = self.decision;
        let developed = preference
            .developed
            .filter(|&format| format != Format::Raf && self.file(format).is_some())
            .or([Format::Png, Format::Hif, Format::Jpg]
                .into_iter()
                .find(|&format| self.file(format).is_some()));
        let format = preference
            .format
            .filter(|&format| self.file(format).is_some())
            .or(developed)
            .unwrap_or(Format::Raf);
        let first = &self.first().path;
        let name = first
            .file_stem()
            .unwrap_or(first.as_os_str())
            .to_string_lossy();
        let folder = first
            .parent()
            .and_then(|p| p.strip_prefix(root).ok())
            .unwrap_or(Path::new(""));
        PhotoView {
            id,
            name: name.into_owned(),
            folder: folder.to_string_lossy().into_owned(),
            png: self.png.as_ref().map(file_name),
            raf: self.raf.as_ref().map(file_name),
            hif: self.hif.as_ref().map(file_name),
            jpg: self.jpg.as_ref().map(file_name),
            format,
            developed,
            review: preference.review,
            favorite: preference.favorite,
            edits: preference.edits,
        }
    }
}

fn file_name(file: &Original) -> String {
    file.path
        .file_name()
        .unwrap_or(file.path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

#[derive(Serialize)]
pub struct PhotoView {
    id: usize,
    name: String,
    folder: String,
    png: Option<String>,
    raf: Option<String>,
    hif: Option<String>,
    jpg: Option<String>,
    format: Format,
    developed: Option<Format>,
    review: Review,
    favorite: bool,
    #[serde(skip_serializing_if = "PhotoEdits::is_default")]
    edits: PhotoEdits,
}

#[derive(Default, Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Review {
    #[default]
    Unreviewed,
    Keep,
    Delete,
}

#[derive(Default, Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Grade {
    #[default]
    Original,
    Vivid,
    WarmFilm,
    CoolFade,
    Monochrome,
    FujiFlogEterna,
    FujiFlogBleach,
    FujiFlogWdr,
    FujiFlog2Eterna,
    FujiFlog2Bleach,
    FujiFlog2Wdr,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(default)]
pub struct Adjustments {
    pub exposure: i16,
    pub contrast: i16,
    pub saturation: i16,
    pub warmth: i16,
    pub highlights: i16,
    pub shadows: i16,
    pub rotation: u8,
    pub grade: Grade,
    pub strength: u8,
}

impl Default for Adjustments {
    fn default() -> Self {
        Self {
            exposure: 0,
            contrast: 0,
            saturation: 0,
            warmth: 0,
            highlights: 0,
            shadows: 0,
            rotation: 0,
            grade: Grade::Original,
            strength: 100,
        }
    }
}

impl Adjustments {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    pub fn normalize(&mut self, format: Format) {
        self.exposure = self.exposure.clamp(-200, 200);
        self.contrast = self.contrast.clamp(-100, 100);
        self.saturation = self.saturation.clamp(-100, 100);
        self.warmth = self.warmth.clamp(-100, 100);
        self.highlights = self.highlights.clamp(-100, 100);
        self.shadows = self.shadows.clamp(-100, 100);
        self.rotation %= 4;
        if format != Format::Raf {
            self.grade = Grade::Original;
        }
        self.strength = if self.grade == Grade::Original {
            100
        } else {
            self.strength.min(100)
        };
    }
}

#[derive(Default, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct PhotoEdits {
    #[serde(skip_serializing_if = "Adjustments::is_default")]
    pub png: Adjustments,
    #[serde(skip_serializing_if = "Adjustments::is_default")]
    pub raf: Adjustments,
    #[serde(skip_serializing_if = "Adjustments::is_default")]
    pub hif: Adjustments,
    #[serde(skip_serializing_if = "Adjustments::is_default")]
    pub jpg: Adjustments,
}

impl PhotoEdits {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Default, Clone, Copy)]
pub struct Decision {
    pub format: Option<Format>,
    pub developed: Option<Format>,
    pub review: Review,
    pub favorite: bool,
    pub edits: PhotoEdits,
}

impl Decision {
    pub fn set_review(&mut self, review: Review) {
        self.favorite = review == Review::Keep && self.favorite;
        self.review = review;
    }

    pub fn set_favorite(&mut self, favorite: bool) {
        if favorite {
            self.review = Review::Keep;
        }
        self.favorite = favorite;
    }

    pub fn select_format(&mut self, format: Format) {
        self.format = Some(format);
        if format != Format::Raf {
            self.developed = Some(format);
        }
    }

    pub fn edits(&self, format: Format) -> Adjustments {
        match format {
            Format::Png => self.edits.png,
            Format::Raf => self.edits.raf,
            Format::Hif => self.edits.hif,
            Format::Jpg => self.edits.jpg,
        }
    }

    pub fn set_edits(&mut self, format: Format, mut edits: Adjustments) {
        edits.normalize(format);
        match format {
            Format::Png => self.edits.png = edits,
            Format::Raf => self.edits.raf = edits,
            Format::Hif => self.edits.hif = edits,
            Format::Jpg => self.edits.jpg = edits,
        }
    }
}

#[derive(Default)]
struct Group {
    png: Vec<Original>,
    raf: Vec<Original>,
    hif: Vec<Original>,
}

pub fn scan(root: &Path) -> io::Result<(Vec<Photo>, usize)> {
    let mut groups = BTreeMap::new();
    let mut exports = Vec::new();
    let mut skipped = 0;
    let mut folders = vec![root.to_path_buf()];
    while let Some(folder) = folders.pop() {
        let entries = match fs::read_dir(&folder) {
            Ok(entries) => entries,
            Err(error) if folder == root => return Err(error),
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        for result in entries {
            let Ok(entry) = result else {
                skipped += 1;
                continue;
            };
            let Ok(kind) = entry.file_type() else {
                skipped += 1;
                continue;
            };
            if kind.is_dir() {
                let name = entry.file_name();
                if !matches!(
                    name.to_str(),
                    Some(".Trash" | ".Trashes" | ".git" | ".Spotlight-V100")
                ) {
                    folders.push(entry.path());
                }
            } else if kind.is_file() {
                // macOS puts AppleDouble metadata alongside media on exFAT.
                if entry.file_name().as_encoded_bytes().starts_with(b"._") {
                    continue;
                }
                let path = entry.path();
                let Some(format) = Format::of(&path) else {
                    continue;
                };
                let Ok(metadata) = entry.metadata() else {
                    skipped += 1;
                    continue;
                };
                let stem = path
                    .file_stem()
                    .unwrap_or(path.as_os_str())
                    .to_string_lossy()
                    .to_lowercase();
                let original = Original::new(path, &metadata);
                if format == Format::Jpg {
                    exports.push(Photo {
                        jpg: Some(original),
                        ..Photo::default()
                    });
                    continue;
                }
                let group: &mut Group = groups.entry((folder.clone(), stem)).or_default();
                match format {
                    Format::Png => group.png.push(original),
                    Format::Raf => group.raf.push(original),
                    Format::Hif => group.hif.push(original),
                    Format::Jpg => unreachable!(),
                }
            }
        }
    }

    let mut photos = Vec::new();
    for (_, mut group) in groups {
        let first = photos.len();
        group.png.sort_unstable_by(|a, b| a.path.cmp(&b.path));
        group.raf.sort_unstable_by(|a, b| a.path.cmp(&b.path));
        // Preserve exact-case matches on case-sensitive volumes before pairing the rest.
        let mut remaining = Vec::new();
        for png in group.png {
            if let Some(index) = group
                .raf
                .iter()
                .position(|raf| raf.path.file_stem() == png.path.file_stem())
            {
                photos.push(Photo {
                    png: Some(png),
                    raf: Some(group.raf.remove(index)),
                    ..Photo::default()
                });
            } else {
                remaining.push(png);
            }
        }
        let mut png = remaining.into_iter();
        let mut raf = group.raf.into_iter();
        loop {
            let photo = Photo {
                png: png.next(),
                raf: raf.next(),
                ..Photo::default()
            };
            if photo.files().next().is_none() {
                break;
            }
            photos.push(photo);
        }

        group.hif.sort_unstable_by(|a, b| a.path.cmp(&b.path));
        let mut unmatched = Vec::new();
        for hif in group.hif {
            if let Some(photo) = photos[first..].iter_mut().find(|photo| {
                photo.hif.is_none()
                    && photo
                        .files()
                        .any(|file| file.path.file_stem() == hif.path.file_stem())
            }) {
                photo.hif = Some(hif);
            } else {
                unmatched.push(hif);
            }
        }
        let mut available = photos[first..]
            .iter_mut()
            .filter(|photo| photo.hif.is_none());
        let mut standalone = Vec::new();
        for hif in unmatched {
            if let Some(photo) = available.next() {
                photo.hif = Some(hif);
            } else {
                standalone.push(Photo {
                    hif: Some(hif),
                    ..Photo::default()
                });
            }
        }
        photos.extend(standalone);
    }
    exports.sort_unstable_by(|left, right| left.first().path.cmp(&right.first().path));
    photos.extend(exports);
    Ok((photos, skipped))
}

/// Locate the camera-generated JPEG described by the RAF header.
pub fn raf_jpeg(path: &Path) -> io::Result<Vec<u8>> {
    let mut file = File::open(path)?;
    let mut header = [0; 92];
    file.read_exact(&mut header)?;
    if &header[..16] != b"FUJIFILMCCD-RAW " {
        return Err(io::Error::other("invalid RAF header"));
    }
    let offset = u32::from_be_bytes(header[84..88].try_into().unwrap()) as u64;
    let length = u32::from_be_bytes(header[88..92].try_into().unwrap()) as u64;
    if offset < 92
        || !(4..=32 * 1024 * 1024).contains(&length)
        || offset + length > file.metadata()?.len()
    {
        return Err(io::Error::other("invalid RAF preview range"));
    }
    file.seek(SeekFrom::Start(offset))?;
    let mut jpeg = vec![0; length as usize];
    file.read_exact(&mut jpeg)?;
    if !jpeg.starts_with(&[0xff, 0xd8, 0xff]) {
        return Err(io::Error::other("invalid RAF JPEG"));
    }
    Ok(jpeg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn finds_pairs_and_validates_raf_previews() -> io::Result<()> {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("framekeep-test-{}-{suffix}", std::process::id()));
        fs::create_dir(&root)?;
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(root.clone());
        fs::create_dir(root.join("other"))?;
        fs::write(root.join("Shot.PNG"), b"fixture")?;
        fs::write(root.join("shot.raf"), b"fixture")?;
        fs::write(root.join("other/shot.raf"), b"fixture")?;
        fs::write(root.join("not-a-photo.txt"), b"fixture")?;
        fs::write(root.join("._Shot.PNG"), b"AppleDouble metadata")?;
        std::os::unix::fs::symlink(root.join("Shot.PNG"), root.join("alias.png"))?;

        let (photos, skipped) = scan(&root)?;
        assert_eq!(skipped, 0);
        assert_eq!(photos.len(), 2);
        let (id, pair) = photos
            .iter()
            .enumerate()
            .find(|(_, photo)| photo.png.is_some() && photo.raf.is_some())
            .expect("same-folder variants should be paired");
        assert_eq!(pair.view(id, &root).format, Format::Png);
        let mut pair = pair.clone();
        pair.decision.set_favorite(true);
        pair.decision.format = Some(Format::Raf);
        let result = pair.view(id, &root);
        assert_eq!(result.review, Review::Keep);
        assert!(result.favorite);
        assert_eq!(result.format, Format::Raf);
        assert_eq!(result.png.as_deref(), Some("Shot.PNG"));
        assert_eq!(result.raf.as_deref(), Some("shot.raf"));

        let raf = root.join("shot.raf");
        let jpeg = include_bytes!("../ui/welcome.jpg");
        let mut header = [0; 108];
        header[..16].copy_from_slice(b"FUJIFILMCCD-RAW ");
        header[84..88].copy_from_slice(&108u32.to_be_bytes());
        header[88..92].copy_from_slice(&(jpeg.len() as u32).to_be_bytes());
        fs::write(&raf, [&header[..], &jpeg[..]].concat())?;
        assert_eq!(raf_jpeg(&raf)?, jpeg);
        header[88..92].copy_from_slice(&u32::MAX.to_be_bytes());
        fs::write(&raf, header)?;
        assert!(raf_jpeg(&raf).is_err());
        Ok(())
    }
}

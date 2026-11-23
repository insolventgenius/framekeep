use crate::library::Grade;
use serde::Serialize;
use std::{
    ffi::{CStr, c_char},
    fs,
    path::{Path, PathBuf},
    ptr,
    sync::OnceLock,
};

#[derive(Clone, Copy, Serialize)]
struct Look {
    grade: Grade,
    label: &'static str,
    #[serde(skip)]
    file: &'static str,
    #[serde(skip)]
    log2: bool,
}

const LOOKS: [Look; 6] = [
    Look {
        grade: Grade::FujiFlogEterna,
        label: "ETERNA (F-Log)",
        file: "XT50_FLog_FGamut_to_ETERNA_BT.709_33grid_V.1.00.cube",
        log2: false,
    },
    Look {
        grade: Grade::FujiFlogBleach,
        label: "ETERNA Bleach Bypass (F-Log)",
        file: "XT50_FLog_FGamut_to_ETERNA-BB_BT.709_33grid_V.1.00.cube",
        log2: false,
    },
    Look {
        grade: Grade::FujiFlogWdr,
        label: "Wide Dynamic Range (F-Log)",
        file: "XT50_FLog_FGamut_to_WDR_BT.709_33grid_V.1.00.cube",
        log2: false,
    },
    Look {
        grade: Grade::FujiFlog2Eterna,
        label: "ETERNA (F-Log2)",
        file: "XT50_FLog2_FGamut_to_ETERNA_BT.709_33grid_V.1.00.cube",
        log2: true,
    },
    Look {
        grade: Grade::FujiFlog2Bleach,
        label: "ETERNA Bleach Bypass (F-Log2)",
        file: "XT50_FLog2_FGamut_to_ETERNA-BB_BT.709_33grid_V.1.00.cube",
        log2: true,
    },
    Look {
        grade: Grade::FujiFlog2Wdr,
        label: "Wide Dynamic Range (F-Log2)",
        file: "XT50_FLog2_FGamut_to_WDR_BT.709_33grid_V.1.00.cube",
        log2: true,
    },
];

struct Cube {
    dimension: usize,
    rgba: Vec<f32>,
}

static DIRECTORY: OnceLock<PathBuf> = OnceLock::new();
static CUBES: OnceLock<[Option<Cube>; LOOKS.len()]> = OnceLock::new();

pub fn set_directory(path: PathBuf) {
    let _ = DIRECTORY.set(path);
}

pub fn options() -> serde_json::Value {
    let available: Vec<_> = LOOKS
        .iter()
        .zip(cubes())
        .filter_map(|(look, cube)| cube.as_ref().map(|_| look))
        .collect();
    serde_json::json!(available)
}

fn cubes() -> &'static [Option<Cube>; LOOKS.len()] {
    CUBES.get_or_init(|| {
        DIRECTORY.get().map_or_else(
            || [const { None }; LOOKS.len()],
            |directory| load(directory),
        )
    })
}

fn load(directory: &Path) -> [Option<Cube>; LOOKS.len()] {
    let mut loaded = [const { None }; LOOKS.len()];
    let mut directories = vec![directory.to_path_buf()];
    while let Some(directory) = directories.pop() {
        let Ok(entries) = fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                directories.push(entry.path());
                continue;
            }
            if !kind.is_file() {
                continue;
            }
            let file = entry.file_name();
            // Some unzip tools keep the archive's Windows separators in the filename.
            let file = file.to_string_lossy();
            let name = file.rsplit('\\').next().unwrap_or(&file);
            let Some(index) = LOOKS
                .iter()
                .position(|look| look.file.eq_ignore_ascii_case(name))
            else {
                continue;
            };
            if loaded[index].is_none()
                && entry
                    .metadata()
                    .is_ok_and(|metadata| metadata.len() <= 8 * 1024 * 1024)
            {
                loaded[index] = fs::read_to_string(entry.path())
                    .ok()
                    .and_then(|data| parse_cube(&data));
            }
        }
    }
    loaded
}

fn parse_cube(data: &str) -> Option<Cube> {
    let mut dimension: usize = 0;
    let mut rgba = Vec::new();
    for line in data.lines() {
        let mut fields = line.split('#').next()?.split_whitespace();
        let Some(first) = fields.next() else { continue };
        match first {
            "TITLE" => (),
            "DOMAIN_MIN" | "DOMAIN_MAX" => {
                let expected = if first == "DOMAIN_MIN" { 0.0 } else { 1.0 };
                let domain: Vec<_> = fields
                    .map(str::parse::<f32>)
                    .collect::<Result<_, _>>()
                    .ok()?;
                if domain != [expected; 3] {
                    return None;
                }
            }
            "LUT_3D_SIZE" => {
                if dimension != 0 {
                    return None;
                }
                dimension = fields.next()?.parse().ok()?;
                if !(2..=64).contains(&dimension) || fields.next().is_some() {
                    return None;
                }
                rgba.reserve(dimension.pow(3) * 4);
            }
            _ => {
                if dimension == 0 || rgba.len() >= dimension.pow(3) * 4 {
                    return None;
                }
                for value in [first, fields.next()?, fields.next()?] {
                    let value: f32 = value.parse().ok()?;
                    if !value.is_finite() {
                        return None;
                    }
                    // The display-referred LUT targets a BT.709 reference monitor (BT.1886).
                    // Prepare linear Rec.709 values for Core Image's working color space.
                    rgba.push(value.clamp(0.0, 1.0).powf(2.4));
                }
                if fields.next().is_some() {
                    return None;
                }
                rgba.push(1.0);
            }
        }
    }
    (dimension > 0 && rgba.len() == dimension.pow(3) * 4).then_some(Cube { dimension, rgba })
}

#[unsafe(no_mangle)]
extern "C" fn framekeep_lut_cube(
    grade: *const c_char,
    dimension: *mut usize,
    is_log2: *mut bool,
) -> *const f32 {
    let Ok(id) = unsafe { CStr::from_ptr(grade) }.to_str() else {
        return ptr::null();
    };
    let input = serde::de::value::StrDeserializer::<serde::de::value::Error>::new(id);
    let Ok(grade) = <Grade as serde::Deserialize>::deserialize(input) else {
        return ptr::null();
    };
    let Some(index) = LOOKS.iter().position(|look| look.grade == grade) else {
        return ptr::null();
    };
    let Some(cube) = &cubes()[index] else {
        return ptr::null();
    };
    unsafe {
        *dimension = cube.dimension;
        *is_log2 = LOOKS[index].log2;
    }
    // The application-wide table owns the immutable float buffer for the life of the process.
    cube.rgba.as_ptr()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_dimensions_and_converts_display_values_to_linear() {
        let valid = "TITLE \"Fixture\"\nLUT_3D_SIZE 2\n".to_owned() + &"0.0 0.5 1.0\n".repeat(8);
        let cube = parse_cube(&valid).unwrap();
        assert_eq!(cube.dimension, 2);
        assert_eq!(cube.rgba.len(), 32);
        assert_eq!(cube.rgba[..4], [0.0, 0.5_f32.powf(2.4), 1.0, 1.0]);
        assert!(parse_cube(&(valid.clone() + "1 1 1\n")).is_none());
        assert!(parse_cube(&valid.replace("0.5", "NaN")).is_none());
        assert!(parse_cube(&valid.replace("LUT_3D_SIZE 2", "LUT_3D_SIZE 3")).is_none());
    }
}

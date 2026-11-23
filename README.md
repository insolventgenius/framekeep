# Framekeep

Framekeep is a local macOS photo browser built in Rust with a native AppKit window. Open a folder to scan it and its subfolders for PNG, Fujifilm RAF, and Fujifilm HIF files, plus JPEGs saved by Framekeep.

## Requirements and dependencies

- **macOS 13 Ventura or later** to run the app.
- **A current stable Rust toolchain with Cargo** to build from source. The project uses Rust edition 2024.
- **Apple’s Command Line Tools for Xcode or Xcode** for Clang and the macOS SDK. To install the standalone tools, run `xcode-select --install`.
- **Rust crates.** Cargo installs `serde` (with derive support) and `serde_json` for serialization, and `cc` to compile the Objective-C host. `Cargo.lock` records the exact versions of all crates.
- **macOS system libraries.** The app links against SQLite (`libsqlite3`), AppKit, Foundation, WebKit, ImageIO, CoreGraphics, and CoreImage. WebKit runs the bundled HTML, CSS, and JavaScript directly.

## Build and run

From the project root, build and open the release app.

```sh
./scripts/package.sh
open dist/Framekeep.app
```

The packaging script uses Bash and builds with the locked Cargo dependencies. It renders `Small Robot.png` as a rounded app icon, copies the resources into `dist/Framekeep.app`, and ad-hoc signs the bundle with `codesign`. Once built, you can also double-click the app in Finder.

For development, run the app with Cargo. You can pass a folder to open at launch.

```sh
cargo run --locked
cargo run --locked -- "/path/to/photos"
```

Run the tests from the same directory with `cargo test --locked`.

## Review photos

1. Click **Open folder** or press **⌘O**. Start in **Unreviewed** and use **⌘R** to rescan after files change on disk.
2. In **Viewer**, browse with **←** and **→** or the filmstrip. Switch to **Grid** with **G** and use the thumbnail-size slider. Click a thumbnail to open it in Viewer.
3. Click **Keep** or press **K** to mark a keeper. Use the heart or press **H** to add it to **Favorites** and **Keep**. Removing the heart leaves it in Keep. Use **Return to Unreviewed** to clear a decision.
4. Click **Delete** or press **⌫** to queue the photo. Open the **Delete** category to review the queue.

Press **⌘Z** to undo the latest review decision or move to Trash. Press it again to step back through earlier actions in the current folder. Press **?** to open the keyboard shortcut list.

## Compare versions

Same-stem PNG, HIF, and RAF files in the same folder can share a viewer entry. Use **RAW** and **Developed**, or press **V**, to change versions. With only a RAF available, selecting **Developed** creates a PNG beside it. If both HIF and PNG are available, use the Developed dropdown to switch between them.

Drag the large preview to Finder or another app to copy the source file for the displayed version. RAF thumbnails use the embedded camera JPEG when available. The viewer and exports use the macOS RAW decoder when supported, with the embedded JPEG as a fallback. Camera support varies by macOS version. Framekeep decodes HIF as HEIF. [Apple’s macOS 27 RAW compatibility list](https://support.apple.com/en-us/127493), [Fujifilm’s HIF format documentation](https://fujifilm-dsc.com/en/manual/x-h2s/menu_shooting/image_quality_setting/)

## Edit photos

Choose **Edit** to adjust exposure, contrast, highlights, shadows, saturation, warmth, and rotation. RAF versions also support **Vivid**, **Warm Film**, **Cool Fade**, and **Monochrome** grades with adjustable intensity. Framekeep saves pending edits automatically for the selected file version.

- **Original** previews the source. **Reset** clears the adjustments. **Cancel** restores the adjustments that were present when you opened the editor.
- **Done** closes the editor and keeps your adjustments. Each file version has its own settings, and the source image file stays intact.
- **Save New Photo** writes an edited JPEG beside its source as `<name>_edited_<index>_<MM-DD-YYYY>.jpg`, using the Mac’s local date and the first available index starting at 0. After the copy is saved, Framekeep clears the adjustments on the source version and the new JPEG appears in **Unreviewed**.

### Fujifilm X-T50 LUTs

Download the **X-T50 Ver. 1.00** 3D LUT ZIP from Fujifilm and extract it into `vendor/x-t50-3d-lut-v100/` for local use. Git ignores the extracted folder and the matching ZIP in `vendor/`. The app loads the LUTs from that folder at launch. Set `FRAMEKEEP_LUT_DIR` when launching from a shell to use a different local folder. [Fujifilm’s 3D LUT downloads](https://www.fujifilm-x.com/global/support/download/lut/)

Select **RAW**, open **Edit**, and choose a look under **Fujifilm X-T50** in **Color grade**. The picker shows the ETERNA, ETERNA Bleach Bypass, and Wide Dynamic Range looks in both F-Log and F-Log2. The intensity slider blends the look into the photo, and **Save New Photo** includes it in the JPEG. Framekeep adapts these video LUTs to still photos using Fujifilm’s published F-Gamut and log curves. [F-Log data sheet](https://dl.fujifilm-x.com/technical-data/F-Log_DataSheet_E_Ver.1.2.pdf), [F-Log2 data sheet](https://dl.fujifilm-x.com/technical-data/F-Log2_DataSheet_E_Ver.1.1.pdf)

## Move files to Trash

- In **Delete**, choose **Recycle all** and confirm to move every file version of the queued photos to macOS Trash.
- In **Keep**, choose **Trash all RAWs** and confirm to move only the RAF files for kept photos, including favorites, to macOS Trash. Framekeep keeps existing developed versions such as HIF and PNG as they are. It exports a PNG from the RAF only when that photo has no developed version. The progress bar counts the RAWs processed. If Framekeep cannot verify or create a developed version, it leaves the RAF in place.
- Click **Undo Trash** to restore the latest batch before switching folders, rescanning, or quitting. PNGs created during RAW cleanup stay in the folder.

## Local data

Review categories, favorites, format choices, adjustments, and the last opened folder are stored in `framekeep.sqlite3`. Running with Cargo from the project root or opening the bundled app in `dist` puts the database in the project root. Set `FRAMEKEEP_DATABASE` when launching from a shell to use a different path.

The `photos` table holds review state and adjustments (`edits_json`), and `photo_paths` associates file versions with each photo. When a new database is first opened, Framekeep imports earlier preferences from `~/Library/Application Support/Framekeep/preferences.json` if present.

## Visual reference

The supplied web archive informed the dark surfaces, lime highlights, and image-led layout. The original welcome photograph was created with the built-in image generation tool.

> Use case: photorealistic-natural. Asset type: wide hero photograph for the welcome screen of a dark desktop photo review app. Create a landscape photograph of a single small cabin with golden lit windows in the Dolomites, under monumental slate-grey jagged mountains and folds of mist at muted amber blue hour. Dark pine trees, natural rugged grass and stone, a quiet cinematic atmosphere, Fujifilm editorial travel photography with subtle film grain, authentic fine detail. Landscape 3:2 framing designed to crop to a wide 2.4:1 banner, place the visually distinctive cabin at right-center, leave the left third deep and shadowy with quieter detail for a UI overlay. Refined natural lighting; muted cool shadows and faint warm highlights, no artificial glow. No people, text, logos, graphics, watermarks or interface.

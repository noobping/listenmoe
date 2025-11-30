#[cfg(not(feature = "setup"))]
#[cfg(debug_assertions)]
pub const APP_ID: &str = concat!("dev.noobping.", env!("CARGO_PKG_NAME"), ".develop");
#[cfg(not(feature = "setup"))]
#[cfg(not(debug_assertions))]
pub const APP_ID: &str = concat!("dev.noobping.", env!("CARGO_PKG_NAME"));

pub const RESOURCE_ID: &str = concat!("/dev/noobping/", env!("CARGO_PKG_NAME"));

use std::{fs, path::Path};

fn main() {
    // Directories
    let data_dir = Path::new("data");

    // Tell Cargo when to rerun the build script
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=data");

    // Ensure data/ exists
    fs::create_dir_all(&data_dir).unwrap();

    // Collect all .svg icon files in data/icons/
    let mut icons: Vec<String> = Vec::new();
    collect_svg_icons(&data_dir, &data_dir, &mut icons);
    icons.sort();

    // Generate resources.xml content
    let mut xml = String::from("<gresources>\n");
    xml.push_str(&format!("\t<gresource prefix=\"{RESOURCE_ID}\">\n"));
    for f in &icons {
        xml.push_str(&format!("\t\t<file>{}</file>\n", f));
    }
    xml.push_str("\t</gresource>\n</gresources>\n");

    // Write resources.xml there
    fs::write(data_dir.join("resources.xml"), xml).unwrap();

    // Compile GResources from data/resources.xml into resources.gresource
    glib_build_tools::compile_resources(&["data"], "data/resources.xml", "compiled.gresource");

    #[cfg(not(feature = "setup"))]
    desktop_file();
}

/// Recursively collect all `.svg` files under `dir`,
/// and push their path *relative to `data_dir`* into `icons`.
fn collect_svg_icons(dir: &Path, data_dir: &Path, icons: &mut Vec<String>) {
    for entry in fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();

        if path.is_dir() {
            collect_svg_icons(&path, data_dir, icons);
        } else if path.extension().and_then(|e| e.to_str()) == Some("svg") {
            // Strip "data/" so we end up with e.g. "icons/foo/bar.svg"
            let rel = path.strip_prefix(data_dir).unwrap();
            icons.push(rel.to_string_lossy().into_owned());
        }
    }
}

#[cfg(not(feature = "setup"))]
fn desktop_file() {
    use std::{fs, path::Path};
    let project = env!("CARGO_PKG_NAME");
    let dir = Path::new(".");
    let version = env!("CARGO_PKG_VERSION");
    let comment = option_env!("CARGO_PKG_DESCRIPTION").unwrap_or("Password manager");
    let contents = format!(
        "[Desktop Entry]
Type=Application
Version={version}
Name={project}
Comment={comment}
Exec={project} %u
Icon={APP_ID}
Terminal=false
Categories=AudioVideo;Player;
"
    );
    fs::write(&dir.join(format!("{project}.desktop")), contents)
        .expect("Can not build desktop file")
}

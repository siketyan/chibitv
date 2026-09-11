//! Tells the crate what the FFmpeg it links was built with.
//!
//! FFmpeg itself is built by `build.sh`, which installs it where the
//! workspace's `.cargo/config.toml` points rusty_ffmpeg at, and leaves a list
//! of the backends it was built with next to it. This script turns that list
//! into `cfg`s and checks that every backend the Cargo features ask for is in
//! it, since a feature without its backend would only fail at run time. The
//! features also decide whether FFmpeg is linked at all: without any of them
//! nothing here applies and the crate builds without FFmpeg.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

/// A backend feature of the crate, and the platforms it exists on.
struct Feature {
    /// The name in the `CARGO_FEATURE_*` environment.
    name: &'static str,
    /// The name `build.sh` takes and records.
    backend: &'static str,
    /// The `target_os` values it exists on; empty for all of them.
    platforms: &'static [&'static str],
}

const FEATURES: &[Feature] = &[
    Feature {
        name: "VIDEOTOOLBOX",
        backend: "videotoolbox",
        platforms: &["macos"],
    },
    Feature {
        name: "NVIDIA",
        backend: "nvidia",
        platforms: &["linux", "windows"],
    },
    Feature {
        name: "QSV",
        backend: "qsv",
        platforms: &["linux", "windows"],
    },
    Feature {
        name: "VAAPI",
        backend: "vaapi",
        platforms: &["linux"],
    },
    Feature {
        name: "AMF",
        backend: "amf",
        platforms: &["windows", "linux"],
    },
    Feature {
        name: "X264",
        backend: "x264",
        platforms: &[],
    },
    Feature {
        name: "X265",
        backend: "x265",
        platforms: &[],
    },
    Feature {
        name: "SVT_AV1",
        backend: "svt-av1",
        platforms: &[],
    },
];

/// What `build.sh` can build besides the backends: the MPEG-2 encoder the
/// tests make their input with.
const EXTRAS: &[&str] = &["mpeg2-encoder"];

fn cfg_name(backend: &str) -> String {
    format!("ffmpeg_{}", backend.replace('-', "_"))
}

fn main() {
    println!("cargo::rustc-check-cfg=cfg(ffmpeg)");
    for backend in FEATURES
        .iter()
        .map(|feature| feature.backend)
        .chain(EXTRAS.iter().copied())
    {
        println!("cargo::rustc-check-cfg=cfg({})", cfg_name(backend));
    }
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-env-changed=FFMPEG_PKG_CONFIG_PATH");

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let mut wanted = Vec::new();
    for feature in FEATURES {
        if env::var_os(format!("CARGO_FEATURE_{}", feature.name)).is_none() {
            continue;
        }
        if !feature.platforms.is_empty() && !feature.platforms.contains(&target_os.as_str()) {
            println!(
                "cargo::warning=The {} feature of chibitv_ffmpeg does nothing on {target_os}",
                feature.backend
            );
            continue;
        }
        wanted.push(feature.backend);
    }
    if wanted.is_empty() {
        return;
    }

    let built = built_backends();
    // The list makes Cargo run this script again when FFmpeg is rebuilt.
    println!("cargo::rerun-if-changed={}", built.path.display());

    let missing: Vec<&str> = wanted
        .iter()
        .copied()
        .filter(|backend| !built.backends.iter().any(|built| built == backend))
        .collect();
    if !missing.is_empty() {
        panic!(
            "The FFmpeg in {} was built without {}. Run crates/chibitv_ffmpeg/build.sh with \
             every backend the features ask for, for instance:\n    crates/chibitv_ffmpeg/build.sh {}",
            built.prefix.display(),
            missing.join(", "),
            wanted.join(" ")
        );
    }

    println!("cargo::rustc-cfg=ffmpeg");
    for backend in &built.backends {
        println!("cargo::rustc-cfg={}", cfg_name(backend));
    }
}

struct Built {
    prefix: PathBuf,
    path: PathBuf,
    backends: Vec<String>,
}

/// Reads the list `build.sh` left in the prefix that `FFMPEG_PKG_CONFIG_PATH`
/// points into.
fn built_backends() -> Built {
    let pkg_config_path = env::var_os("FFMPEG_PKG_CONFIG_PATH").map(PathBuf::from).unwrap_or_else(|| {
        panic!(
            "FFMPEG_PKG_CONFIG_PATH is not set; the workspace's .cargo/config.toml sets it to where \
             crates/chibitv_ffmpeg/build.sh installs FFmpeg"
        )
    });
    let prefix = pkg_config_path
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| pkg_config_path.clone());
    let path = prefix.join("chibitv-backends");
    let backends = fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "No FFmpeg built by crates/chibitv_ffmpeg/build.sh in {} ({error}). Run the script \
             with the backends the features ask for first.",
            prefix.display()
        )
    });
    Built {
        prefix,
        path,
        backends: backends
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(String::from)
            .collect(),
    }
}

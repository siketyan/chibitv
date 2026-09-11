//! Builds the minimal FFmpeg the crate links, and the C shim over it.
//!
//! Which parts of FFmpeg go in follows from the Cargo features: nothing at all
//! without a backend feature, so that the crate is free to build everywhere
//! and only a build that wants transcoding pays for it. FFmpeg's sources are
//! fetched into `vendor/` next to this file and built there too, once per
//! configuration rather than once per Cargo profile, since FFmpeg is compiled
//! optimised either way. `CHIBITV_FFMPEG_VENDOR_DIR` moves that directory, for
//! instance to a cache, and `CHIBITV_FFMPEG_SOURCE` points at an FFmpeg source
//! tree to use instead of the release archive.

use std::collections::hash_map::DefaultHasher;
use std::env;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;

const FFMPEG_VERSION: &str = "9.0.1";

/// A feature of the crate and the platforms it exists on.
struct Feature {
    /// The name in the `CARGO_FEATURE_*` environment.
    name: &'static str,
    /// The `cfg` it becomes when it takes effect.
    cfg: &'static str,
    /// The `target_os` values it exists on; empty for all of them.
    platforms: &'static [&'static str],
    /// Whether it is a transcoding backend, as opposed to test support.
    backend: bool,
}

const FEATURES: &[Feature] = &[
    Feature {
        name: "VIDEOTOOLBOX",
        cfg: "ffmpeg_videotoolbox",
        platforms: &["macos"],
        backend: true,
    },
    Feature {
        name: "NVIDIA",
        cfg: "ffmpeg_nvidia",
        platforms: &["linux", "windows"],
        backend: true,
    },
    Feature {
        name: "QSV",
        cfg: "ffmpeg_qsv",
        platforms: &["linux", "windows"],
        backend: true,
    },
    Feature {
        name: "VAAPI",
        cfg: "ffmpeg_vaapi",
        platforms: &["linux"],
        backend: true,
    },
    Feature {
        name: "AMF",
        cfg: "ffmpeg_amf",
        platforms: &["windows", "linux"],
        backend: true,
    },
    Feature {
        name: "X264",
        cfg: "ffmpeg_x264",
        platforms: &[],
        backend: true,
    },
    Feature {
        name: "X265",
        cfg: "ffmpeg_x265",
        platforms: &[],
        backend: true,
    },
    Feature {
        name: "SVT_AV1",
        cfg: "ffmpeg_svt_av1",
        platforms: &[],
        backend: true,
    },
    Feature {
        name: "MPEG2_ENCODER",
        cfg: "ffmpeg_mpeg2_encoder",
        platforms: &[],
        backend: false,
    },
];

/// What the build ends up with, as a set of the `cfg` names above.
struct Config {
    target_os: String,
    target_env: String,
    target_arch: String,
    enabled: Vec<&'static str>,
}

impl Config {
    fn from_env() -> Self {
        let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
        let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
        let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
        let mut enabled = Vec::new();

        for feature in FEATURES {
            if env::var_os(format!("CARGO_FEATURE_{}", feature.name)).is_none() {
                continue;
            }
            if !feature.platforms.is_empty() && !feature.platforms.contains(&target_os.as_str()) {
                println!(
                    "cargo::warning=The {} feature of chibitv_ffmpeg does nothing on {}",
                    feature.name.to_lowercase().replace('_', "-"),
                    target_os
                );
                continue;
            }
            enabled.push(feature.cfg);
        }

        Self {
            target_os,
            target_env,
            target_arch,
            enabled,
        }
    }

    fn has(&self, cfg: &str) -> bool {
        self.enabled.contains(&cfg)
    }

    fn has_backend(&self) -> bool {
        FEATURES
            .iter()
            .any(|feature| feature.backend && self.has(feature.cfg))
    }

    fn is_windows(&self) -> bool {
        self.target_os == "windows"
    }

    fn is_linux(&self) -> bool {
        self.target_os == "linux"
    }
}

fn main() {
    println!("cargo::rustc-check-cfg=cfg(ffmpeg)");
    for feature in FEATURES {
        println!("cargo::rustc-check-cfg=cfg({})", feature.cfg);
    }
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-changed=csrc");
    println!("cargo::rerun-if-env-changed=CHIBITV_FFMPEG_VENDOR_DIR");
    println!("cargo::rerun-if-env-changed=CHIBITV_FFMPEG_SOURCE");
    println!("cargo::rerun-if-env-changed=CHIBITV_FFMPEG_AMF_INCLUDE");

    let config = Config::from_env();
    if !config.has_backend() {
        return;
    }

    println!("cargo::rustc-cfg=ffmpeg");
    for cfg in &config.enabled {
        println!("cargo::rustc-cfg={cfg}");
    }

    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let vendor_dir = env::var_os("CHIBITV_FFMPEG_VENDOR_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest_dir.join("vendor"));
    fs::create_dir_all(&vendor_dir).expect("could not create the vendor directory");

    let source_dir = source_dir(&vendor_dir);
    let arguments = configure_arguments(&config);
    let build_dir = build_ffmpeg(&config, &vendor_dir, &source_dir, &arguments);

    cc::Build::new()
        .file(manifest_dir.join("csrc/chibitv_ffmpeg.c"))
        .include(&build_dir)
        .include(&source_dir)
        .warnings(true)
        .compile("chibitv_ffmpeg");

    link(&config, &build_dir);
}

/// The FFmpeg source tree, fetched and unpacked if need be.
fn source_dir(vendor_dir: &Path) -> PathBuf {
    if let Some(source) = env::var_os("CHIBITV_FFMPEG_SOURCE") {
        let source = PathBuf::from(source);
        assert!(
            source.join("configure").is_file(),
            "CHIBITV_FFMPEG_SOURCE ({}) is not an FFmpeg source tree",
            source.display()
        );
        return source;
    }

    let source = vendor_dir.join(format!("ffmpeg-{FFMPEG_VERSION}"));
    if source.join("configure").is_file() {
        return source;
    }

    let archive = vendor_dir.join(format!("ffmpeg-{FFMPEG_VERSION}.tar.xz"));
    if !archive.is_file() {
        let url = format!("https://ffmpeg.org/releases/ffmpeg-{FFMPEG_VERSION}.tar.xz");
        println!("cargo::warning=Downloading {url}");
        let partial = archive.with_extension("xz.part");
        run(
            Command::new("curl")
                .args([
                    "--fail",
                    "--location",
                    "--silent",
                    "--show-error",
                    "--output",
                ])
                .arg(&partial)
                .arg(&url),
            "download the FFmpeg sources",
        );
        fs::rename(&partial, &archive).expect("could not move the downloaded archive into place");
    }

    run(
        Command::new("tar")
            .arg("-C")
            .arg(vendor_dir)
            .arg("-xf")
            .arg(&archive),
        "unpack the FFmpeg sources",
    );
    assert!(
        source.join("configure").is_file(),
        "the archive did not unpack to {}",
        source.display()
    );
    source
}

/// Everything after `configure`, which is what the build is keyed on.
fn configure_arguments(config: &Config) -> Vec<String> {
    let mut arguments: Vec<String> = [
        // Nothing but what is listed below.
        "--disable-everything",
        "--disable-autodetect",
        "--disable-programs",
        "--disable-doc",
        "--disable-debug",
        "--disable-network",
        "--disable-iconv",
        "--disable-avdevice",
        "--disable-avformat",
        "--disable-swresample",
        "--disable-shared",
        "--enable-static",
        "--enable-pic",
        "--enable-avcodec",
        "--enable-avfilter",
        // Converts between pixel formats between the decoder and the encoder.
        "--enable-swscale",
        // The software decoders, and the input side of every hardware
        // acceleration that assists them.
        "--enable-decoder=mpeg2video,hevc",
        // Deinterlacing and pixel format conversion in software, and moving
        // pictures between system and device memory.
        "--enable-filter=buffer,buffersink,bwdif,format,scale,hwupload,hwdownload",
    ]
    .into_iter()
    .map(String::from)
    .collect();

    if config.is_windows() && config.target_env == "msvc" {
        arguments.push("--toolchain=msvc".into());
    }

    // FFmpeg's assembly needs nasm on x86; without it the build is slow but
    // works.
    if matches!(config.target_arch.as_str(), "x86" | "x86_64") && !command_exists("nasm") {
        println!("cargo::warning=nasm was not found, so FFmpeg is built without its x86 assembly");
        arguments.push("--disable-x86asm".into());
    }

    if config.has("ffmpeg_x264") || config.has("ffmpeg_x265") {
        arguments.push("--enable-gpl".into());
    }
    if config.has("ffmpeg_x264") {
        arguments.push("--enable-libx264".into());
        arguments.push("--enable-encoder=libx264".into());
    }
    if config.has("ffmpeg_x265") {
        arguments.push("--enable-libx265".into());
        arguments.push("--enable-encoder=libx265".into());
    }
    if config.has("ffmpeg_svt_av1") {
        arguments.push("--enable-libsvtav1".into());
        arguments.push("--enable-encoder=libsvtav1".into());
    }
    if config.has("ffmpeg_mpeg2_encoder") {
        arguments.push("--enable-encoder=mpeg2video".into());
    }

    if config.has("ffmpeg_videotoolbox") {
        arguments.push("--enable-videotoolbox".into());
        arguments.push("--enable-hwaccel=mpeg2_videotoolbox,hevc_videotoolbox".into());
        arguments.push("--enable-encoder=h264_videotoolbox,hevc_videotoolbox".into());
        arguments.push("--enable-filter=yadif_videotoolbox,scale_vt".into());
    }
    if config.has("ffmpeg_nvidia") {
        // The CUVID decoders deinterlace on the GPU themselves, which the NVDEC
        // hardware acceleration of the software decoders does not.
        arguments.push("--enable-ffnvcodec".into());
        arguments.push("--enable-cuvid".into());
        arguments.push("--enable-nvenc".into());
        arguments.push("--enable-decoder=mpeg2_cuvid,hevc_cuvid".into());
        arguments.push("--enable-encoder=h264_nvenc,hevc_nvenc,av1_nvenc".into());
    }
    if config.has("ffmpeg_vaapi") || (config.has("ffmpeg_qsv") && config.is_linux()) {
        arguments.push("--enable-vaapi".into());
    }
    if config.has("ffmpeg_vaapi") {
        arguments.push("--enable-hwaccel=mpeg2_vaapi,hevc_vaapi".into());
        arguments.push("--enable-encoder=h264_vaapi,hevc_vaapi,av1_vaapi".into());
        arguments.push("--enable-filter=deinterlace_vaapi,scale_vaapi".into());
    }
    if config.has("ffmpeg_qsv") {
        arguments.push("--enable-libvpl".into());
        arguments.push("--enable-decoder=mpeg2_qsv,hevc_qsv".into());
        arguments.push("--enable-encoder=h264_qsv,hevc_qsv,av1_qsv".into());
        arguments.push("--enable-filter=deinterlace_qsv,scale_qsv".into());
        if config.is_windows() {
            // The device libvpl sits on top of on Windows.
            arguments.push("--enable-d3d11va".into());
        }
    }
    if config.has("ffmpeg_amf") {
        arguments.push("--enable-amf".into());
        arguments.push("--enable-encoder=h264_amf,hevc_amf,av1_amf".into());
        if let Some(include) = env::var_os("CHIBITV_FFMPEG_AMF_INCLUDE") {
            arguments.push(format!(
                "--extra-cflags=-I{}",
                Path::new(&include).display()
            ));
        }
    }

    arguments
}

/// Configures and builds FFmpeg unless the same build is already there, and
/// returns the build directory.
fn build_ffmpeg(
    config: &Config,
    vendor_dir: &Path,
    source_dir: &Path,
    arguments: &[String],
) -> PathBuf {
    let mut hasher = DefaultHasher::new();
    FFMPEG_VERSION.hash(&mut hasher);
    env::var("TARGET").unwrap_or_default().hash(&mut hasher);
    source_dir.hash(&mut hasher);
    arguments.hash(&mut hasher);
    let key = format!("{:016x}", hasher.finish());

    let build_dir = vendor_dir.join("build").join(&key);
    let stamp = build_dir.join("chibitv-stamp");
    // A missing stamp makes Cargo run this script again, which is right: it
    // means the build is not there.
    println!("cargo::rerun-if-changed={}", stamp.display());

    let description = format!(
        "ffmpeg {FFMPEG_VERSION} for {}\n{}\n",
        config.target_os,
        arguments.join("\n")
    );
    if fs::read_to_string(&stamp).ok().as_deref() == Some(&description) {
        return build_dir;
    }

    // Start over rather than on top of a build that was interrupted.
    let _ = fs::remove_dir_all(&build_dir);
    fs::create_dir_all(&build_dir).expect("could not create the FFmpeg build directory");

    let mut configure = Command::new("sh");
    configure
        .arg(source_dir.join("configure"))
        .args(arguments)
        .current_dir(&build_dir);
    let output = configure
        .output()
        .unwrap_or_else(|error| panic!("could not run sh to configure FFmpeg: {error}"));
    if !output.status.success() {
        // configure says what is wrong on its own output for a bad argument
        // and in its log for a missing dependency.
        let log = fs::read_to_string(build_dir.join("ffbuild/config.log")).unwrap_or_default();
        let tail = log.lines().rev().take(40).collect::<Vec<_>>();
        panic!(
            "FFmpeg's configure failed with the arguments {arguments:?}\n{}{}\nThe end of ffbuild/config.log:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
            tail.into_iter().rev().collect::<Vec<_>>().join("\n")
        );
    }

    let jobs = env::var("NUM_JOBS").unwrap_or_else(|_| "1".into());
    run(
        Command::new("make")
            .arg(format!("-j{jobs}"))
            .current_dir(&build_dir),
        "build FFmpeg",
    );

    fs::write(&stamp, description).expect("could not write the FFmpeg build stamp");
    build_dir
}

/// Tells rustc what to link, in dependency order.
fn link(config: &Config, build_dir: &Path) {
    for library in ["avfilter", "avcodec", "swscale", "avutil"] {
        println!(
            "cargo::rustc-link-search=native={}",
            build_dir.join(format!("lib{library}")).display()
        );
        println!("cargo::rustc-link-lib=static:+verbatim=lib{library}.a");
    }

    // The libraries the enabled parts of FFmpeg call into, linked as the shared
    // libraries installed on the system, which is also where FFmpeg's configure
    // found them.
    if config.has("ffmpeg_x264") {
        probe("x264");
    }
    if config.has("ffmpeg_x265") {
        probe("x265");
    }
    if config.has("ffmpeg_svt_av1") {
        probe("SvtAv1Enc");
    }
    if config.has("ffmpeg_vaapi") || (config.has("ffmpeg_qsv") && config.is_linux()) {
        probe("libva");
        probe("libva-drm");
    }
    if config.has("ffmpeg_qsv") {
        probe("vpl");
    }
    if config.has("ffmpeg_videotoolbox") {
        for framework in [
            "VideoToolbox",
            "CoreMedia",
            "CoreVideo",
            "CoreFoundation",
            "CoreServices",
        ] {
            println!("cargo::rustc-link-lib=framework={framework}");
        }
    }

    if config.is_windows() {
        // bcrypt is where libavutil gets random numbers on Windows, and ole32
        // is what the Direct3D device behind libvpl and AMF is created
        // through. Not exercised yet.
        for library in ["bcrypt", "ole32"] {
            println!("cargo::rustc-link-lib={library}");
        }
    } else {
        println!("cargo::rustc-link-lib=m");
        println!("cargo::rustc-link-lib=pthread");
        if config.is_linux() {
            // NVENC and AMF are loaded at run time.
            println!("cargo::rustc-link-lib=dl");
        }
    }
}

fn probe(name: &str) {
    pkg_config::Config::new()
        .cargo_metadata(true)
        .probe(name)
        .unwrap_or_else(|error| {
            panic!("pkg-config could not find {name}, which FFmpeg was built against: {error}")
        });
}

fn command_exists(name: &str) -> bool {
    Command::new(name)
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn run(command: &mut Command, purpose: &str) {
    let status = command.status().unwrap_or_else(|error| {
        panic!(
            "could not run {:?} to {purpose}: {error}",
            command.get_program()
        )
    });
    assert!(
        status.success(),
        "could not {purpose}: {command:?} exited with {status}"
    );
}

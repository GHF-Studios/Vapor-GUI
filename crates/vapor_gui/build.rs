use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    let target_os = env::var("CARGO_CFG_TARGET_OS").expect("Cargo provides the target OS");
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").expect("Cargo provides the target architecture");
    let (library_name, rpath) = match target_os.as_str() {
        "linux" => ("libsteam_api.so", Some("-Wl,-rpath,$ORIGIN")),
        "macos" => ("libsteam_api.dylib", Some("-Wl,-rpath,@loader_path")),
        "windows" if target_arch == "x86" => ("steam_api.dll", None),
        "windows" => ("steam_api64.dll", None),
        _ => panic!("Vapor GUI has no Steamworks runtime packaging rule for target OS {target_os}"),
    };

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo provides OUT_DIR"));
    let profile_dir = out_dir
        .ancestors()
        .find(|candidate| candidate.join("build").is_dir() && candidate.join("deps").is_dir())
        .expect("Cargo target profile directory exists");
    let steamworks_build_dir = profile_dir.join("build");
    let source = find_steamworks_redistributable(&steamworks_build_dir, library_name)
        .unwrap_or_else(|| panic!("Steamworks redistributable {library_name} is missing from Cargo's target build outputs"));
    let destination = profile_dir.join(library_name);
    fs::copy(&source, &destination).unwrap_or_else(|error| {
        panic!("could not stage {} beside the Vapor GUI executable at {}: {error}", source.display(), destination.display())
    });
    println!("cargo:rerun-if-changed={}", source.display());

    if let Some(rpath) = rpath {
        println!("cargo:rustc-link-arg-bin=vapor-gui={rpath}");
    }
}

fn find_steamworks_redistributable(build_dir: &Path, file_name: &str) -> Option<PathBuf> {
    let mut directories = fs::read_dir(build_dir)
        .ok()?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("steamworks-sys-"))
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    directories.sort();

    directories
        .into_iter()
        .map(|directory| directory.join("out").join(file_name))
        .find(|path| path.is_file())
}

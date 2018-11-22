extern crate bindgen;
extern crate cmake;
extern crate clang_sys;

use std::env;
use std::ffi::OsStr;
use std::process::{self, Command, Stdio};
use std::path::{Path, PathBuf};
use cmake::Config;

// Use `cargo build -vv` to get detailed output on this script's progress.

fn main() {
    let llvm_info = LLVMInfo::new();

    // Build the exporter library and link it (and its dependencies)
    build_native(&llvm_info);

    // Generate ast_tags and ExportResult bindings
    if let Err(e) = generate_bindings() {
        eprintln!("{}", e);
        if let Err(e) = check_clang_version() {
            eprintln!("{}", e);
        }
        process::exit(1);
    }
}

fn check_clang_version() -> Result<(), String> {
    // Check that bindgen is using the same version of libclang and the clang
    // invocation that it pulls -isystem from. See Bindings::generate() for the
    // -isystem construction.
    if let Some(clang) = clang_sys::support::Clang::find(None, &[]) {
        let libclang_version = bindgen::clang_version().parsed.ok_or("Could not parse version of libclang in bindgen")?;
        let clang_version = clang.version.ok_or("Could not parse version of clang executable in clang-sys")?;
        let libclang_version_str = format!(
            "{}.{}",
            libclang_version.0,
            libclang_version.1,
        );
        let clang_version_str = format!(
            "{}.{}",
            clang_version.Major,
            clang_version.Minor,
        );
        if libclang_version.0 != clang_version.Major as u32
            || libclang_version.1 != clang_version.Minor as u32 {
                return Err(format!(
                    "
Bindgen requires a matching libclang and clang installation. Bindgen is using
libclang version ({libclang}) which does not match the autodetected clang
version ({clang}). If you have clang version {libclang} installed, please set
the `CLANG_PATH` environment variable to the path of this version of the clang
binary.",
                    libclang=libclang_version_str,
                    clang=clang_version_str,
                ));
            }
    }

    Ok(())
}

fn generate_bindings() -> Result<(), &'static str> {
    // The bindgen::Builder is the main entry point
    // to bindgen, and lets you build up options for
    // the resulting bindings.
    let bindings = bindgen::Builder::default()
        // Do not generate unstable Rust code that
        // requires a nightly rustc and enabling
        // unstable features.
        // .no_unstable_rust()
        // The input header we would like to generate
        // bindings for.
        .header("src/ast_tags.hpp")
        .generate_comments(true)
        .derive_default(true)
        .rustified_enum("ASTEntryTag")
        .rustified_enum("TypeTag")
        .rustified_enum("StringTypeTag")

        // Finish the builder and generate the bindings.
        .generate()
        .or(Err("Unable to generate AST bindings"))?;

    let cppbindings = bindgen::Builder::default()
        .header("src/ExportResult.hpp")
        .whitelist_type("ExportResult")
        .generate_comments(true)
        .derive_default(true)
        // Finish the builder and generate the bindings.
        .generate()
        .or(Err("Unable to generate ExportResult bindings"))?;


    // Write the bindings to the $OUT_DIR/bindings.rs file.
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_dir.join("bindings.rs"))
        .expect("Couldn't write bindings!");
    cppbindings
        .write_to_file(out_dir.join("cppbindings.rs"))
        .expect("Couldn't write cppbindings!");

    Ok(())
}

/// Call out to CMake, build the exporter library, and tell cargo where to look
/// for it.  Note that `CMAKE_BUILD_TYPE` gets implicitly determined by the
/// cmake crate according to the following:
///
///   - if `opt-level=0`                              then `CMAKE_BUILD_TYPE=Debug`
///   - if `opt-level={1,2,3}` and not `debug=false`, then `CMAKE_BUILD_TYPE=RelWithDebInfo`
fn build_native(llvm_info: &LLVMInfo) {
    // Find where the (already built) LLVM lib dir is
    let llvm_lib = &llvm_info.lib_dir;

    let dst = Config::new("src")
        // Where to find LLVM/Clang CMake files
        .define("LLVM_DIR",           &format!("{}/cmake/llvm",  llvm_lib))
        .define("Clang_DIR",          &format!("{}/cmake/clang", llvm_lib))

        // What to build
        .build_target("clangAstExporter")
        .cxxflag("-std=c++11")
        .build();

    let out_dir = dst.display();


    // Statically link against static TinyCBOR lib
    println!("cargo:rustc-link-search={}/build/tinycbor/lib", out_dir);
    println!("cargo:rustc-link-lib=static=tinycbor");

    // Statically link against 'clangAstExporter'
    println!("cargo:rustc-link-search={}/build", out_dir);
    println!("cargo:rustc-link-lib=static={}", "clangAstExporter");

    // Link against these Clang libs. The ordering here is important! Libraries
    // must be listed before their dependencies when statically linking.
    println!("cargo:rustc-link-search={}", llvm_lib);
    for lib in &[
        "clangTooling",
        "clangFrontend",
        "clangASTMatchers",
        "clangParse",
        "clangSerialization",
        "clangSema",
        "clangEdit",
        "clangAnalysis",
        "clangDriver",
        "clangFormat",
        "clangToolingCore",
        "clangAST",
        "clangRewrite",
        "clangLex",
        "clangBasic",
    ] {
        println!("cargo:rustc-link-lib={}", lib);
    }

    if llvm_info.link_statically {
        for lib in &llvm_info.static_libs {
            // IMPORTANT: We cannot specify static= here because rustc will
            // reorder those libs before the clang libs above which don't have
            // static or dylib.
            println!("cargo:rustc-link-lib={}", lib);
        }

        // Dynamically link against any system libraries required if statically
        // linking against LLVM.
        for lib in &llvm_info.system_libs {
            println!("cargo:rustc-link-lib={}", lib);
        }
    } else {
        // link against libLLVM DSO
        println!("cargo:rustc-link-lib=dylib=LLVM");
    }

    // Link against the C++ std library.
    if cfg!(target_os = "macos") {
        println!("cargo:rustc-link-lib=c++");
    } else {
        println!("cargo:rustc-link-lib=stdc++");
    }
}

/// Holds information about LLVM paths we have found
struct LLVMInfo {
    /// LLVM lib dir containing libclang* and libLLVM* libraries
    pub lib_dir: String,

    /// Must we statically link against libLLVM? Default is false.
    pub link_statically: bool,

    /// List of libs we need if linking against LLVM statically
    pub static_libs: Vec<String>,

    /// System libraries required to statically link against LLVM
    pub system_libs: Vec<String>,
}

impl LLVMInfo {
    fn new() -> Self {
        fn find_llvm_config() -> Option<String> {
            // Explicitly provided path in LLVM_CONFIG_PATH
            env::var("LLVM_CONFIG_PATH").ok()
            // Relative to LLVM_LIB_DIR
                .or(env::var("LLVM_LIB_DIR").ok().map(|d| {
                    String::from(
                        Path::new(&d)
                            .join("../bin/llvm-config")
                            .canonicalize()
                            .unwrap()
                            .to_string_lossy()
                    )
                }))
            // In PATH
                .or([
                    "llvm-config-7.0",
                    "llvm-config-6.1",
                    "llvm-config-6.0",
                    "llvm-config",

                    // Homebrew install location on MacOS
                    "/usr/local/opt/llvm/bin/llvm-config",
                ].iter().find_map(|c| {
                    if Command::new(c)
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .spawn()
                        .is_ok() {
                            Some(String::from(*c))
                        } else {
                            None
                        }
                }))
        }

        /// Invoke given `command`, if any, with the specified arguments.
        fn invoke_command<I, S>(command: Option<&String>, args: I) -> Option<String>
        where I: IntoIterator<Item = S>, S: AsRef<OsStr> {
            command.and_then(|c| {
                Command::new(c)
                    .args(args)
                    .output()
                    .ok()
                    .and_then(|output| {
                        if output.status.success() {
                            Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
                        } else {
                            None
                        }
                    })
            })
        }

        let llvm_config = find_llvm_config();
        let lib_dir = {
            let path_str = env::var("LLVM_LIB_DIR").ok().or(
                invoke_command(llvm_config.as_ref(), &["--libdir"])
            ).expect(
                "
Couldn't find LLVM lib dir. Try setting the `LLVM_LIB_DIR` environment
variable or make sure `llvm-config` is on $PATH then re-build. For example:

  $ export LLVM_LIB_DIR=/usr/local/opt/llvm/lib
"
            );
            String::from(Path::new(&path_str).canonicalize().unwrap().to_string_lossy())
        };
        let system_libs = env::var("LLVM_SYSTEM_LIBS")
            .ok()
            .or(invoke_command(llvm_config.as_ref(), &["--system-libs", "--link-static"]))
            .unwrap_or(String::new())
            .split_whitespace()
            .map(|lib| String::from(lib.trim_left_matches("-l")))
            .collect();

        let llvm_dylib = invoke_command(llvm_config.as_ref(), &["--libs", "--link-shared"]);

        // <sysroot>/lib/rustlib/<target>/lib/ contains a libLLVM DSO for the
        // rust compiler. On MacOS, this lib is named libLLVM.dylib, which will
        // always conflict with the dylib we are trying to link against. On
        // Linux we generally will not hit this issue because the prebuilt lib
        // includes the `svn` suffix. This would conflict with a source build
        // from master, however.
        //
        // We check here if the lib we want to link against will conflict with
        // the rustlib version. If so we can't dynamically link against libLLVM.
        let conflicts_with_rustlib_llvm = {
            if let Some(llvm_dylib) = llvm_dylib.as_ref() {
                let dylib_suffix = {
                    if cfg!(target_os = "macos") {
                        ".dylib"
                    } else {
                        ".so"
                    } // Windows is not supported
                };
                let mut dylib_file = String::from("lib");
                dylib_file.push_str(llvm_dylib.trim_left_matches("-l"));
                dylib_file.push_str(dylib_suffix);
                let sysroot = invoke_command(
                    env::var("RUSTC").ok().as_ref(),
                    &["--print=sysroot"],
                ).unwrap();

                // Does <sysroot>/lib/rustlib/<target>/lib/<dylib_file> exist?
                let mut libllvm_path = PathBuf::new();
                libllvm_path.push(sysroot);
                libllvm_path.push("lib/rustlib");
                libllvm_path.push(env::var("TARGET").unwrap());
                libllvm_path.push("lib");
                libllvm_path.push(dylib_file);

                libllvm_path.as_path().exists()
            } else {
                false
            }
        };

        let link_statically = cfg!(feature="llvm-static") || {
            let args = if conflicts_with_rustlib_llvm {
                vec!["--shared-mode", "--ignore-libllvm"]
            } else {
                vec!["--shared-mode"]
            };
            invoke_command(llvm_config.as_ref(), &args)
                .map_or(false, |c| c == "static")
        };

        // If we do need to statically link against libLLVM, construct the list
        // of libs.
        let static_libs = invoke_command(llvm_config.as_ref(), &[
            "--libs", "--link-static",
            "MC", "MCParser", "Support", "Option", "BitReader", "ProfileData", "BinaryFormat", "Core",
        ])
            .unwrap_or(String::new())
            .split_whitespace()
            .map(|lib| String::from(lib.trim_left_matches("-l")))
            .collect();

        Self {
            lib_dir,
            link_statically,
            static_libs,
            system_libs,
        }
    }
}

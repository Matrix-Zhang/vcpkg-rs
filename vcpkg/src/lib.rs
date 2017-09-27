//! A build dependency for Cargo libraries to find libraries in a
//! [Vcpkg](https://github.com/Microsoft/vcpkg) tree.
//!
//! Note: You must set one of `RUSTFLAGS=-Ctarget-feature=+crt-static` or
//! `VCPKGRS_DYNAMIC=1` in your environment or the vcpkg-rs helper
//! will not find any libraries. If `VCPKGRS_DYNAMIC` is set, `cargo install` will
//! generate dynamically linked binaries, in which case you will have to arrange for
//! dlls from your Vcpkg installation to be available in your path.
//!
//! The simplest possible usage for a library whose Vcpkg port name matches the
//! name of the lib and DLL that are being looked for looks like this :-
//!
//! ```rust,no_run
//! vcpkg::probe_package("libssh2").unwrap();
//! ```
//!
//! In practice the .lib and .dll often differ in name from the package itself,
//! in which case the library names must be specified, like this :-
//!
//! ```rust,no_run
//! vcpkg::Config::new()
//!     .lib_names("zlib","zlib1")
//!     .probe("zlib").unwrap();
//! ```
//!
//! If the search was successful all appropriate Cargo metadata will be printed
//! on stdout.
//!
//! The decision to choose static variants of libraries is driven by adding
//! `RUSTFLAGS=-Ctarget-feature=+crt-static` to the environment. This requires
//! a nightly compiler but is scheduled to be stable in rustc 1.19.
//!
//! A number of environment variables are available to globally configure which
//! libraries are selected.
//!
//! * `VCPKG_ROOT` - Set the directory to look in for a vcpkg installation. If
//! it is not set, vcpkg will use the user-wide installation if one has been
//! set up with `vcpkg integrate install`
//!
//! * `VCPKGRS_NO_FOO` - if set, vcpkg-rs will not attempt to find the
//! library named `foo`.
//!
//! * `VCPKGRS_DISABLE` - if set, vcpkg-rs will not attempt to find any libraries.
//!
//! * `VCPKGRS_DYNAMIC` - if set, vcpkg-rs will link to DLL builds of ports.
//!
//! There is a companion crate `vcpkg_cli` that allows testing of environment
//! and flag combinations.
//!
//! ```Batchfile
//! C:\src> vcpkg_cli probe -l static mysqlclient
//! Found library mysqlclient
//! Include paths:
//!         C:\src\diesel_build\vcpkg-dll\installed\x64-windows-static\include
//! Library paths:
//!         C:\src\diesel_build\vcpkg-dll\installed\x64-windows-static\lib
//! Cargo metadata:
//!         cargo:rustc-link-search=native=C:\src\[..]\x64-windows-static\lib
//!         cargo:rustc-link-lib=mysqlclient
//! ```

// Option::unwrap_or_default() is not available in Rust 1.10
//#![allow(or_fun_call)]

use std::ascii::AsciiExt;
use std::collections::BTreeMap;
use std::env;
use std::error;
use std::ffi::OsStr;
use std::fmt;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

#[derive(Default)]
pub struct Config {
    /// should the cargo metadata actually be emitted
    cargo_metadata: bool,

    /// should cargo:include= metadata be emitted (defaults to false)
    emit_includes: bool,

    /// .libs that must be be found for probing to be considered successful
    required_libs: Vec<String>,

    /// .libs that must be be found for probing to be considered successful
    required_dlls: Vec<String>,

    /// should DLLs be copies to OUT_DIR?
    copy_dlls: bool,
}

#[derive(Debug)]
pub struct Library {
    /// Paths for the linker to search for static or import libraries
    pub link_paths: Vec<PathBuf>,

    /// Paths to search at runtme to find DLLs
    pub dll_paths: Vec<PathBuf>,

    /// Paths to search for
    pub include_paths: Vec<PathBuf>,

    /// cargo: metadata lines
    pub cargo_metadata: Vec<String>,

    /// libraries found are static
    pub is_static: bool,

    // DLLs found
    pub found_dlls: Vec<PathBuf>,

    // static libs or import libs found
    pub found_libs: Vec<PathBuf>,
}

enum MSVCTarget {
    X86,
    X64,
}

impl fmt::Display for MSVCTarget {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            MSVCTarget::X86 => write!(f, "x86-windows"),
            MSVCTarget::X64 => write!(f, "x64-windows"),
        }
    }
}

#[derive(Debug)] // need Display?
pub enum Error {
    /// Aborted because of a `VCPKGRS_NO_*` environment variable.
    ///
    /// Contains the name of the responsible environment variable.
    DisabledByEnv(String),

    /// Aborted because a required environment variable was not set.
    RequiredEnvMissing(String),

    /// Only MSVC ABI is supported
    NotMSVC,

    /// Can't find a vcpkg tree
    VcpkgNotFound(String),

    /// Library not found in vcpkg tree
    LibNotFound(String),

    /// Could not understand vcpkg installation
    VcpkgInstallation(String),

    #[doc(hidden)] __Nonexhaustive,
}

impl error::Error for Error {
    fn description(&self) -> &str {
        match *self {
            Error::DisabledByEnv(_) => "vcpkg-rs requested to be aborted",
            Error::RequiredEnvMissing(_) => "a required env setting is missing",
            Error::NotMSVC => "vcpkg-rs only can only find libraries for MSVC ABI 64 bit builds",
            Error::VcpkgNotFound(_) => "could not find Vcpkg tree",
            Error::LibNotFound(_) => "could not find library in Vcpkg tree",
            Error::VcpkgInstallation(_) => "could not look up details of packages in vcpkg tree",
            Error::__Nonexhaustive => panic!(),
        }
    }

    fn cause(&self) -> Option<&error::Error> {
        match *self {
            // Error::Command { ref cause, .. } => Some(cause),
            _ => None,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        match *self {
            Error::DisabledByEnv(ref name) => write!(f, "Aborted because {} is set", name),
            Error::RequiredEnvMissing(ref name) => write!(f, "Aborted because {} is not set", name),
            Error::NotMSVC => write!(
                f,
                "the vcpkg-rs Vcpkg build helper can only find libraries built for the MSVC ABI."
            ),
            Error::VcpkgNotFound(ref detail) => write!(f, "Could not find Vcpkg tree: {}", detail),
            Error::LibNotFound(ref detail) => {
                write!(f, "Could not find library in Vcpkg tree {}", detail)
            }
            Error::VcpkgInstallation(ref detail) => write!(
                f,
                "Could not look up details of packages in vcpkg tree {}",
                detail
            ),
            Error::__Nonexhaustive => panic!(),
        }
    }
}

pub fn probe_package(name: &str) -> Result<Library, Error> {
    Config::new().probe(name)
}

fn find_vcpkg_root() -> Result<PathBuf, Error> {
    // prefer the setting from the environment is there is one
    if let Some(path) = env::var_os("VCPKG_ROOT") {
        return Ok(PathBuf::from(path));
    }

    // see if there is a per-user vcpkg tree that has been integrated into msbuild
    // using `vcpkg integrate install`
    let local_app_data = try!(env::var("LOCALAPPDATA").map_err(|_| {
        Error::VcpkgNotFound("Failed to read LOCALAPPDATA environment variable".to_string())
    })); // not present or can't utf8
    let vcpkg_user_targets_path = Path::new(local_app_data.as_str())
        .join("vcpkg")
        .join("vcpkg.user.targets");

    let file = try!(File::open(vcpkg_user_targets_path.clone()).map_err(|_| {
        Error::VcpkgNotFound(
            "No vcpkg.user.targets found. Set the VCPKG_ROOT environment \
             variable or run 'vcpkg integrate install'"
                .to_string(),
        )
    }));
    let file = BufReader::new(&file);

    for line in file.lines() {
        let line = try!(line.map_err(|_| {
            Error::VcpkgNotFound(format!(
                "Parsing of {} failed.",
                vcpkg_user_targets_path.to_string_lossy().to_owned()
            ))
        }));
        let mut split = line.split("Project=\"");
        split.next(); // eat anything before Project="
        if let Some(found) = split.next() {
            // " is illegal in a Windows pathname
            if let Some(found) = found.split_terminator('"').next() {
                let mut vcpkg_root = PathBuf::from(found);
                if !(vcpkg_root.pop() && vcpkg_root.pop() && vcpkg_root.pop() && vcpkg_root.pop()) {
                    return Err(Error::VcpkgNotFound(
                        format!("Could not find vcpkg root above {}", found),
                    ));
                }
                return Ok(vcpkg_root);
            }
        }
    }

    Err(Error::VcpkgNotFound(format!(
        "Project location not found parsing {}.",
        vcpkg_user_targets_path.to_string_lossy().to_owned()
    )))
}

fn validate_vcpkg_root(path: &PathBuf) -> Result<(), Error> {
    let mut vcpkg_root_path = path.clone();
    vcpkg_root_path.push(".vcpkg-root");

    if vcpkg_root_path.exists() {
        Ok(())
    } else {
        Err(Error::VcpkgNotFound(format!(
            "Could not find Vcpkg root at {}",
            vcpkg_root_path.to_string_lossy()
        )))
    }
}

fn find_vcpkg_target(msvc_target: &MSVCTarget) -> Result<VcpkgTarget, Error> {
    let vcpkg_root = try!(find_vcpkg_root());
    try!(validate_vcpkg_root(&vcpkg_root));

    let static_lib = env::var("CARGO_CFG_TARGET_FEATURE")
        .unwrap_or(String::new()) // rustc 1.10
        .contains("crt-static");

    let mut base = vcpkg_root;
    base.push("installed");
    let status_path = base.join("vcpkg");

    let static_appendage = if static_lib { "-static" } else { "" };

    let vcpkg_triple = format!("{}{}", msvc_target.to_string(), static_appendage);
    base.push(&vcpkg_triple);

    let lib_path = base.join("lib");
    let bin_path = base.join("bin");
    let include_path = base.join("include");

    Ok(VcpkgTarget {
        vcpkg_triple: vcpkg_triple,
        lib_path: lib_path,
        bin_path: bin_path,
        include_path: include_path,
        is_static: static_lib,
        status_path: status_path,
    })
}

#[derive(Clone, Debug)]
struct Port {
    // dlls if any
    dlls: Vec<String>,

    // libs (static or import)
    libs: Vec<String>,

    // ports that this port depends on
    deps: Vec<String>,
}

fn load_port_manifest(
    path: &PathBuf,
    port: &str,
    version: &str,
    vcpkg_triple: &str,
) -> Result<(Vec<String>, Vec<String>), Error> {
    let manifest_file = path.join("info")
        .join(format!("{}_{}_{}.list", port, version, vcpkg_triple));

    let mut dlls = Vec::new();
    let mut libs = Vec::new();

    let f = try!(File::open(&manifest_file).map_err(|_| {
        Error::VcpkgInstallation(format!(
            "Could not open port manifest file {}",
            manifest_file.display()
        ))
    }));

    let file = BufReader::new(&f);

    let dll_prefix = Path::new(vcpkg_triple).join("bin");
    let lib_prefix = Path::new(vcpkg_triple).join("lib");

    for line in file.lines() {
        let line = line.unwrap();

        let file_path = Path::new(&line);

        if let Ok(dll) = file_path.strip_prefix(&dll_prefix) {
            if dll.extension() == Some(OsStr::new("dll"))
                && dll.components().collect::<Vec<_>>().len() == 1
            {
                // match "mylib.dll" but not "debug/mylib.dll" or "manual_link/mylib.dll"

                dll.to_str().map(|s| dlls.push(s.to_owned()));
            }
        } else if let Ok(lib) = file_path.strip_prefix(&lib_prefix) {
            if lib.extension() == Some(OsStr::new("lib"))
                && lib.components().collect::<Vec<_>>().len() == 1
            {
                lib.to_str().map(|s| libs.push(s.to_owned()));
            }
        }
    }

    Ok((dlls, libs))
}

fn load_ports(target: &VcpkgTarget) -> Result<BTreeMap<String, Port>, Error> {
    let mut ports = BTreeMap::new();

    let status_filename = target.status_path.join("status");
    let f = try!(File::open(&status_filename).map_err(|_| {
        Error::VcpkgInstallation(format!(
            "Could not open status file at {}",
            status_filename.display()
        ))
    }));
    let file = BufReader::new(&f);

    // let name: Option<String> = None;
    // let triple: Option<String> = None;
    // let name: Option<String> = None;

    let mut current: BTreeMap<String, String> = BTreeMap::new();

    for line in file.lines() {
        let line = line.unwrap();
        let parts = line.splitn(2, ": ").clone().collect::<Vec<_>>();
        if parts.len() == 2 {
            //            println!("{}: {}", parts[0], parts[1]);
            current.insert(parts[0].trim().into(), parts[1].trim().into());
        } else if parts.len() == 1 {
            if let Some(name) = current.get("Package") {
                if let Some(arch) = current.get("Architecture") {
                    if *arch == target.vcpkg_triple {
                        // println!("-++++++-");
                        // println!("{:?}", current);
                        // println!("--------");

                        let deps = if let Some(deps) = current.get("Depends") {
                            deps.split(", ").map(|x| x.to_owned()).collect()
                        } else {
                            Vec::new()
                        };

                        // hmmm
                        let version = &current["Version"];

                        if current
                            .get("Status")
                            .unwrap_or(&String::new())
                            .ends_with(" installed")
                        {
                            let lib_info = try!(load_port_manifest(
                                &target.status_path,
                                name,
                                version,
                                &target.vcpkg_triple
                            ));
                            let port = Port {
                                dlls: lib_info.0,
                                libs: lib_info.1,
                                deps: deps,
                            };

                            ports.insert(name.clone(), port);
                        }
                    }
                }
            }

            current.clear();
        } else {
            // couldn't parse the file
            // maybe return Err()?
        }
        //    println!("courant {:?}", current);
    }

    Ok(ports)
}

/// paths and triple for the chosen target
struct VcpkgTarget {
    vcpkg_triple: String,
    lib_path: PathBuf,
    bin_path: PathBuf,
    include_path: PathBuf,

    // directory containing the status file
    status_path: PathBuf,

    is_static: bool,
}

impl Config {
    pub fn new() -> Config {
        Config {
            cargo_metadata: true,
            copy_dlls: true,
            ..Default::default()
            // emit_includes: false,
            // required_libs: Vec::new(),
        }
    }

    /// Override the name of the library to look for if it differs from the package name.
    ///
    /// This may be called more than once if multiple libs are required.
    /// All libs must be found for the probe to succeed. `.probe()` must
    /// be run with a different configuration to look for libraries under one of several names.
    /// `.libname("ssleay32")` will look for ssleay32.lib and also ssleay32.dll if
    /// dynamic linking is selected.
    pub fn lib_name(&mut self, lib_stem: &str) -> &mut Config {
        self.required_libs.push(lib_stem.to_owned());
        self.required_dlls.push(lib_stem.to_owned());
        self
    }

    /// Override the name of the library to look for if it differs from the package name.
    ///
    /// This may be called more than once if multiple libs are required.
    /// All libs must be found for the probe to succeed. `.probe()` must
    /// be run with a different configuration to look for libraries under one of several names.
    /// `.lib_names("libcurl_imp","curl")` will look for libcurl_imp.lib and also curl.dll if
    /// dynamic linking is selected.
    pub fn lib_names(&mut self, lib_stem: &str, dll_stem: &str) -> &mut Config {
        self.required_libs.push(lib_stem.to_owned());
        self.required_dlls.push(dll_stem.to_owned());
        self
    }

    /// Define whether metadata should be emitted for cargo allowing it to
    /// automatically link the binary. Defaults to `true`.
    pub fn cargo_metadata(&mut self, cargo_metadata: bool) -> &mut Config {
        self.cargo_metadata = cargo_metadata;
        self
    }

    /// Define cargo:include= metadata should be emitted. Defaults to `false`.
    pub fn emit_includes(&mut self, emit_includes: bool) -> &mut Config {
        self.emit_includes = emit_includes;
        self
    }

    /// Should DLLs be copied to OUT_DIR?
    /// Defaults to `true`.
    pub fn copy_dlls(&mut self, copy_dlls: bool) -> &mut Config {
        self.copy_dlls = copy_dlls;
        self
    }

    /// Find the library `port_name` in a Vcpkg tree.
    ///
    /// This will use all configuration previously set to select the
    /// architecture and linkage.
    pub fn probe_old(&mut self, port_name: &str) -> Result<Library, Error> {
        // determine the target type, bailing out if it is not some
        // kind of msvc
        let msvc_target = try!(msvc_target());

        // bail out if requested to not try at all
        if env::var_os("VCPKGRS_DISABLE").is_some() {
            return Err(Error::DisabledByEnv("VCPKGRS_DISABLE".to_owned()));
        }

        // bail out if requested to not try at all (old)
        if env::var_os("NO_VCPKG").is_some() {
            return Err(Error::DisabledByEnv("NO_VCPKG".to_owned()));
        }

        // bail out if requested to skip this package
        let abort_var_name = format!("VCPKGRS_NO_{}", envify(port_name));
        if env::var_os(&abort_var_name).is_some() {
            return Err(Error::DisabledByEnv(abort_var_name));
        }

        // bail out if requested to skip this package (old)
        let abort_var_name = format!("{}_NO_VCPKG", envify(port_name));
        if env::var_os(&abort_var_name).is_some() {
            return Err(Error::DisabledByEnv(abort_var_name));
        }

        // if no overrides have been selected, then the Vcpkg port name
        // is the the .lib name and the .dll name
        if self.required_libs.is_empty() {
            self.required_libs.push(port_name.to_owned());
            self.required_dlls.push(port_name.to_owned());
        }

        let vcpkg_target = try!(find_vcpkg_target(&msvc_target));

        // require explicit opt-in before using dynamically linked
        // variants, otherwise cargo install of various things will
        // stop working if Vcpkg is installed.
        if !vcpkg_target.is_static && !env::var_os("VCPKGRS_DYNAMIC").is_some() {
            return Err(Error::RequiredEnvMissing("VCPKGRS_DYNAMIC".to_owned()));
        }

        let mut lib = Library::new(vcpkg_target.is_static);

        if self.emit_includes {
            lib.cargo_metadata.push(format!(
                "cargo:include={}",
                vcpkg_target.include_path.display()
            ));
        }
        lib.include_paths.push(vcpkg_target.include_path.clone());

        lib.cargo_metadata.push(format!(
            "cargo:rustc-link-search=native={}",
            vcpkg_target
                .lib_path
                .to_str()
                .expect("failed to convert string type")
        ));
        lib.link_paths.push(vcpkg_target.lib_path.clone());
        if !vcpkg_target.is_static {
            lib.cargo_metadata.push(format!(
                "cargo:rustc-link-search=native={}",
                vcpkg_target
                    .bin_path
                    .to_str()
                    .expect("failed to convert string type")
            ));
            // this path is dropped by recent versions of cargo hence the copies to OUT_DIR below
            lib.dll_paths.push(vcpkg_target.bin_path.clone());
        }

        try!(self.emit_libs(&mut lib, &vcpkg_target));

        if self.copy_dlls {
            try!(self.do_dll_copy(&mut lib));
        }

        if self.cargo_metadata {
            for line in &lib.cargo_metadata {
                println!("{}", line);
            }
        }
        Ok(lib)
    }

    fn emit_libs(&mut self, lib: &mut Library, vcpkg_target: &VcpkgTarget) -> Result<(), Error> {
        for required_lib in &self.required_libs {
            // this could use static-nobundle= for static libraries but it is apparently
            // not necessary to make the distinction for windows-msvc.
            lib.cargo_metadata.push(format!("cargo:rustc-link-lib={}", required_lib));

            // verify that the library exists
            let mut lib_location = vcpkg_target.lib_path.clone();
            lib_location.push(required_lib.clone() + ".lib");

            if !lib_location.exists() {
                return Err(Error::LibNotFound(lib_location.display().to_string()));
            }
            lib.found_libs.push(lib_location);
        }

        if !vcpkg_target.is_static {
            for required_dll in &self.required_dlls {
                let mut dll_location = vcpkg_target.bin_path.clone();
                dll_location.push(required_dll.clone() + ".dll");

                // verify that the DLL exists
                if !dll_location.exists() {
                    return Err(Error::LibNotFound(dll_location.display().to_string()));
                }
                lib.found_dlls.push(dll_location);
            }
        }

        Ok(())
    }

    fn do_dll_copy(&mut self, lib: &mut Library) -> Result<(), Error> {
        if let Some(target_dir) = env::var_os("OUT_DIR") {
            if !lib.found_dlls.is_empty() {
                for file in &lib.found_dlls {
                    let mut dest_path = Path::new(target_dir.as_os_str()).to_path_buf();
                    dest_path.push(Path::new(file.file_name().unwrap()));
                    try!(fs::copy(file, &dest_path).map_err(|_| {
                        Error::LibNotFound(format!(
                            "Can't copy file {} to {}",
                            file.to_string_lossy(),
                            dest_path.to_string_lossy()
                        ))
                    }));
                    println!(
                        "vcpkg build helper copied {} to {}",
                        file.to_string_lossy(),
                        dest_path.to_string_lossy()
                    );
                }
                lib.cargo_metadata.push(format!(
                    "cargo:rustc-link-search=native={}",
                    env::var("OUT_DIR").unwrap()
                ));
                // work around https://github.com/rust-lang/cargo/issues/3957
                lib.cargo_metadata.push(format!(
                    "cargo:rustc-link-search={}",
                    env::var("OUT_DIR").unwrap()
                ));
            }
        } else {
            return Err(Error::LibNotFound("Unable to get OUT_DIR".to_owned()));
        }
        Ok(())
    }

    pub fn probe(&mut self, port_name: &str) -> Result<Library, Error> {
        // determine the target type, bailing out if it is not some
        // kind of msvc
        let msvc_target = try!(msvc_target());

        // bail out if requested to not try at all
        if env::var_os("VCPKGRS_DISABLE").is_some() {
            return Err(Error::DisabledByEnv("VCPKGRS_DISABLE".to_owned()));
        }

        // bail out if requested to not try at all (old)
        if env::var_os("NO_VCPKG").is_some() {
            return Err(Error::DisabledByEnv("NO_VCPKG".to_owned()));
        }

        // bail out if requested to skip this package
        let abort_var_name = format!("VCPKGRS_NO_{}", envify(port_name));
        if env::var_os(&abort_var_name).is_some() {
            return Err(Error::DisabledByEnv(abort_var_name));
        }

        // bail out if requested to skip this package (old)
        let abort_var_name = format!("{}_NO_VCPKG", envify(port_name));
        if env::var_os(&abort_var_name).is_some() {
            return Err(Error::DisabledByEnv(abort_var_name));
        }

        let vcpkg_target = try!(find_vcpkg_target(&msvc_target));

        // if no overrides have been selected, then the Vcpkg port name
        // is the the .lib name and the .dll name
        if self.required_libs.is_empty() {
            let ports = try!(load_ports(&vcpkg_target));

            if ports.get(&port_name.to_owned()).is_none() {
                return Err(Error::LibNotFound(port_name.to_owned()));
            }

            // the complete set of ports required
            let mut required_ports: BTreeMap<String, Port> = BTreeMap::new();

            // working of ports that we need to include
            //        let mut ports_to_scan: BTreeSet<String> = BTreeSet::new();
            //        ports_to_scan.insert(port_name.to_owned());
            let mut ports_to_scan = vec![port_name.to_owned()]; //: Vec<String> = BTreeSet::new();

            while !ports_to_scan.is_empty() {
                let port_name = ports_to_scan.pop().unwrap();

                if required_ports.contains_key(&port_name) {
                    continue;
                }

                if let Some(port) = ports.get(&port_name) {
                    for dep in &port.deps {
                        ports_to_scan.push(dep.clone());
                    }
                    required_ports.insert(port_name, (*port).clone());
                } else {
                    // what?
                }
            }

            // for port in ports {
            //     println!("port {:?}", port);
            // }
            // println!("=============================");
            for port in &required_ports {
                println!("required port {:?}", port);
            }

            // if no overrides have been selected, then the Vcpkg port name
            // is the the .lib name and the .dll name
            if self.required_libs.is_empty() {
                for (_, port) in &required_ports {
                    self.required_libs.extend(port.libs.iter().map(|s| {
                        Path::new(&s)
                            .file_stem()
                            .unwrap()
                            .to_string_lossy()
                            .into_owned()
                    }));
                    self.required_dlls
                        .extend(port.dlls.iter().cloned().map(|s| {
                            Path::new(&s)
                                .file_stem()
                                .unwrap()
                                .to_string_lossy()
                                .into_owned()
                        }));
                }
            }
        }
        // require explicit opt-in before using dynamically linked
        // variants, otherwise cargo install of various things will
        // stop working if Vcpkg is installed.
        if !vcpkg_target.is_static && !env::var_os("VCPKGRS_DYNAMIC").is_some() {
            return Err(Error::RequiredEnvMissing("VCPKGRS_DYNAMIC".to_owned()));
        }

        let mut lib = Library::new(vcpkg_target.is_static);

        if self.emit_includes {
            lib.cargo_metadata.push(format!(
                "cargo:include={}",
                vcpkg_target.include_path.display()
            ));
        }
        lib.include_paths.push(vcpkg_target.include_path.clone());

        lib.cargo_metadata.push(format!(
            "cargo:rustc-link-search=native={}",
            vcpkg_target
                .lib_path
                .to_str()
                .expect("failed to convert string type")
        ));
        lib.link_paths.push(vcpkg_target.lib_path.clone());
        if !vcpkg_target.is_static {
            lib.cargo_metadata.push(format!(
                "cargo:rustc-link-search=native={}",
                vcpkg_target
                    .bin_path
                    .to_str()
                    .expect("failed to convert string type")
            ));
            // this path is dropped by recent versions of cargo hence the copies to OUT_DIR below
            lib.dll_paths.push(vcpkg_target.bin_path.clone());
        }

        try!(self.emit_libs(&mut lib, &vcpkg_target));

        if self.copy_dlls {
            try!(self.do_dll_copy(&mut lib));
        }

        if self.cargo_metadata {
            for line in &lib.cargo_metadata {
                println!("{}", line);
            }
        }
        Ok(lib)
    }
}

impl Library {
    pub fn new(is_static: bool) -> Library {
        Library {
            link_paths: Vec::new(),
            dll_paths: Vec::new(),
            include_paths: Vec::new(),
            cargo_metadata: Vec::new(),
            is_static: is_static,
            found_dlls: Vec::new(),
            found_libs: Vec::new(),
        }
    }
}

fn envify(name: &str) -> String {
    name.chars()
        .map(|c| c.to_ascii_uppercase())
        .map(|c| if c == '-' { '_' } else { c })
        .collect()
}

fn msvc_target() -> Result<MSVCTarget, Error> {
    let target = env::var("TARGET").unwrap_or(String::new());
    if !target.contains("-pc-windows-msvc") {
        Err(Error::NotMSVC)
    } else if target.starts_with("x86_64-") {
        Ok(MSVCTarget::X64)
    } else {
        // everything else is x86
        Ok(MSVCTarget::X86)
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use std::env;

    #[test]
    #[allow(non_snake_case)]
    fn tests_set_environment_so_RUST_TEST_THREADS_must_be_set_to_1_in_environment() {
        assert_eq!(env::var("RUST_TEST_THREADS"), Ok("1".to_string()));
    }

    #[test]
    fn do_nothing_for_non_msvc_target() {
        env::set_var("VCPKG_ROOT", "/");
        env::set_var("TARGET", "x86_64-unknown-linux-gnu");
        assert!(match ::probe_package("foo") {
            Err(Error::NotMSVC) => true,
            _ => false,
        });

        env::set_var("TARGET", "x86_64-pc-windows-gnu");
        assert_eq!(env::var("TARGET"), Ok("x86_64-pc-windows-gnu".to_string()));
        assert!(match ::probe_package("foo") {
            Err(Error::NotMSVC) => true,
            _ => false,
        });
        env::remove_var("TARGET");
        env::remove_var("VCPKG_ROOT");
    }

    #[test]
    fn do_nothing_for_bailout_variables_set() {
        env::set_var("VCPKG_ROOT", "/");
        env::set_var("TARGET", "x86_64-pc-windows-msvc");

        for &var in &[
            "VCPKGRS_DISABLE",
            "VCPKGRS_NO_FOO",
            "FOO_NO_VCPKG",
            "NO_VCPKG",
        ] {
            env::set_var(var, "1");
            assert!(match ::probe_package("foo") {
                Err(Error::DisabledByEnv(ref v)) if v == var => true,
                _ => false,
            });
            env::remove_var(var);
        }
        env::remove_var("TARGET");
        env::remove_var("VCPKG_ROOT");
    }

    // these tests are good but are leaning on a real vcpkg installation

    // #[test]
    // fn default_build_refuses_dynamic() {
    //     env::set_var("VCPKG_ROOT", "/");
    //     env::set_var("TARGET", "x86_64-pc-windows-msvc");
    //     println!("Result is {:?}", ::probe_package("foo"));
    //     assert!(match ::probe_package("foo") {
    //                 Err(Error::RequiredEnvMissing(ref v)) if v == "VCPKGRS_DYNAMIC" => true,
    //                 //                    Err(Error::RequiredEnvMissing(_)) => true,
    //                 _ => false,
    //             });
    //     env::remove_var("TARGET");
    //     env::remove_var("VCPKG_ROOT");
    // }

    // #[test]
    // fn dynamic_build_works_but_cant_find_lib() {
    //     env::set_var("VCPKG_ROOT", "/");
    //     // this is a garbage test - "check it gets past x but fails at y"
    //     env::set_var("TARGET", "x86_64-pc-windows-msvc");
    //     env::set_var("VCPKGRS_DYNAMIC", "1");
    //     assert!(match ::probe_package("foo") {
    //                 Err(Error::LibNotFound(_)) => true,
    //                 _ => false,
    //             });
    //     env::remove_var("VCPKGRS_DYNAMIC");
    //     env::remove_var("TARGET");
    //     env::remove_var("VCPKG_ROOT");
    // }

    // #[test]
    // fn static_build_works_but_cant_find_lib() {
    //     env::set_var("VCPKG_ROOT", "/");
    //     env::set_var("TARGET", "x86_64-pc-windows-msvc");
    //     // CARGO_CFG_TARGET_FEATURE is set in response to
    //     // RUSTFLAGS=-Ctarget-feature=+crt-static. It would
    //     //  be nice to test that also.
    //     env::set_var("CARGO_CFG_TARGET_FEATURE", "crt-static");
    //     assert!(match ::probe_package("foo") {
    //                 Err(Error::LibNotFound(_)) => true,
    //                 _ => false,
    //             });
    //     env::remove_var("CARGO_CFG_TARGET_FEATURE");
    //     env::remove_var("TARGET");
    //     env::remove_var("VCPKG_ROOT");
    // }

    // #[test]
    // fn do_nothing_for_bailout_variables_set() {
    //     env::set_var("TARGET", "x86_64-pc-windows-msvc");
    //     env::set_var("TARGET", "x86_64-pc-windows-msvc");
    //     println!("{:?}", ::probe_package("foo"));
    //     assert!(match ::probe_package("foo") {
    //                 Err(Error::LibNotFound(_)) => true,
    //                 _ => false,
    //             });
    //     assert!(false);
    //     env::remove_var("TARGET");
    // }
}

//! Xtensa Rust Toolchain source and installation tools.

use crate::{
    emoji,
    error::Error,
    host_triple::HostTriple,
    toolchain::{
        download_file,
        gcc::{ESP32S2_GCC, ESP32S3_GCC, ESP32_GCC, RISCV_GCC},
        github_query,
        llvm::CLANG_NAME,
        Installable,
    },
};
use async_trait::async_trait;
use directories::BaseDirs;
use log::{debug, info, warn};
use miette::Result;
use regex::Regex;
use std::{
    env,
    fmt::Debug,
    fs::{read_dir, remove_dir_all},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

/// Xtensa Rust Toolchain repository
const DEFAULT_XTENSA_RUST_REPOSITORY: &str =
    "https://github.com/esp-rs/rust-build/releases/download";
/// Xtensa Rust Toolchain API URL
const XTENSA_RUST_LATEST_API_URL: &str =
    "https://api.github.com/repos/esp-rs/rust-build/releases/latest";
const XTENSA_RUST_API_URL: &str = "https://api.github.com/repos/esp-rs/rust-build/releases";

/// Xtensa Rust Toolchain version regex.
pub const RE_EXTENDED_SEMANTIC_VERSION: &str = r"^(?P<major>0|[1-9]\d*)\.(?P<minor>0|[1-9]\d*)\.(?P<patch>0|[1-9]\d*)\.(?P<subpatch>0|[1-9]\d*)?$";
const RE_SEMANTIC_VERSION: &str =
    r"^(?P<major>0|[1-9]\d*)\.(?P<minor>0|[1-9]\d*)\.(?P<patch>0|[1-9]\d*)?$";

#[derive(Debug, Clone, Default)]
pub struct XtensaRust {
    /// Path to the cargo home directory.
    pub cargo_home: PathBuf,
    /// Xtensa Rust toolchain file.
    pub dist_file: String,
    /// Xtensa Rust toolchain URL.
    pub dist_url: String,
    /// Host triple.
    pub host_triple: String,
    /// LLVM Toolchain path.
    pub path: PathBuf,
    /// Path to the rustup home directory.
    pub rustup_home: PathBuf,
    #[cfg(unix)]
    /// Xtensa Src Rust toolchain file.
    pub src_dist_file: String,
    #[cfg(unix)]
    /// Xtensa Src Rust toolchain URL.
    pub src_dist_url: String,
    /// Xtensa Rust toolchain destination path.
    pub toolchain_destination: PathBuf,
    /// Xtensa Rust Toolchain version.
    pub version: String,
}

impl XtensaRust {
    /// Get the latest version of Xtensa Rust toolchain.
    pub async fn get_latest_version() -> Result<String> {
        let json = github_query(XTENSA_RUST_LATEST_API_URL)?;
        let mut version = json["tag_name"].to_string();

        version.retain(|c| c != 'v' && c != '"');
        Self::parse_version(&version)?;
        debug!("{} Latest Xtensa Rust version: {}", emoji::DEBUG, version);
        Ok(version)
    }

    /// Create a new instance.
    pub fn new(toolchain_version: &str, host_triple: &HostTriple, toolchain_path: &Path) -> Self {
        let artifact_extension = get_artifact_extension(host_triple);
        let version = toolchain_version.to_string();
        let dist = format!("rust-{version}-{host_triple}");
        let dist_file = format!("{dist}.{artifact_extension}");
        let dist_url = format!("{DEFAULT_XTENSA_RUST_REPOSITORY}/v{version}/{dist_file}");
        #[cfg(unix)]
        let src_dist = format!("rust-src-{version}");
        #[cfg(unix)]
        let src_dist_file = format!("{src_dist}.{artifact_extension}");
        #[cfg(unix)]
        let src_dist_url = format!("{DEFAULT_XTENSA_RUST_REPOSITORY}/v{version}/{src_dist_file}");
        let cargo_home = get_cargo_home();
        let rustup_home = get_rustup_home();
        let toolchain_destination = toolchain_path.to_path_buf();

        Self {
            cargo_home,
            dist_file,
            dist_url,
            host_triple: host_triple.to_string(),
            path: toolchain_path.to_path_buf(),
            rustup_home,
            #[cfg(unix)]
            src_dist_file,
            #[cfg(unix)]
            src_dist_url,
            toolchain_destination,
            version,
        }
    }

    /// Parses the version of the Xtensa toolchain.
    pub fn parse_version(arg: &str) -> Result<String, Error> {
        if std::env::var_os("ESPUP_SKIP_VERSION_PARSE").is_some() {
            return Ok(arg.to_string());
        }

        debug!("{} Parsing Xtensa Rust version: {}", emoji::DEBUG, arg);
        let re_extended = Regex::new(RE_EXTENDED_SEMANTIC_VERSION).unwrap();
        let re_semver = Regex::new(RE_SEMANTIC_VERSION).unwrap();
        let json = github_query(XTENSA_RUST_API_URL)?;
        if re_semver.is_match(arg) {
            let mut extended_versions: Vec<String> = Vec::new();
            for release in json.as_array().unwrap() {
                let tag_name = release["tag_name"].to_string().replace(['\"', 'v'], "");
                if tag_name.starts_with(arg) {
                    extended_versions.push(tag_name);
                }
            }
            if extended_versions.is_empty() {
                return Err(Error::InvalidVersion(arg.to_string()));
            }
            let mut max_version = extended_versions.pop().unwrap();
            let mut max_subpatch = 0;
            for version in extended_versions {
                let subpatch: i8 = re_extended
                    .captures(&version)
                    .and_then(|cap| {
                        cap.name("subpatch")
                            .map(|subpatch| subpatch.as_str().parse().unwrap())
                    })
                    .unwrap();
                if subpatch > max_subpatch {
                    max_subpatch = subpatch;
                    max_version = version;
                }
            }
            return Ok(max_version);
        } else if re_extended.is_match(arg) {
            for release in json.as_array().unwrap() {
                let tag_name = release["tag_name"].to_string().replace(['\"', 'v'], "");
                if tag_name.starts_with(arg) {
                    return Ok(arg.to_string());
                }
            }
        }
        Err(Error::InvalidVersion(arg.to_string()))
    }

    /// Removes the Xtensa Rust toolchain.
    pub fn uninstall(toolchain_path: &Path) -> Result<(), Error> {
        info!("{} Uninstalling Xtensa Rust toolchain", emoji::WRENCH);
        let dir = read_dir(toolchain_path)?;
        for entry in dir {
            let subdir_name = entry.unwrap().path().display().to_string();
            if !subdir_name.contains(RISCV_GCC)
                && !subdir_name.contains(ESP32_GCC)
                && !subdir_name.contains(ESP32S2_GCC)
                && !subdir_name.contains(ESP32S3_GCC)
                && !subdir_name.contains(CLANG_NAME)
            {
                remove_dir_all(Path::new(&subdir_name)).unwrap();
            }
        }
        Ok(())
    }
}

#[async_trait]
impl Installable for XtensaRust {
    async fn install(&self) -> Result<Vec<String>, Error> {
        if self.toolchain_destination.exists() {
            let toolchain_name = format!(
                "+{}",
                self.toolchain_destination
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap(),
            );
            let rustc_version = Command::new("rustc")
                .args([&toolchain_name, "--version"])
                .stdout(Stdio::piped())
                .output()?;
            let output = String::from_utf8_lossy(&rustc_version.stdout);
            if rustc_version.status.success() && output.contains(&self.version) {
                warn!(
                "{} Previous installation of Xtensa Rust {} exists in: '{}'. Reusing this installation.",
                emoji::WARN,
                &self.version,
                &self.toolchain_destination.display()
            );
                return Ok(vec![]);
            } else {
                Self::uninstall(&self.toolchain_destination)?;
            }
        }

        info!(
            "{} Installing Xtensa Rust {} toolchain",
            emoji::WRENCH,
            self.version
        );

        #[cfg(unix)]
        if cfg!(unix) {
            let temp_rust_dir = tempfile::TempDir::new()
                .unwrap()
                .into_path()
                .display()
                .to_string();
            download_file(
                self.dist_url.clone(),
                "rust.tar.xz",
                &temp_rust_dir,
                true,
                false,
            )
            .await?;

            info!(
                "{} Installing 'rust' component for Xtensa Rust toolchain",
                emoji::WRENCH
            );

            if !Command::new("/usr/bin/env")
                .arg("bash")
                .arg(format!(
                    "{}/rust-nightly-{}/install.sh",
                    temp_rust_dir, &self.host_triple,
                ))
                .arg(format!(
                    "--destdir={}",
                    self.toolchain_destination.display()
                ))
                .arg("--prefix=''")
                .arg("--without=rust-docs-json-preview,rust-docs")
                .arg("--disable-ldconfig")
                .stdout(Stdio::null())
                .output()?
                .status
                .success()
            {
                Self::uninstall(&self.toolchain_destination)?;
                return Err(Error::XtensaRust);
            }

            let temp_rust_src_dir = tempfile::TempDir::new()
                .unwrap()
                .into_path()
                .display()
                .to_string();
            download_file(
                self.src_dist_url.clone(),
                "rust-src.tar.xz",
                &temp_rust_src_dir,
                true,
                false,
            )
            .await?;
            info!(
                "{} Installing 'rust-src' component for Xtensa Rust toolchain",
                emoji::WRENCH
            );
            if !Command::new("/usr/bin/env")
                .arg("bash")
                .arg(format!("{}/rust-src-nightly/install.sh", temp_rust_src_dir))
                .arg(format!(
                    "--destdir={}",
                    self.toolchain_destination.display()
                ))
                .arg("--prefix=''")
                .arg("--disable-ldconfig")
                .stdout(Stdio::null())
                .output()?
                .status
                .success()
            {
                Self::uninstall(&self.toolchain_destination)?;
                return Err(Error::XtensaRustSrc);
            }
        }
        // Some platfroms like Windows are available in single bundle rust + src, because install
        // script in dist is not available for the plaform. It's sufficient to extract the toolchain
        #[cfg(windows)]
        if cfg!(windows) {
            download_file(
                self.dist_url.clone(),
                "rust.zip",
                &self.toolchain_destination.display().to_string(),
                true,
                true,
            )
            .await?;
        }

        Ok(vec![]) // No exports
    }

    fn name(&self) -> String {
        "Xtensa Rust".to_string()
    }
}

#[derive(Debug, Clone)]
pub struct RiscVTarget {
    /// Nightly version.
    pub nightly_version: String,
}

impl RiscVTarget {
    /// Create a crate instance.
    pub fn new(nightly_version: &str) -> Self {
        RiscVTarget {
            nightly_version: nightly_version.to_string(),
        }
    }

    /// Uninstalls the RISC-V target.
    pub fn uninstall(nightly_version: &str) -> Result<(), Error> {
        info!("{} Uninstalling RISC-V target", emoji::WRENCH);

        if !Command::new("rustup")
            .args([
                "target",
                "remove",
                "--toolchain",
                nightly_version,
                "riscv32imc-unknown-none-elf",
                "riscv32imac-unknown-none-elf",
            ])
            .stdout(Stdio::null())
            .status()?
            .success()
        {
            return Err(Error::UninstallRiscvTarget);
        }
        Ok(())
    }
}

#[async_trait]
impl Installable for RiscVTarget {
    async fn install(&self) -> Result<Vec<String>, Error> {
        info!(
            "{} Installing RISC-V targets ('riscv32imc-unknown-none-elf' and 'riscv32imac-unknown-none-elf') for '{}' toolchain",
            emoji::WRENCH,
            &self.nightly_version
        );

        if !Command::new("rustup")
            .args([
                "toolchain",
                "install",
                &self.nightly_version,
                "--profile",
                "minimal",
                "--component",
                "rust-src",
                "--target",
                "riscv32imc-unknown-none-elf",
                "riscv32imac-unknown-none-elf",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?
            .success()
        {
            return Err(Error::InstallRiscvTarget(self.nightly_version.clone()));
        }

        Ok(vec![]) // No exports
    }

    fn name(&self) -> String {
        "RISC-V Rust target".to_string()
    }
}

/// Gets the artifact extension based on the host architecture.
fn get_artifact_extension(host_triple: &HostTriple) -> &str {
    match host_triple {
        HostTriple::X86_64PcWindowsMsvc | HostTriple::X86_64PcWindowsGnu => "zip",
        _ => "tar.xz",
    }
}

/// Gets the default cargo home path.
fn get_cargo_home() -> PathBuf {
    PathBuf::from(env::var("CARGO_HOME").unwrap_or_else(|_e| {
        format!(
            "{}",
            BaseDirs::new().unwrap().home_dir().join(".cargo").display()
        )
    }))
}

/// Gets the default rustup home path.
pub fn get_rustup_home() -> PathBuf {
    PathBuf::from(env::var("RUSTUP_HOME").unwrap_or_else(|_e| {
        format!(
            "{}",
            BaseDirs::new()
                .unwrap()
                .home_dir()
                .join(".rustup")
                .display()
        )
    }))
}

/// Checks if rustup is installed.
pub async fn check_rust_installation() -> Result<(), Error> {
    info!("{} Checking Rust installation", emoji::WRENCH);

    if let Err(e) = Command::new("rustup")
        .arg("--version")
        .stdout(Stdio::piped())
        .output()
    {
        if let std::io::ErrorKind::NotFound = e.kind() {
            return Err(Error::MissingRust);
        } else {
            return Err(Error::RustupDetection(e.to_string()));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::{
        logging::initialize_logger,
        toolchain::rust::{get_cargo_home, get_rustup_home, XtensaRust},
    };
    use directories::BaseDirs;

    #[test]
    fn test_xtensa_rust_parse_version() {
        initialize_logger("debug");
        assert_eq!(XtensaRust::parse_version("1.65.0.0").unwrap(), "1.65.0.0");
        assert_eq!(XtensaRust::parse_version("1.65.0.1").unwrap(), "1.65.0.1");
        assert_eq!(XtensaRust::parse_version("1.64.0.0").unwrap(), "1.64.0.0");
        assert_eq!(XtensaRust::parse_version("1.63.0").unwrap(), "1.63.0.2");
        assert_eq!(XtensaRust::parse_version("1.65.0").unwrap(), "1.65.0.1");
        assert_eq!(XtensaRust::parse_version("1.64.0").unwrap(), "1.64.0.0");
        assert!(XtensaRust::parse_version("422.0.0").is_err());
        assert!(XtensaRust::parse_version("422.0.0.0").is_err());
        assert!(XtensaRust::parse_version("a.1.1.1").is_err());
        assert!(XtensaRust::parse_version("1.1.1.1.1").is_err());
        assert!(XtensaRust::parse_version("1..1.1").is_err());
        assert!(XtensaRust::parse_version("1._.*.1").is_err());
    }

    #[test]
    fn test_get_cargo_home() {
        // No CARGO_HOME set
        std::env::remove_var("CARGO_HOME");
        assert_eq!(
            get_cargo_home(),
            BaseDirs::new().unwrap().home_dir().join(".cargo")
        );
        // CARGO_HOME set
        let temp_dir = tempfile::TempDir::new().unwrap();
        let cargo_home = temp_dir.path().to_path_buf();
        std::env::set_var("CARGO_HOME", cargo_home.to_str().unwrap());
        assert_eq!(get_cargo_home(), cargo_home);
    }

    #[test]
    fn test_get_rustup_home() {
        // No RUSTUP_HOME set
        std::env::remove_var("RUSTUP_HOME");
        assert_eq!(
            get_rustup_home(),
            BaseDirs::new().unwrap().home_dir().join(".rustup")
        );
        // RUSTUP_HOME set
        let temp_dir = tempfile::TempDir::new().unwrap();
        let rustup_home = temp_dir.path().to_path_buf();
        std::env::set_var("RUSTUP_HOME", rustup_home.to_str().unwrap());
        assert_eq!(get_rustup_home(), rustup_home);
    }
}

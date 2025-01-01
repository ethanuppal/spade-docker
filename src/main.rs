use core::str;
use std::{
    fs,
    io::{self, Read, Write},
    path::PathBuf,
    process::{Command, Stdio},
};

use argh::FromArgs;

macro_rules! string_enum {
    (
        #[string_enum(name = $name_string:literal, doc = $doc:literal)]
        enum $name:ident {
            $($variant:ident = $string:literal),* $(,)?
        }
    ) => {
        #[doc = $doc]
        enum $name {
            $($variant),*
        }

        impl std::str::FromStr for $name {
            type Err = String;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                match s {
                    $($string => Ok(Self::$variant),)*
                    _ => Err(format!("Invalid {} '{}'", $name_string, s)),
                }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                match self {
                    $(Self::$variant => $string),*
                }.fmt(f)
            }
        }
    };
}

string_enum! {
    #[string_enum(
        name = "architecture",
        doc="Must be supported by both the [Rust image](https://hub.docker.com/_/rust/) and [Zig](https://ziglang.org/download/)."
    )]
    enum Architecture {
        X86_64 = "x86_64",
        Aarch64 = "aarch64",
    }
}

string_enum! {
    #[string_enum(
        name = "Zig version",
        doc = "See the [Zig releases page](https://ziglang.org/download/) for more information."
    )]
    enum ZigVersion {
        V0_13_0 = "0.13.0"
    }
}

/// Build a new image.
#[derive(FromArgs)]
#[argh(subcommand, name = "build")]
struct BuildCommand {
    /// target architecture
    #[argh(option, short = 'a', long = "arch")]
    architecture: Architecture,

    /// version of zig to install
    #[argh(option, default = "ZigVersion::V0_13_0")]
    zig_version: ZigVersion,

    /// version of spade to package
    #[argh(option)]
    spade_rev: String,

    /// version of swim to package
    #[argh(option)]
    swim_rev: String,
}

/// Prune built images.
#[derive(FromArgs)]
#[argh(subcommand, name = "clean")]
struct CleanCommand {}

#[derive(FromArgs)]
#[argh(subcommand)]
enum Subcommand {
    Build(BuildCommand),
    Clean(CleanCommand),
}

/// Manage Spade docker images.
#[derive(FromArgs)]
struct CliArgs {
    #[argh(subcommand)]
    subcommand: Subcommand,
}

fn data_dir() -> PathBuf {
    dirs::data_local_dir().unwrap().join("spade-docker")
}

fn init_log_if_missing() -> io::Result<()> {
    fs::create_dir_all(data_dir())
}

fn log_image(hash: &str) -> io::Result<()> {
    let mut logged_images = retrieve_logged_images()?;
    if !logged_images.contains(&hash.to_string()) {
        logged_images.push(hash.to_string());
    }
    try_update_log(&logged_images)
}

fn retrieve_logged_images() -> io::Result<Vec<String>> {
    let log_file = data_dir().join("hashes.txt");
    if log_file.exists() {
        let contents =
            String::from_utf8(fs::read(log_file)?).expect("bug: non utf8 data written to log file");
        Ok(contents.split("\n").map(str::to_string).collect())
    } else {
        Ok(vec![])
    }
}

fn try_update_log(new_log: &[String]) -> io::Result<()> {
    let temp_file = data_dir().join("hashes.temp.txt");
    let log_file = data_dir().join("hashes.txt");
    fs::write(&temp_file, new_log.join("\n"))?;
    fs::rename(temp_file, log_file)
}

fn main() -> io::Result<()> {
    init_log_if_missing()?;

    match argh::from_env::<CliArgs>().subcommand {
        Subcommand::Build(build_command) => {
            let mut stderr = Command::new("docker")
                .arg("build")
                .args([
                    "--build-arg",
                    &format!("TARGET_PLATFORM={}", build_command.architecture),
                ])
                .args([
                    "--build-arg",
                    &format!("ZIG_VERSION={}", build_command.zig_version),
                ])
                .args([
                    "--build-arg",
                    &format!("SPADE_REV={}", build_command.spade_rev),
                ])
                .args([
                    "--build-arg",
                    &format!("SWIM_REV={}", build_command.swim_rev),
                ])
                .arg(".")
                .args(["--progress", "plain"])
                .stderr(Stdio::piped())
                .spawn()?
                .stderr
                .unwrap();

            let mut stderr_captured = String::new();
            let mut buffer = [0; 1024];
            while let Ok(amount) = stderr.read(&mut buffer) {
                if amount == 0 {
                    break;
                }
                stderr_captured.push_str(
                    str::from_utf8(&buffer[0..amount])
                        .expect("`docker build` produced invalid utf8 output"),
                );
                io::stderr()
                    .write_all(&buffer[0..amount])
                    .expect("failed to write to stderr");
                io::stderr().flush().expect("failed to flush stderr");
            }

            let last_line = stderr_captured
                .lines()
                .find(|line| line.contains("writing image sha256:"))
                .expect("`docker build` did not write image");
            let hash = last_line
                .split(' ')
                .map(str::trim)
                .find_map(|segment| segment.strip_prefix("sha256:"))
                .expect("no hash in `docker build` output");
            log_image(hash)
        }
        Subcommand::Clean(_clean_command) => {
            let logged_images = retrieve_logged_images()?;
            for (i, image_hash) in logged_images.iter().enumerate() {
                let image_info_output = Command::new("docker")
                    .arg("image")
                    .arg("inspect")
                    .arg(image_hash)
                    .output()?;
                let stdout = String::from_utf8(image_info_output.stdout)
                    .expect("`docker image inspect` output was unvalid utf8");
                let image_info: serde_json::Value = serde_json::from_str(&stdout)?;
                if image_info[0]["Config"]["Labels"]["tool"]
                    .as_str()
                    .map(|value| value == "spade-docker")
                    .unwrap_or_default()
                {
                    let remove_status = Command::new("docker")
                        .args(["rmi", "-f", image_hash])
                        .spawn()?
                        .wait()?;
                    if remove_status.success() {
                        try_update_log(&logged_images[i + 1..])?;
                    }
                }
            }
            Ok(())
        }
    }
}

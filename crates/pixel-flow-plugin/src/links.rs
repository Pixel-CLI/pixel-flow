// SPDX-FileCopyrightText: The Pixel contributors
// SPDX-License-Identifier: MIT

//! `pixel-flow install-links`: make `pixel <command>` find this plugin.
//!
//! Pixel core resolves `pixel commit` to a plugin named `commit`, or to an
//! executable `pixel-commit` on `PATH`. This plugin is one binary, so the
//! second form needs one name per command: a symlink `pixel-<command>` to
//! `pixel-flow`, which dispatches on the name it was started under
//! ([`crate::command_of_program`]).

use std::ffi::OsString;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::commands::COMMANDS;

const USAGE: &str = "usage: pixel-flow install-links [--dir <dir>] [--remove]\n\
  Link pixel-<command> to this binary in <dir> (default: the directory of this binary).\n\
  --remove deletes the links that point to it.\n";

/// What `install-links` does to one name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Linked,
    AlreadyLinked,
    Removed,
    Absent,
    /// Something else is there; it is left alone.
    Occupied,
}

/// Link (or unlink) every command in `dir` to `exe`. The link target is the
/// bare file name when `exe` sits in `dir`, so the directory can move.
pub fn sync(dir: &Path, exe: &Path, remove: bool) -> Result<Vec<(&'static str, Outcome)>, String> {
    let exe = exe
        .canonicalize()
        .map_err(|e| format!("cannot resolve {}: {e}", exe.display()))?;
    let dir_real = dir
        .canonicalize()
        .map_err(|e| format!("cannot resolve {}: {e}", dir.display()))?;
    let target: PathBuf = if exe.parent() == Some(dir_real.as_path()) {
        PathBuf::from(exe.file_name().ok_or("the binary has no file name")?)
    } else {
        exe.clone()
    };
    let mut out = Vec::new();
    for c in &COMMANDS {
        let link = dir.join(format!("pixel-{}", c.name));
        // `flow` answers to the binary's own name: that file is the plugin,
        // not a link to it, and `--remove` must never delete it.
        let is_binary = std::fs::symlink_metadata(&link).is_ok_and(|m| !m.is_symlink())
            && link.canonicalize().ok().as_deref() == Some(&exe);
        if is_binary {
            out.push((
                c.name,
                if remove {
                    Outcome::Absent
                } else {
                    Outcome::AlreadyLinked
                },
            ));
            continue;
        }
        let points_here = std::fs::read_link(&link)
            .ok()
            .is_some_and(|t| dir_real.join(&t).canonicalize().ok().as_deref() == Some(&exe));
        let exists = std::fs::symlink_metadata(&link).is_ok();
        let outcome = match (remove, exists, points_here) {
            (true, false, _) => Outcome::Absent,
            (true, true, true) => {
                std::fs::remove_file(&link)
                    .map_err(|e| format!("cannot remove {}: {e}", link.display()))?;
                Outcome::Removed
            }
            (false, true, true) => Outcome::AlreadyLinked,
            (_, true, false) => Outcome::Occupied,
            (false, false, _) => {
                symlink(&target, &link)
                    .map_err(|e| format!("cannot link {}: {e}", link.display()))?;
                Outcome::Linked
            }
        };
        out.push((c.name, outcome));
    }
    Ok(out)
}

pub fn run(args: &[OsString]) -> ExitCode {
    let mut dir: Option<PathBuf> = None;
    let mut remove = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.to_str() {
            Some("--remove") => remove = true,
            Some("--dir") => match it.next() {
                Some(value) => dir = Some(PathBuf::from(value)),
                None => {
                    eprint!("pixel-flow: --dir needs a value\n{USAGE}");
                    return ExitCode::from(2);
                }
            },
            Some("-h" | "--help") => {
                print!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            _ => {
                eprint!("pixel-flow: unexpected argument {arg:?}\n{USAGE}");
                return ExitCode::from(2);
            }
        }
    }
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            eprintln!("pixel-flow: cannot find this binary: {e}");
            return ExitCode::FAILURE;
        }
    };
    let dir = dir.unwrap_or_else(|| {
        exe.parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
    });
    match sync(&dir, &exe, remove) {
        Ok(rows) => {
            let mut occupied = 0;
            for (name, outcome) in &rows {
                if *outcome == Outcome::Occupied {
                    occupied += 1;
                    eprintln!(
                        "pixel-flow: {}/pixel-{name} exists and is not a link to pixel-flow; left alone",
                        dir.display()
                    );
                }
            }
            let count = |wanted: Outcome| rows.iter().filter(|(_, o)| *o == wanted).count();
            println!(
                "{}: {} linked, {} already linked, {} removed, {} left alone",
                dir.display(),
                count(Outcome::Linked),
                count(Outcome::AlreadyLinked),
                count(Outcome::Removed),
                occupied
            );
            if occupied > 0 {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(e) => {
            eprintln!("pixel-flow: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_exe(dir: &Path) -> PathBuf {
        let exe = dir.join("pixel-flow");
        std::fs::write(&exe, "#!/bin/sh\n").unwrap();
        exe
    }

    #[test]
    fn it_links_every_other_command_once_and_removes_only_its_own() {
        let dir = tempfile::tempdir().unwrap();
        let exe = fake_exe(dir.path());
        let rows = sync(dir.path(), &exe, false).unwrap();
        assert_eq!(rows.len(), COMMANDS.len());
        let state = |rows: &[(&'static str, Outcome)], n: &str| {
            rows.iter().find(|(c, _)| *c == n).unwrap().1
        };
        // `flow` is the binary itself; `ultraflow` gets its link.
        assert_eq!(state(&rows, "flow"), Outcome::AlreadyLinked);
        assert_eq!(state(&rows, "ultraflow"), Outcome::Linked);
        assert_eq!(
            std::fs::read_link(dir.path().join("pixel-ultraflow")).unwrap(),
            Path::new("pixel-flow")
        );
        // Second run: nothing to do.
        assert!(
            sync(dir.path(), &exe, false)
                .unwrap()
                .iter()
                .all(|(_, o)| *o == Outcome::AlreadyLinked)
        );
        // A foreign file is never replaced or removed.
        std::fs::remove_file(dir.path().join("pixel-ultraflow")).unwrap();
        std::fs::write(dir.path().join("pixel-ultraflow"), "mine").unwrap();
        let rows = sync(dir.path(), &exe, true).unwrap();
        assert_eq!(state(&rows, "ultraflow"), Outcome::Occupied);
        assert_eq!(state(&rows, "flow"), Outcome::Absent);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("pixel-ultraflow")).unwrap(),
            "mine"
        );
        assert!(exe.exists(), "--remove must never delete the binary");
    }

    #[test]
    fn removing_deletes_the_links_it_made() {
        let dir = tempfile::tempdir().unwrap();
        let exe = fake_exe(dir.path());
        sync(dir.path(), &exe, false).unwrap();
        let rows = sync(dir.path(), &exe, true).unwrap();
        assert!(
            rows.iter()
                .any(|(c, o)| *c == "ultraflow" && *o == Outcome::Removed)
        );
        assert!(!dir.path().join("pixel-ultraflow").exists());
        assert!(exe.exists());
    }

    #[test]
    fn a_link_elsewhere_points_at_the_absolute_binary() {
        let bin = tempfile::tempdir().unwrap();
        let links = tempfile::tempdir().unwrap();
        let exe = fake_exe(bin.path());
        sync(links.path(), &exe, false).unwrap();
        assert_eq!(
            std::fs::read_link(links.path().join("pixel-ultraflow")).unwrap(),
            exe.canonicalize().unwrap()
        );
        // The directory holds no `pixel-flow` of its own, so `flow` is linked too.
        assert!(links.path().join("pixel-flow").is_symlink());
    }
}

//! Native file/folder pickers per platform:
//! - **Windows:** tamni QNC folder picker (WinForms, veći od legacy FolderBrowserDialog)
//! - **Linux / macOS:** [rfd](https://docs.rs/rfd)

use std::path::{Path, PathBuf};

#[cfg(windows)]
use std::process::Command;

const PICK_FOLDER_PS: &str = include_str!("../scripts/pick_folder_dark.ps1");
const PICK_MARKER: &str = "QNC_PICK:";

#[cfg(windows)]
fn looks_like_windows_path(line: &str) -> bool {
    let s = line.trim();
    if s.len() < 3 {
        return false;
    }
    if s.starts_with("\\\\") {
        return s.len() > 3;
    }
    let b = s.as_bytes();
    b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\'
}

#[cfg(windows)]
fn parse_picked_stdout(stdout: &str) -> Option<String> {
    for line in stdout.lines().rev() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix(PICK_MARKER) {
            let path = rest.trim();
            if !path.is_empty() {
                return Some(path.to_string());
            }
        }
    }
    for line in stdout.lines().rev() {
        let line = line.trim();
        if looks_like_windows_path(line) {
            return Some(line.to_string());
        }
    }
    None
}

#[cfg(windows)]
fn run_ps_file(script: &str, initial: &str) -> Option<String> {
    let tmp = std::env::temp_dir().join(format!("qnc-pick-folder-{}.ps1", std::process::id()));
    if std::fs::write(&tmp, script).is_err() {
        return None;
    }
    let out = Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-STA",
            "-File",
            &tmp.to_string_lossy(),
            "-InitialPath",
            initial,
        ])
        .output();
    let _ = std::fs::remove_file(&tmp);
    let out = out.ok()?;
    if !out.status.success() {
        return None;
    }
    parse_picked_stdout(&String::from_utf8_lossy(&out.stdout))
}

#[cfg(windows)]
fn ps_escape(s: &str) -> String {
    s.replace('\'', "''")
}

#[cfg(windows)]
pub fn pick_directory(initial: &Path) -> Option<PathBuf> {
    let init = initial.to_string_lossy();
    run_ps_file(PICK_FOLDER_PS, init.as_ref()).map(PathBuf::from)
}

#[cfg(windows)]
pub fn pick_media_files(initial: &Path) -> Option<Vec<PathBuf>> {
    let init = ps_escape(&initial.to_string_lossy());
    let script = format!(
        "Add-Type -AssemblyName System.Windows.Forms; \
         [System.Windows.Forms.Application]::EnableVisualStyles(); \
         $d = New-Object System.Windows.Forms.OpenFileDialog; \
         $d.Title = 'QNC — odaberi medijske datoteke'; \
         $d.Filter = 'Medij|*.mxf;*.mov;*.mp4;*.mts;*.m2ts;*.avi;*.mkv;*.wav;*.aif;*.aiff;*.flac;*.caf;*.w64;*.mp3;*.m4a;*.aac|MXF (*.mxf)|*.mxf|Video|*.mxf;*.mov;*.mp4;*.mts;*.m2ts;*.avi;*.mkv|Audio|*.wav;*.aif;*.aiff;*.flac;*.caf;*.w64;*.bwav;*.rf64;*.mp3;*.m4a;*.aac|Sve (*.*)|*.*'; \
         $d.Multiselect = $true; \
         if ('{init}' -ne '' -and (Test-Path -LiteralPath '{init}')) {{ $d.InitialDirectory = '{init}' }}; \
         if ($d.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) {{ $d.FileNames }}"
    );
    let out = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-STA", "-Command", &script])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    Some(
        text.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(PathBuf::from)
            .collect(),
    )
}

#[cfg(not(windows))]
pub fn pick_directory(initial: &Path) -> Option<PathBuf> {
    let mut dialog = rfd::FileDialog::new()
        .set_title("QNC — odaberi mapu")
        .set_can_create_directories(true);
    if initial.is_dir() {
        dialog = dialog.set_directory(initial);
    }
    dialog.pick_folder()
}

#[cfg(not(windows))]
pub fn pick_media_files(initial: &Path) -> Option<Vec<PathBuf>> {
    let mut dialog = rfd::FileDialog::new()
        .set_title("QNC — odaberi medijske datoteke")
        .add_filter(
            "Video / MXF",
            &[
                "mxf", "mov", "mp4", "mts", "m2ts", "avi", "mkv", "m4v", "r3d",
            ],
        )
        .add_filter(
            "Audio",
            &[
                "wav", "aif", "aiff", "flac", "caf", "w64", "bwav", "rf64", "mp3", "m4a", "aac",
            ],
        )
        .add_filter("MXF", &["mxf"]);
    if initial.is_dir() {
        dialog = dialog.set_directory(initial);
    }
    dialog.pick_files()
}

//! Launch native `qnc-client` Story UI (no browser &lt;video&gt;).

use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde_json::{json, Value};

#[derive(Debug, Clone)]
pub struct NativeLaunchRequest {
    pub project_id: String,
    pub clip_id: Option<String>,
    pub seek: f64,
}

pub fn launch(req: &NativeLaunchRequest) -> Result<Value, String> {
    let pid = req.project_id.trim();
    if pid.is_empty() {
        return Err("project_id required".into());
    }
    let client = resolve_qnc_client()?;
    let seek = req.seek.max(0.0);
    let mut args: Vec<String> = Vec::new();
    let command_line;
    if let Some(clip) = req
        .clip_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        args.extend([
            "play-clip".into(),
            "--project-id".into(),
            pid.into(),
            "--clip-id".into(),
            clip.into(),
            "--gui".into(),
            "--audio".into(),
        ]);
        if seek > 0.0 {
            args.push("--seek".into());
            args.push(format!("{seek}"));
        }
        command_line = format!(
            "{} play-clip --project-id {pid} --clip-id {clip} --gui --audio",
            client.display()
        );
    } else {
        args.extend([
            "play".into(),
            "--project-id".into(),
            pid.into(),
            "--gui".into(),
            "--audio".into(),
        ]);
        if seek > 0.0 {
            args.push("--seek".into());
            args.push(format!("{seek}"));
        }
        command_line = format!("{} play --project-id {pid} --gui --audio", client.display());
    }

    let mut cmd = Command::new(&client);
    cmd.args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
        const DETACHED_PROCESS: u32 = 0x00000008;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS);
    }
    cmd.spawn()
        .map_err(|e| format!("Ne mogu pokrenuti qnc-client ({}): {e}", client.display()))?;

    Ok(json!({
        "status": "ok",
        "message": "Native Story pokrenut",
        "command": command_line,
        "client": client.display().to_string(),
        "project_id": pid,
        "clip_id": req.clip_id.clone().unwrap_or_default(),
    }))
}

fn resolve_qnc_client() -> Result<PathBuf, String> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    let bin = client_bin_name();

    // Explicit override (dev / CI).
    if let Ok(p) = std::env::var("QNC_CLIENT_BIN") {
        let p = PathBuf::from(p.trim());
        if p.is_file() {
            return Ok(p);
        }
        candidates.push(p);
    }

    // Same CARGO_TARGET_DIR as host build (run_host.ps1 / inherited shells).
    if let Ok(td) = std::env::var("CARGO_TARGET_DIR") {
        let td = PathBuf::from(td.trim());
        candidates.push(td.join("debug").join(bin));
        candidates.push(td.join("release").join(bin));
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            // Same folder as qnc-host (release layout) — sibling binary.
            candidates.push(dir.join(bin));
            // …/target/debug|release/qnc-host.exe → same target profile + other profile
            if let Some(target) = dir.parent() {
                candidates.push(target.join("debug").join(bin));
                candidates.push(target.join("release").join(bin));
                // …/qnc-host/target-check → workspace target or sibling package target
                if let Some(host_crate) = target.parent() {
                    candidates.push(host_crate.join("target").join("debug").join(bin));
                    candidates.push(host_crate.join("target").join("release").join(bin));
                    if let Some(ws) = host_crate.parent() {
                        candidates.push(ws.join("target").join("debug").join(bin));
                        candidates.push(ws.join("target").join("release").join(bin));
                        candidates.push(
                            ws.join("qnc-host")
                                .join("target-check")
                                .join("debug")
                                .join(bin),
                        );
                        candidates.push(
                            ws.join("qnc-host")
                                .join("target-check")
                                .join("release")
                                .join(bin),
                        );
                    }
                }
            }
        }
    }
    if let Ok(root) = std::env::var("CARGO_MANIFEST_DIR") {
        let root = PathBuf::from(root);
        // qnc-host/Cargo.toml → ../target/...
        if let Some(ws) = root.parent() {
            candidates.push(ws.join("target").join("debug").join(bin));
            candidates.push(ws.join("target").join("release").join(bin));
            candidates.push(
                ws.join("qnc-host")
                    .join("target-check")
                    .join("debug")
                    .join(bin),
            );
            candidates.push(
                ws.join("qnc-host")
                    .join("target-check")
                    .join("release")
                    .join(bin),
            );
            candidates.push(ws.join("qnc-client").join("target").join("debug").join(bin));
            candidates.push(
                ws.join("qnc-client")
                    .join("target")
                    .join("release")
                    .join(bin),
            );
            candidates.push(
                ws.join("qnc-client")
                    .join("target-check")
                    .join("debug")
                    .join(bin),
            );
        }
    }
    // Walk up from cwd looking for target/debug/qnc-client
    if let Ok(cwd) = std::env::current_dir() {
        let mut cur = cwd.clone();
        for _ in 0..6 {
            candidates.push(cur.join("target").join("debug").join(bin));
            candidates.push(cur.join("target").join("release").join(bin));
            candidates.push(
                cur.join("qnc-host")
                    .join("target-check")
                    .join("debug")
                    .join(bin),
            );
            candidates.push(
                cur.join("qnc-host")
                    .join("target-check")
                    .join("release")
                    .join(bin),
            );
            if !cur.pop() {
                break;
            }
        }
    }

    for path in &candidates {
        if path.is_file() {
            return Ok(path.clone());
        }
    }

    let hint = format!(
        "Nema {bin} binaryja. U QNC folderu:  $env:CARGO_TARGET_DIR=$null; cargo build -p qnc-client   zatim restartaj host (.\\run_host.ps1). Ručno: cargo run -p qnc-client -- play --project-id YOUR_ID --gui --audio"
    );
    Err(hint)
}

fn client_bin_name() -> &'static str {
    if cfg!(windows) {
        "qnc-client.exe"
    } else {
        "qnc-client"
    }
}

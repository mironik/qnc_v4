use std::fs;
use std::path::{Path, PathBuf};

pub fn sanitize_clip_id(clip_id: &str) -> String {
    let safe: String = clip_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if safe.is_empty() {
        "clip".into()
    } else {
        safe
    }
}

pub fn project_media_path(dir: &Path, clip_id: &str, src: &Path) -> PathBuf {
    let safe = sanitize_clip_id(clip_id);
    let ext = src
        .extension()
        .and_then(|e| e.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("mp4");
    dir.join(format!("{safe}.{ext}"))
}

/// Kopira medij u project/{proxy|original}/ — zadržava ekstenziju izvora.
pub fn copy_into_project_dir(dir: &Path, clip_id: &str, src: &Path) -> Result<PathBuf, String> {
    if !src.is_file() {
        return Err(format!("izvor ne postoji: {}", src.display()));
    }
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let dest = project_media_path(dir, clip_id, src);
    if src.canonicalize().map_err(|e| e.to_string())? == dest.canonicalize().unwrap_or(dest.clone())
    {
        return Ok(dest);
    }
    fs::copy(src, &dest).map_err(|e| format!("kopiranje: {e}"))?;
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn project_media_path_keeps_source_extension() {
        let dir = Path::new("/tmp/original");
        let src = Path::new("/card/Clip0001.MXF");
        assert_eq!(
            project_media_path(dir, "Clip 0001", src)
                .file_name()
                .unwrap(),
            "Clip_0001.MXF"
        );
    }

    #[test]
    fn copy_into_project_dir_creates_file() {
        let base = std::env::temp_dir().join("qnc_project_media_copy");
        let _ = fs::remove_dir_all(&base);
        let src = base.join("card").join("Take.mxf");
        fs::create_dir_all(src.parent().unwrap()).unwrap();
        fs::File::create(&src).unwrap().write_all(b"xavc").unwrap();
        let dest_dir = base.join("original");
        let dest = copy_into_project_dir(&dest_dir, "Take", &src).expect("copy");
        assert!(dest.is_file());
        let _ = fs::remove_dir_all(&base);
    }
}

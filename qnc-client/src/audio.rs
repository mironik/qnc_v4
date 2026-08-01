use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::Command;

use rodio::{Decoder, OutputStream, OutputStreamHandle, Sink};

pub struct AudioEngine {
    _stream: OutputStream,
    _handle: OutputStreamHandle,
    sink: Sink,
}

impl AudioEngine {
    pub fn new() -> Result<Self, String> {
        let (stream, handle) =
            OutputStream::try_default().map_err(|e| format!("audio output: {e}"))?;
        let sink = Sink::try_new(&handle).map_err(|e| format!("audio sink: {e}"))?;
        sink.pause();
        Ok(Self {
            _stream: stream,
            _handle: handle,
            sink,
        })
    }

    pub fn clear(&self) {
        self.sink.clear();
    }

    pub fn pause(&self) {
        self.sink.pause();
    }

    pub fn play(&self) {
        self.sink.play();
    }

    pub fn empty(&self) -> bool {
        self.sink.empty()
    }

    /// Queue host audio bytes (AAC-in-MP4). Converts to WAV first — rodio/symphonia
    /// can panic on some ISOBMFF AAC init paths.
    pub fn append_m4a(&self, bytes: Vec<u8>) -> Result<(), String> {
        let wav = m4a_bytes_to_wav(&bytes)?;
        let cursor = Cursor::new(wav);
        let source = Decoder::new(cursor).map_err(|e| format!("decode wav: {e}"))?;
        self.sink.append(source);
        Ok(())
    }
}

fn m4a_bytes_to_wav(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let dir = std::env::temp_dir().join("qnc-client");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let in_path = dir.join(format!("a_in_{stamp}.mp4"));
    let out_path = dir.join(format!("a_out_{stamp}.wav"));
    std::fs::write(&in_path, bytes).map_err(|e| e.to_string())?;
    ffmpeg_to_wav(&in_path, &out_path)?;
    let wav = std::fs::read(&out_path).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&in_path);
    let _ = std::fs::remove_file(&out_path);
    Ok(wav)
}

fn ffmpeg_to_wav(input: &Path, output: &PathBuf) -> Result<(), String> {
    let status = Command::new("ffmpeg")
        .args(["-y", "-hide_banner", "-loglevel", "error", "-i"])
        .arg(input)
        .args(["-vn", "-acodec", "pcm_s16le", "-ar", "48000", "-ac", "2"])
        .arg(output)
        .status()
        .map_err(|e| format!("ffmpeg missing/failed: {e}"))?;
    if !status.success() {
        return Err(format!("ffmpeg wav extract failed ({status})"));
    }
    Ok(())
}

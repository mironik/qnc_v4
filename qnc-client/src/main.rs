//! QNC native client (F4/F5).
//! Editorial wrap play + clip play use /api/story (native product path).

mod api;
mod audio;
mod clip_play;
mod editorial;
mod focus;
mod shortcuts;
mod timeline;
mod transport;

use clap::{Parser, Subcommand};

use api::HostClient;
use transport::TransportApp;

#[derive(Parser, Debug)]
#[command(name = "qnc-client", about = "QNC native playback client")]
struct Cli {
    #[arg(long, default_value = "http://127.0.0.1:8001", global = true)]
    server: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    Health,
    /// Editorial wrap playback (Story parts + /api/story/playback on host)
    Play {
        #[arg(long)]
        project_id: String,
        #[arg(long, default_value_t = 0.0)]
        seek: f64,
        #[arg(long)]
        gui: bool,
        #[arg(long)]
        audio: bool,
    },
    /// Play one imported proxy via /api/story (no timeline parts needed)
    PlayClip {
        #[arg(long)]
        project_id: String,
        #[arg(long)]
        clip_id: String,
        #[arg(long, default_value_t = 1.0)]
        seek: f64,
        #[arg(long)]
        gui: bool,
        #[arg(long)]
        audio: bool,
    },
}

fn main() {
    let cli = Cli::parse();
    let host = HostClient::new(&cli.server);
    match cli.command {
        Commands::Health => match host.health() {
            Ok(h) => println!("status={}", h.status),
            Err(e) => {
                eprintln!("health failed: {e}");
                std::process::exit(1);
            }
        },
        Commands::Play {
            project_id,
            seek,
            gui,
            audio,
        } => {
            let with_audio = audio || gui;
            if gui {
                if let Err(e) = TransportApp::open(host, project_id, seek, with_audio, None) {
                    eprintln!("gui play failed: {e}");
                    eprintln!("Hint: host must expose /api/story/playback.");
                    eprintln!("For source dock: play-clip --project-id … --clip-id … --gui");
                    std::process::exit(1);
                }
            } else if let Err(e) = run_editorial_oneshot(&host, &project_id, seek, with_audio) {
                eprintln!("play failed: {e}");
                std::process::exit(1);
            }
        }
        Commands::PlayClip {
            project_id,
            clip_id,
            seek,
            gui,
            audio,
        } => {
            let with_audio = audio || gui;
            if gui {
                // Same Story window as `play --gui`, with source dock preselected.
                if let Err(e) =
                    TransportApp::open(host, project_id, seek, with_audio, Some(clip_id))
                {
                    eprintln!("play-clip gui failed: {e}");
                    std::process::exit(1);
                }
            } else if let Err(e) =
                clip_play::run_oneshot(&host, &project_id, &clip_id, seek, with_audio)
            {
                eprintln!("play-clip failed: {e}");
                std::process::exit(1);
            }
        }
    }
}

fn run_editorial_oneshot(
    host: &HostClient,
    project_id: &str,
    seek: f64,
    with_audio: bool,
) -> Result<(), String> {
    let _ = host.health()?;
    let start = host.playback_start(project_id)?;
    println!(
        "session={} buses={}",
        start.session_id,
        start
            .audio_buses
            .iter()
            .map(|b| b.role.as_str())
            .collect::<Vec<_>>()
            .join(",")
    );
    if seek > 0.0 {
        host.playback_seek(&start.session_id, seek)?;
    }
    let state = host.playback_state(&start.session_id)?;
    println!(
        "layer={} virtual_sec={:.3}",
        state.active.layer, state.virtual_sec
    );
    let tmp = std::env::temp_dir().join("qnc-client");
    std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let frame_path = tmp.join(format!("frame_{}.jpg", state.session_id));
    let frame_url = host.frame_url(&state, if seek > 0.0 { seek } else { state.virtual_sec });
    host.download_file(&frame_url, &frame_path)?;
    println!("frame={}", frame_path.display());
    if with_audio {
        let audio_url = host.audio_url(&state, 4.0);
        let audio_path = tmp.join(format!("mix_{}.m4a", state.session_id));
        host.download_file(&audio_url, &audio_path)?;
        println!("audio={}", audio_path.display());
        let engine = audio::AudioEngine::new()?;
        let bytes = std::fs::read(&audio_path).map_err(|e| e.to_string())?;
        engine.append_m4a(bytes)?;
        engine.play();
        println!("rodio: playing mixed chunk (~4s)…");
        std::thread::sleep(std::time::Duration::from_millis(4200));
    }
    host.playback_stop(&state.session_id)?;
    Ok(())
}

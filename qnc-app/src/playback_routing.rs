//! App-owned playback transport — forms emit intents; open + seek live here.
//!
//! Components and workflow screens must not call `build_open_request` or player TX.

use crate::app::{Phase, QncApp, Screen};
use crate::playback_stack::PlaybackStack;
use crate::player_bridge::{self, PlayerClient};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlaybackTransportIntent {
    None,
    CueFrame(i64),
    ScrubFrame(i64),
    ScrubFrameAndPlay(i64),
    TogglePlay,
}

impl QncApp {
    /// Space / play-pause when the active editorial screen has a source loaded.
    pub fn playback_transport_available(&self) -> bool {
        if self.phase != Phase::Workspace {
            return false;
        }
        match self.screen {
            Screen::Story => self.story.playback_source_ref().is_some(),
            Screen::MediaAssist => self.media_assist.playback_source_ref().is_some(),
            Screen::Ingest => self.ingest.playback_source_ref().is_some(),
            _ => false,
        }
    }

    pub fn playback_transport_toggle(&mut self) {
        if !self.playback_transport_available() {
            return;
        }
        let err = match self.screen {
            Screen::Story => toggle_play(&mut self.playback, &self.story),
            Screen::MediaAssist => toggle_play(&mut self.playback, &self.media_assist),
            Screen::Ingest => toggle_play(&mut self.playback, &self.ingest),
            _ => return,
        };
        if let Err(err) = err {
            self.playback_transport_error(err);
        }
    }

    pub fn playback_transport_cue_frame(&mut self, frame: i64) {
        self.playback_transport_seek_frame(frame, false);
    }

    pub fn playback_transport_scrub_frame(&mut self, frame: i64) {
        self.playback_transport_seek_frame(frame, true);
    }

    pub(crate) fn playback_transport_intent(&mut self, intent: PlaybackTransportIntent) {
        match intent {
            PlaybackTransportIntent::None => {}
            PlaybackTransportIntent::CueFrame(frame) => self.playback_transport_cue_frame(frame),
            PlaybackTransportIntent::ScrubFrame(frame) => {
                self.playback_transport_scrub_frame(frame)
            }
            PlaybackTransportIntent::ScrubFrameAndPlay(frame) => {
                self.playback_transport_scrub_frame_and_play(frame)
            }
            PlaybackTransportIntent::TogglePlay => self.playback_transport_toggle(),
        }
    }

    pub(crate) fn playback_transport_intents(
        &mut self,
        intents: impl IntoIterator<Item = PlaybackTransportIntent>,
    ) {
        for intent in intents {
            self.playback_transport_intent(intent);
        }
    }

    fn playback_transport_seek_frame(&mut self, frame: i64, coalesce: bool) {
        let err = match self.screen {
            Screen::Story => seek_frame(&mut self.playback, &self.story, frame, coalesce),
            Screen::MediaAssist => {
                seek_frame(&mut self.playback, &self.media_assist, frame, coalesce)
            }
            Screen::Ingest => seek_frame(&mut self.playback, &self.ingest, frame, coalesce),
            _ => return,
        };
        if let Err(err) = err {
            self.playback_transport_error(err);
        }
    }

    fn playback_transport_error(&mut self, err: String) {
        match self.screen {
            Screen::Story => self.story.apply_player_error(err),
            Screen::MediaAssist => self.media_assist.apply_player_error(err),
            Screen::Ingest => self.ingest.apply_player_error(err),
            _ => {}
        }
    }

    fn playback_transport_scrub_frame_and_play(&mut self, frame: i64) {
        let err = match self.screen {
            Screen::Story => seek_frame_and_play(&mut self.playback, &self.story, frame),
            Screen::MediaAssist => {
                seek_frame_and_play(&mut self.playback, &self.media_assist, frame)
            }
            Screen::Ingest => seek_frame_and_play(&mut self.playback, &self.ingest, frame),
            _ => return,
        };
        if let Err(err) = err {
            self.playback_transport_error(err);
        }
    }
}

fn toggle_play(playback: &mut PlaybackStack, client: &impl PlayerClient) -> Result<(), String> {
    let request = player_bridge::build_open_request(client)?;
    playback.ensure_open(request)?;
    playback.toggle_play()
}

fn seek_frame(
    playback: &mut PlaybackStack,
    client: &impl PlayerClient,
    frame: i64,
    coalesce: bool,
) -> Result<(), String> {
    let request = player_bridge::build_open_request(client)?;
    playback.ensure_open(request)?;
    let ok = if coalesce {
        playback.scrub_frame(frame)
    } else {
        playback.cue_frame(frame)
    };
    if ok {
        Ok(())
    } else {
        Err("Timeline nije spreman — pričekaj SourceReady".into())
    }
}

fn seek_frame_and_play(
    playback: &mut PlaybackStack,
    client: &impl PlayerClient,
    frame: i64,
) -> Result<(), String> {
    let mut request = player_bridge::build_open_request(client)?;
    request.start_source_frame = crate::player_contract::FrameNumber(frame.max(0));
    playback.ensure_open(request)?;
    if !playback.scrub_frame(frame) {
        return Err("Timeline nije spreman — pričekaj SourceReady".into());
    }
    playback.toggle_play()
}

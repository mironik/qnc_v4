//! App-owned playback transport — forms emit intents; open + seek live here.
//!
//! Components and workflow screens must not call `build_open_request` or player TX.

use crate::app::{Phase, QncApp, Screen};
use crate::playback_stack::PlaybackStack;
use crate::player_bridge::{self, PlayerClient};
use crate::player_remote::BroadcastProgramOpenRequest;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PlaybackTransportIntent {
    None,
    CueFrame(i64),
    ScrubFrame(i64),
    /// Broadcast Player owns one playlist input.
    PreloadProgram(BroadcastProgramOpenRequest),
    PlayProgram(BroadcastProgramOpenRequest),
    PlayLoadedInput,
    Pause,
    TogglePlay,
}

impl QncApp {
    /// Space / play-pause transport availability for chrome buttons.
    pub fn playback_transport_available(&self) -> bool {
        if self.phase != Phase::Workspace {
            return false;
        }
        match self.screen {
            Screen::Story => self.story.playback_transport_available(),
            Screen::MediaAssist => self.media_assist.playback_source_ref().is_some(),
            Screen::Ingest => self.ingest.playback_source_ref().is_some(),
            _ => false,
        }
    }

    pub fn playback_transport_toggle(&mut self) {
        if self.phase != Phase::Workspace {
            return;
        }
        if self.screen == Screen::Story {
            let intent = self.story.playback_transport_toggle_intent(
                self.playback.playlist_input_active(),
                self.playback.playlist_input_playing(),
            );
            match intent {
                PlaybackTransportIntent::None => {}
                PlaybackTransportIntent::TogglePlay
                    if self.story.uses_playlist_input_transport() =>
                {
                    if let Err(err) = self.playback.play_loaded_input() {
                        self.playback_transport_error(err);
                    }
                }
                PlaybackTransportIntent::TogglePlay => {
                    if let Err(err) = toggle_play(&mut self.playback, &self.story) {
                        self.playback_transport_error(err);
                    }
                }
                other => self.playback_transport_intent(other),
            }
            return;
        }
        if !self.playback_transport_available() {
            return;
        }
        let err = match self.screen {
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
            PlaybackTransportIntent::PreloadProgram(request) => {
                self.playback_transport_preload_program(request)
            }
            PlaybackTransportIntent::PlayProgram(request) => {
                self.playback_transport_play_program(request)
            }
            PlaybackTransportIntent::PlayLoadedInput => self.playback_transport_play_loaded_input(),
            PlaybackTransportIntent::Pause => self.playback_transport_pause(),
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
            Screen::Story if self.story.uses_playlist_input_transport() => {
                if self.playback.playlist_input_active() {
                    seek_loaded_input(&mut self.playback, frame, coalesce)
                } else {
                    let intent = self.story.playlist_input_preload_intent();
                    self.playback_transport_intent(intent);
                    Ok(())
                }
            }
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

    fn playback_transport_pause(&mut self) {
        let err = match self.screen {
            Screen::Story | Screen::MediaAssist | Screen::Ingest => self.playback.pause(),
            _ => return,
        };
        if let Err(err) = err {
            self.playback_transport_error(err);
        }
    }

    fn playback_transport_play_program(&mut self, request: BroadcastProgramOpenRequest) {
        if let Err(err) = self.playback.play_program(request) {
            self.playback_transport_error(err);
        }
    }

    fn playback_transport_preload_program(&mut self, request: BroadcastProgramOpenRequest) {
        if let Err(err) = self.playback.preload_program(request) {
            self.playback_transport_error(err);
        }
    }

    fn playback_transport_play_loaded_input(&mut self) {
        if let Err(err) = self.playback.play_loaded_input() {
            self.playback_transport_error(err);
        }
    }
}

fn toggle_play(playback: &mut PlaybackStack, client: &impl PlayerClient) -> Result<(), String> {
    let request = player_bridge::build_open_request(client)?;
    playback.toggle_source_play(request)
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

fn seek_loaded_input(
    playback: &mut PlaybackStack,
    frame: i64,
    coalesce: bool,
) -> Result<(), String> {
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

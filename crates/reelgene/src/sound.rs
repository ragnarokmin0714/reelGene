//! Two sounds: the batch is done, and something failed. Never one per
//! episode — an 80-episode series must not chime 80 times.
//!
//! The sounds are synthesised here (no audio files, nothing with unclear
//! rights) and played without linking an audio library: on Linux a library
//! that needs ALSA would stop reelgene from starting at all on a machine
//! without it, the CLI included. Windows has `PlaySound`; Linux gets the first
//! of `pw-play`, `paplay`, `aplay` found, or silence.

use crate::settings::Settings;
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cue {
    Done,
    Failed,
}

pub struct Player {
    enabled: bool,
    volume: f32,
    quiet: Option<[u8; 2]>,
    last: Option<Instant>,
    /// Where the Linux players read their file from.
    #[cfg_attr(windows, allow(dead_code))]
    dir: PathBuf,
    /// PlaySound reads from memory while it plays: the buffer must outlive it.
    #[cfg(windows)]
    playing: Option<Vec<u8>>,
}

/// Two sounds within this long are one too many.
const GAP: Duration = Duration::from_secs(3);
const RATE: u32 = 22_050;

impl Player {
    pub fn new(settings: &Settings, dir: PathBuf) -> Self {
        let mut p = Self {
            enabled: false,
            volume: 0.0,
            quiet: None,
            last: None,
            dir,
            #[cfg(windows)]
            playing: None,
        };
        p.configure(settings);
        p
    }

    pub fn configure(&mut self, settings: &Settings) {
        self.enabled = settings.sound;
        self.volume = settings.volume;
        self.quiet = settings.quiet_hours;
    }

    pub fn play(&mut self, cue: Cue) {
        if !self.enabled || self.volume <= 0.0 {
            return;
        }
        if let Some(q) = self.quiet
            && local_hour().is_some_and(|h| in_quiet_hours(h, q))
        {
            return;
        }
        if self.last.is_some_and(|t| t.elapsed() < GAP) {
            return;
        }
        self.last = Some(Instant::now());
        self.output(cue, synth(cue, self.volume));
    }

    #[cfg(windows)]
    fn output(&mut self, _cue: Cue, wav: Vec<u8>) {
        use windows_sys::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};
        self.stop();
        let wav = self.playing.insert(wav);
        // SAFETY: SND_MEMORY makes the first argument a pointer to a complete
        // WAV image, which `self.playing` keeps alive until the next `stop`
        // (also run on drop). SND_ASYNC returns at once.
        #[allow(unsafe_code)]
        unsafe {
            PlaySoundW(
                wav.as_ptr().cast(),
                std::ptr::null_mut(),
                SND_MEMORY | SND_ASYNC | SND_NODEFAULT,
            );
        }
    }

    #[cfg(windows)]
    fn stop(&mut self) {
        use windows_sys::Win32::Media::Audio::PlaySoundW;
        if self.playing.is_some() {
            // SAFETY: a null sound stops whatever this process is playing, after
            // which the old buffer is no longer read.
            #[allow(unsafe_code)]
            unsafe {
                PlaySoundW(std::ptr::null(), std::ptr::null_mut(), 0);
            }
            self.playing = None;
        }
    }

    #[cfg(not(windows))]
    fn output(&mut self, cue: Cue, wav: Vec<u8>) {
        use std::process::Command;
        let players: [(&str, &[&str]); 3] = [("pw-play", &[]), ("paplay", &[]), ("aplay", &["-q"])];
        let Some((path, args)) = players
            .iter()
            .find_map(|(name, args)| crate::platform::which(name).map(|p| (p, *args)))
        else {
            return;
        };
        let file = self.dir.join(match cue {
            Cue::Done => "done.wav",
            Cue::Failed => "failed.wav",
        });
        if std::fs::create_dir_all(&self.dir).is_err() || std::fs::write(&file, wav).is_err() {
            return;
        }
        let mut cmd = Command::new(path);
        cmd.args(args).arg(&file);
        crate::platform::spawn_detached(cmd);
    }
}

#[cfg(windows)]
impl Drop for Player {
    fn drop(&mut self) {
        self.stop();
    }
}

fn local_hour() -> Option<u8> {
    u8::try_from(jiff::Zoned::now().hour()).ok()
}

/// `[23, 8]` is 23:00 to 08:00, across midnight; equal hours mean never.
pub fn in_quiet_hours(hour: u8, [from, to]: [u8; 2]) -> bool {
    match from.cmp(&to) {
        std::cmp::Ordering::Equal => false,
        std::cmp::Ordering::Less => (from..to).contains(&hour),
        std::cmp::Ordering::Greater => hour >= from || hour < to,
    }
}

/// A short two-note chime as a 16-bit mono WAV: rising for done, falling for
/// failed.
pub fn synth(cue: Cue, volume: f32) -> Vec<u8> {
    let notes: &[(f32, f32)] = match cue {
        Cue::Done => &[(659.25, 0.11), (987.77, 0.24)],
        Cue::Failed => &[(392.0, 0.16), (261.63, 0.28)],
    };
    let amplitude = volume.clamp(0.0, 1.0) * 0.45 * f32::from(i16::MAX);
    let mut samples: Vec<i16> = Vec::new();
    for &(freq, secs) in notes {
        let n = (secs * RATE as f32) as usize;
        for i in 0..n {
            let t = i as f32 / RATE as f32;
            let attack = (t / 0.006).min(1.0);
            let decay = (-5.0 * t / secs).exp();
            let phase = std::f32::consts::TAU * freq * t;
            let tone = phase.sin() + 0.25 * (2.0 * phase).sin();
            samples.push((tone / 1.25 * attack * decay * amplitude) as i16);
        }
    }
    wav(&samples)
}

fn wav(samples: &[i16]) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&RATE.to_le_bytes());
    out.extend_from_slice(&(RATE * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_chimes_are_valid_short_wavs() {
        let w = synth(Cue::Done, 0.8);
        assert_eq!(&w[..4], b"RIFF");
        assert_eq!(&w[8..16], b"WAVEfmt ");
        let data_len = u32::from_le_bytes(w[40..44].try_into().unwrap()) as usize;
        assert_eq!(w.len(), 44 + data_len);
        let secs = data_len as f32 / 2.0 / RATE as f32;
        assert!((0.3..0.5).contains(&secs), "{secs}s");
        assert!(w[44..].iter().any(|b| *b != 0));
        assert!(
            synth(Cue::Failed, 0.0)[44..].iter().all(|b| *b == 0),
            "volume 0 is silence"
        );
    }

    #[test]
    fn quiet_hours_wrap_past_midnight() {
        assert!(in_quiet_hours(23, [23, 8]));
        assert!(in_quiet_hours(3, [23, 8]));
        assert!(!in_quiet_hours(8, [23, 8]));
        assert!(!in_quiet_hours(12, [23, 8]));
        assert!(in_quiet_hours(13, [13, 14]));
        assert!(!in_quiet_hours(14, [13, 14]));
        assert!(!in_quiet_hours(5, [5, 5]), "equal hours: off");
    }

    #[test]
    fn a_second_sound_right_after_the_first_is_dropped() {
        let settings = Settings {
            sound: true,
            volume: 0.5,
            quiet_hours: None,
            ..Settings::default()
        };
        let dir = std::env::temp_dir().join(format!("reelgene-sound-{}", std::process::id()));
        let mut p = Player::new(&settings, dir.clone());
        p.play(Cue::Done);
        let first = p.last;
        p.play(Cue::Failed);
        assert_eq!(p.last, first);
        std::fs::remove_dir_all(&dir).ok();
    }
}

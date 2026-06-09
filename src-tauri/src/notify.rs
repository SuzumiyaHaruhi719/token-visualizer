//! Session-end NOTIFICATIONS (taskbar flash + Windows toast + chime).
//!
//! The DETECTION of ended sessions is pure and lives in
//! [`cmserver::session_end`] (shared with browser mode). This module is the
//! desktop SIDE-EFFECT layer: given an [`EndedSession`], it fires three
//! independent, best-effort effects:
//!
//! 1. **Taskbar flash** — `request_user_attention` on the dashboard window so the
//!    taskbar button flashes even while the window is hidden.
//! 2. **Windows toast** — via `tauri-plugin-notification`.
//! 3. **Chime** — the bundled `assets/session-end.wav`, played async through the
//!    Win32 `PlaySoundW` API (no audio-graph deps).
//!
//! These must run on the main thread (Windows window ops are not thread-safe off
//! the UI thread); the caller ([`crate::state_poll::DesktopHooks`]) marshals via
//! `AppHandle::run_on_main_thread`.

use cmserver::session_end::{toast_body, EndedSession};
use tauri::{AppHandle, Manager};
use tauri_plugin_notification::NotificationExt;

use crate::windows::DASHBOARD_LABEL;

/// Fire the side effects for one ended session. Best-effort: every step is
/// independent and a failure in one never blocks the others (nor panics).
///
/// The taskbar flash + toast are gated on `notifications_enabled`; the chime is
/// gated independently on `sound_enabled` and played at `volume` (`0.0..=1.0`).
///
/// MUST be called on the main thread (window ops). The caller marshals via
/// `AppHandle::run_on_main_thread`.
pub fn notify_session_ended(
    app: &AppHandle,
    ended: &EndedSession,
    notifications_enabled: bool,
    sound_enabled: bool,
    volume: f32,
) {
    if notifications_enabled {
        flash_taskbar(app);
        send_toast(app, ended);
    }
    if sound_enabled {
        play_chime(app, volume);
    }
}

/// Flash the dashboard window's taskbar button to draw attention. If the window
/// does not exist yet, there is nothing to flash — skip silently.
fn flash_taskbar(app: &AppHandle) {
    let Some(win) = app.get_webview_window(DASHBOARD_LABEL) else {
        return;
    };
    let _ = win.request_user_attention(Some(tauri::UserAttentionType::Critical));
}

/// Send a Windows toast describing the ended session. Body format:
/// `"<project> · <model> · <N> tokens"`, falling back to a generic label when
/// the project/model are unknown (they are, once the session has dropped out).
fn send_toast(app: &AppHandle, ended: &EndedSession) {
    let body = toast_body(ended);
    if let Err(e) = app
        .notification()
        .builder()
        .title("Claude session finished")
        .body(&body)
        .show()
    {
        eprintln!("[notify] toast failed: {e}");
    }
}

/// Volume at/above which we play the file directly (no PCM scaling). Scaling a
/// near-unity factor is wasted work and risks rounding noise.
#[cfg(windows)]
const FULL_VOLUME_THRESHOLD: f32 = 0.99;

/// Resolve the bundled chime path and play it at `volume` (`0.0..=1.0`). At full
/// volume the file plays directly via `SND_FILENAME`; below that the PCM samples
/// are scaled and the result played from memory. In a bundled app the WAV lives
/// in the Tauri resource dir; in a `cargo build` dev run it falls back to
/// `CARGO_MANIFEST_DIR/assets`. A missing file is logged and skipped — never a
/// panic.
fn play_chime(app: &AppHandle, volume: f32) {
    let Some(path) = chime_path(app) else {
        eprintln!("[notify] chime wav not found; skipping sound");
        return;
    };
    #[cfg(windows)]
    {
        let volume = volume.clamp(0.0, 1.0);
        if volume >= FULL_VOLUME_THRESHOLD {
            play_wav_async(&path);
        } else {
            play_wav_at_volume(&path, volume);
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (path, volume); // non-Windows builds (tests/CI) are silent by design.
    }
}

/// Locate `assets/session-end.wav`: prefer the Tauri resource dir (production
/// bundle), fall back to the crate's source `assets/` for non-bundled dev runs.
fn chime_path(app: &AppHandle) -> Option<std::path::PathBuf> {
    const REL: &str = "assets/session-end.wav";
    if let Ok(res) = app.path().resource_dir() {
        let p = res.join(REL);
        if p.is_file() {
            return Some(p);
        }
    }
    let dev = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(REL);
    if dev.is_file() {
        return Some(dev);
    }
    None
}

/// Play a WAV file via Win32 `PlaySoundW` with `SND_FILENAME | SND_ASYNC`:
/// returns immediately, the OS handles playback. Errors are non-fatal.
#[cfg(windows)]
fn play_wav_async(path: &std::path::Path) {
    use windows::core::PCWSTR;
    use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_FILENAME, SND_NODEFAULT};

    // Build a wide, null-terminated path string that must outlive the call.
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: `wide` is a valid null-terminated UTF-16 buffer alive for the
    // duration of the call; SND_ASYNC copies what it needs and returns at once.
    let ok = unsafe {
        PlaySoundW(
            PCWSTR(wide.as_ptr()),
            None,
            SND_FILENAME | SND_ASYNC | SND_NODEFAULT,
        )
    };
    if !ok.as_bool() {
        eprintln!("[notify] PlaySoundW returned false for {}", path.display());
    }
}

/// Play a WAV at a reduced `volume` (`0.0..=1.0`) without any audio-graph deps.
/// Loads the file, scales the 16-bit PCM `data` chunk by `volume`, and plays the
/// in-memory buffer via `PlaySoundW(SND_MEMORY | SND_SYNC)` on a dedicated
/// thread so the buffer outlives playback. If the wav is not the expected PCM
/// layout, falls back to playing the file unscaled.
#[cfg(windows)]
fn play_wav_at_volume(path: &std::path::Path, volume: f32) {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("[notify] read chime failed: {e}");
            return;
        }
    };
    let Some(scaled) = scale_wav_pcm16(&bytes, volume) else {
        // Not the expected PCM layout: play the original file unscaled.
        play_wav_async(path);
        return;
    };

    // SND_SYNC blocks the calling thread for the clip's duration, so play on a
    // dedicated thread. The owned `scaled` buffer is moved in and stays alive
    // for the whole SND_MEMORY call, then is dropped when the thread exits.
    std::thread::spawn(move || {
        use windows::core::PCWSTR;
        use windows::Win32::Media::Audio::{PlaySoundW, SND_MEMORY, SND_NODEFAULT, SND_SYNC};
        // SAFETY: with SND_MEMORY the first arg is a pointer to a WAV image that
        // must remain valid for the (synchronous) call; `scaled` does exactly
        // that and is not freed until this closure returns.
        let ok = unsafe {
            PlaySoundW(
                PCWSTR(scaled.as_ptr() as *const u16),
                None,
                SND_MEMORY | SND_SYNC | SND_NODEFAULT,
            )
        };
        if !ok.as_bool() {
            eprintln!("[notify] PlaySoundW(SND_MEMORY) returned false");
        }
    });
}

/// Offsets/sizes for the canonical 44-byte PCM WAV header.
const WAV_HEADER_LEN: usize = 44;

/// Scale the 16-bit PCM sample data of a canonical WAV buffer by `volume`
/// (`0.0..=1.0`), returning a NEW buffer (header preserved). Returns `None` when
/// the input is not a recognizable 16-bit PCM WAV (so the caller can fall back).
///
/// Pure + unit-tested: locates the `data` chunk after the standard 44-byte
/// header, then multiplies each little-endian `i16` sample by `volume`, clamping
/// to the `i16` range.
fn scale_wav_pcm16(bytes: &[u8], volume: f32) -> Option<Vec<u8>> {
    if bytes.len() < WAV_HEADER_LEN {
        return None;
    }
    if &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return None;
    }
    // Canonical layout: a `data` subchunk id at offset 36, its u32 length at 40,
    // samples from 44 on. Bail out if the layout differs (caller falls back).
    if &bytes[36..40] != b"data" {
        return None;
    }
    let declared = u32::from_le_bytes([bytes[40], bytes[41], bytes[42], bytes[43]]) as usize;
    let available = bytes.len() - WAV_HEADER_LEN;
    // Use whatever sample bytes are actually present (tolerate a short/long size
    // field), trimmed to an even count so every i16 is whole.
    let mut data_len = declared.min(available);
    data_len -= data_len % 2;

    let volume = volume.clamp(0.0, 1.0);
    let mut out = Vec::with_capacity(bytes.len());
    out.extend_from_slice(&bytes[0..WAV_HEADER_LEN]);

    let samples = &bytes[WAV_HEADER_LEN..WAV_HEADER_LEN + data_len];
    for pair in samples.chunks_exact(2) {
        let sample = i16::from_le_bytes([pair[0], pair[1]]);
        let scaled = (sample as f32 * volume).round();
        let scaled = scaled.clamp(i16::MIN as f32, i16::MAX as f32) as i16;
        out.extend_from_slice(&scaled.to_le_bytes());
    }
    // Preserve any trailing bytes after the scaled sample region unchanged.
    out.extend_from_slice(&bytes[WAV_HEADER_LEN + data_len..]);
    Some(out)
}

#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal canonical 16-bit PCM WAV around the given samples.
    fn wav_with_samples(samples: &[i16]) -> Vec<u8> {
        let data: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let mut buf = Vec::with_capacity(WAV_HEADER_LEN + data.len());
        buf.extend_from_slice(b"RIFF");
        buf.extend_from_slice(&((36 + data.len()) as u32).to_le_bytes());
        buf.extend_from_slice(b"WAVE");
        buf.extend_from_slice(b"fmt ");
        buf.extend_from_slice(&16u32.to_le_bytes()); // subchunk1 size
        buf.extend_from_slice(&1u16.to_le_bytes()); // PCM
        buf.extend_from_slice(&1u16.to_le_bytes()); // mono
        buf.extend_from_slice(&8000u32.to_le_bytes()); // sample rate
        buf.extend_from_slice(&16000u32.to_le_bytes()); // byte rate
        buf.extend_from_slice(&2u16.to_le_bytes()); // block align
        buf.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
        buf.extend_from_slice(b"data");
        buf.extend_from_slice(&(data.len() as u32).to_le_bytes());
        buf.extend_from_slice(&data);
        buf
    }

    fn read_samples(wav: &[u8]) -> Vec<i16> {
        wav[WAV_HEADER_LEN..]
            .chunks_exact(2)
            .map(|p| i16::from_le_bytes([p[0], p[1]]))
            .collect()
    }

    #[test]
    fn scale_wav_halves_samples() {
        let wav = wav_with_samples(&[1000, -2000, 4, -5]);
        let out = scale_wav_pcm16(&wav, 0.5).unwrap();
        // Header is preserved verbatim.
        assert_eq!(&out[0..WAV_HEADER_LEN], &wav[0..WAV_HEADER_LEN]);
        // Samples are halved (round-to-nearest): -5 * 0.5 = -2.5 -> -2 (away
        // from zero on .5 via f32::round).
        assert_eq!(read_samples(&out), vec![500, -1000, 2, -3]);
    }

    #[test]
    fn scale_wav_zero_is_silence() {
        let wav = wav_with_samples(&[i16::MAX, i16::MIN, 1234, -4321]);
        let out = scale_wav_pcm16(&wav, 0.0).unwrap();
        assert_eq!(read_samples(&out), vec![0, 0, 0, 0]);
    }

    #[test]
    fn scale_wav_clamps_and_passes_full_volume() {
        let wav = wav_with_samples(&[i16::MAX, i16::MIN]);
        // Volume is clamped to [0,1]; 1.5 acts as 1.0 -> samples unchanged, no
        // overflow past the i16 range.
        let out = scale_wav_pcm16(&wav, 1.5).unwrap();
        assert_eq!(read_samples(&out), vec![i16::MAX, i16::MIN]);
    }

    #[test]
    fn scale_wav_rejects_non_pcm_layout() {
        // Too short / not RIFF: caller falls back to unscaled playback.
        assert!(scale_wav_pcm16(b"not a wav", 0.5).is_none());
        assert!(scale_wav_pcm16(&[0u8; 10], 0.5).is_none());
    }
}

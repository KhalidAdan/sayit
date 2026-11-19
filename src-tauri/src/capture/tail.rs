//! The release tail: never clip the last word.
//!
//! People let go of the key while the last word is still leaving their
//! mouth. End the take at key-up and that word gets cut. hex keeps 200ms
//! after every release, growing to 600ms while speech carries on
//! (docs/hex-teardown.md §2.3). But a fixed tail is a fixed tax on EVERY
//! take's gap: whisper only starts once the take ends, so each ms of audio
//! waited for is a ms of gap. So sayit's tail is earned, take by take:
//!
//! - **Quiet release** (you had already finished): no tail. The take ends
//!   the moment the stop arrives, exactly as before this file existed.
//! - **Speech ran into the release**: keep listening until 160ms of
//!   quiet, or 600ms after key-up, whichever comes first.
//!
//! Both are measured from the OS key-up stamp, not from when the stop
//! command arrives, so time already spent in dispatch counts toward the
//! tail.
//!
//! Telling speech from quiet: the audio is cut into 20ms frames, and each
//! frame's loudness is measured in dBFS. That's decibels relative to
//! digital full scale: 0 is the loudest a sample can be, and a quiet room
//! sits around -60. The take's own noise floor is its quietest tenth of
//! frames: the room between your words, and the beat before you started.
//! A frame is speech when it is at least 12dB over that floor. The floor
//! is relative, not a fixed number, because a laptop mic in a quiet study
//! and a headset next to a fan can sit 30dB apart.
//!
//! This file is pure arithmetic over samples. There's no mic, no clock
//! and no threads here, so every case is a unit test. capture.rs owns the
//! microphone and the waiting; this file only answers "close now, or keep
//! listening?"

/// The analysis frame. 20ms is the classic speech-processing step. It is
/// long enough to span two pitch cycles of even a low voice (~100Hz),
/// and short enough to see a word end. hex's release tail uses the same
/// frames (src/dictation.rs, VoiceTrack).
const FRAME_MS: u64 = 20;

/// How far above the take's noise floor a frame must be to count as
/// speech. 12dB is about 4x the room's amplitude: voiced speech clears it
/// easily, and steady room noise never does. hex's speech-frame test uses
/// the same margin (src/transcription/pause_segmenter.rs,
/// SPEECH_MARGIN_DB).
const SPEECH_MARGIN_DB: f32 = 12.0;

/// No frame quieter than this is speech, however quiet the room. Without
/// it, a near-silent mic's fizz would be "12dB over the floor". The value
/// is hex's MINIMUM_SPEECH_DB (same file). Missing very quiet speech here
/// only means no tail, which is today's behavior. It never means a longer
/// gap.
const MIN_SPEECH_DBFS: f32 = -55.0;

/// Speech frames that make "still talking": 5 frames is 100ms. A key's
/// clack is a few ms of noise, one frame or two, so it can't qualify.
/// hex: TAIL_SPEECH_FRAMES = 5 (src/dictation.rs).
const SUSTAINED_FRAMES: usize = 5;

/// Quiet frames a run of speech may skip over: 2 frames is 40ms, as
/// between the syllables of one word. It is also how close to key-up the
/// last speech must be for the release to count as "mid-word".
/// hex: TAIL_SPEECH_GAP_FRAMES = 2.
const SYLLABLE_GAP_FRAMES: usize = 2;

/// Quiet frames in a row that end a growing tail: 8 frames is 160ms.
/// hex: TAIL_QUIET_FRAMES = 8.
const QUIET_FRAMES: usize = 8;

/// The longest the mic may stay open after key-up. This is the privacy
/// bound Khalid approved, the only listening the key doesn't ask for.
/// hex: MAX_RELEASE_TAIL = 600ms.
pub(crate) const MAX_TAIL_MS: u64 = 600;

/// Which frame is "the floor": the 10th percentile, the quietest tenth.
/// hex finds a clip's noise floor the same way (src/dictation.rs,
/// noise_floor_dbfs).
const FLOOR_PERCENTILE: usize = 10;

/// What an all-zero frame reports, in place of log(0) = -infinity. Such
/// frames (a mic still waking up, a muted driver) say nothing about the
/// room, so the floor skips them. hex: SILENCE_LEVEL_FLOOR_DBFS = -120.
const DIGITAL_SILENCE_DBFS: f32 = -120.0;

/// The verdict. capture.rs asks again on every poll while it listens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tail {
    /// Speech is still going and the cap is still ahead: keep listening.
    Listen,
    /// Close the mic: the take should end `ms` after key-up. 0 means no
    /// tail at all, so whatever is already recorded is the whole take.
    Close { ms: u64, why: Why },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Why {
    /// You'd finished before letting go (or there was nothing to hear).
    QuietAtRelease,
    /// Speech ran into the release, then stopped.
    SpeechEnded,
    /// Speech was still going at MAX_TAIL_MS.
    Capped,
}

impl Why {
    /// The words in the per-take `[timing] tail` log line.
    pub(crate) fn describe(self) -> &'static str {
        match self {
            Why::QuietAtRelease => "quiet at release",
            Why::SpeechEnded => "speech ran into release",
            Why::Capped => "speech ran into release; capped",
        }
    }
}

/// Milliseconds since the key-up stamp. Both numbers come from
/// hotkey::now_ms(): one wall clock, read twice a few ms apart, so the
/// subtraction is honest. It returns 0 when there is no stamp (0) or the
/// clock stepped backwards in between (an NTP correction). "Key-up was
/// just now" is the safe reading: the tail then runs at most
/// MAX_TAIL_MS past the stop's arrival.
pub(crate) fn since_key_up(key_up_ms: u64, now_ms: u64) -> u64 {
    if key_up_ms == 0 {
        return 0;
    }
    now_ms.saturating_sub(key_up_ms)
}

/// Where key-up falls in the recording: `since_ms` before the newest
/// sample. The newest sample is a few ms old by the time we look, because
/// the driver hands audio over in ~10ms blocks. That puts key-up up to one
/// block early in the audio, which is less than one 20ms frame.
pub(crate) fn key_up_index(recorded: usize, rate: u32, since_ms: u64) -> usize {
    let back = since_ms.saturating_mul(rate as u64) / 1000;
    recorded.saturating_sub(back as usize)
}

/// This take's speech level: its noise floor plus SPEECH_MARGIN_DB, and
/// never below MIN_SPEECH_DBFS. It is measured on the audio before key-up
/// (the take proper), so the tail can't move its own goalposts. Returns
/// None when that audio says nothing about the room (empty, or only
/// digital zeros). Then nothing counts as speech, and there is no tail.
pub(crate) fn speech_threshold(before_release: &[f32], rate: u32) -> Option<f32> {
    let mut levels: Vec<f32> = before_release
        .chunks_exact(frame_len(rate))
        .map(level_dbfs)
        .filter(|&db| db > DIGITAL_SILENCE_DBFS)
        .collect();
    if levels.is_empty() {
        return None;
    }
    levels.sort_by(f32::total_cmp);
    let floor = levels[(levels.len() - 1) * FLOOR_PERCENTILE / 100];
    Some((floor + SPEECH_MARGIN_DB).max(MIN_SPEECH_DBFS))
}

/// The decision, over the take so far.
///
/// - `samples` is everything recorded.
/// - `key_up` is the index of the sample heard at key-up.
/// - `threshold` is this take's speech level (from speech_threshold).
///
/// Frames are counted outward from key-up: backwards to ask "were you
/// still talking?", forwards to ask "have you stopped yet?". So every
/// answer is a whole number of 20ms steps from the release itself.
pub(crate) fn decide(samples: &[f32], rate: u32, key_up: usize, threshold: f32) -> Tail {
    let len = frame_len(rate);
    let key_up = key_up.min(samples.len());
    let speech_at = |start: usize| level_dbfs(&samples[start..start + len]) >= threshold;

    // Backwards: did sustained speech run into the release?
    let Some(quiet_at_release) = speech_into(key_up, len, &speech_at) else {
        return Tail::Close {
            ms: 0,
            why: Why::QuietAtRelease,
        };
    };

    // Forwards: count quiet frames until QUIET_FRAMES in a row, or the
    // cap. The count starts with the quiet already running at key-up:
    // 160ms of quiet is 160ms, wherever it began.
    let mut quiet = quiet_at_release;
    for frame in 0..(MAX_TAIL_MS / FRAME_MS) as usize {
        let start = key_up + frame * len;
        if start + len > samples.len() {
            return Tail::Listen; // this frame hasn't been heard yet
        }
        quiet = if speech_at(start) { 0 } else { quiet + 1 };
        if quiet >= QUIET_FRAMES {
            return Tail::Close {
                ms: (frame as u64 + 1) * FRAME_MS,
                why: Why::SpeechEnded,
            };
        }
    }
    Tail::Close {
        ms: MAX_TAIL_MS,
        why: Why::Capped,
    }
}

/// Walks back from key-up one frame at a time, asking: was the speaker
/// still talking? Yes when the last speech frame sits within a syllable
/// gap of key-up, and at least SUSTAINED_FRAMES of speech lead up to it
/// with no gap longer than a syllable's. Then it returns how many quiet
/// frames sat between that last speech and key-up (0 to 2). It returns
/// None when silence comes first (you'd finished), or when the speech
/// runs out too soon (a click).
fn speech_into(key_up: usize, len: usize, speech_at: impl Fn(usize) -> bool) -> Option<usize> {
    let (mut speech, mut gap, mut quiet_at_release) = (0, 0, 0);
    let mut start = key_up;
    while start >= len {
        start -= len;
        if speech_at(start) {
            if speech == 0 {
                quiet_at_release = gap;
            }
            speech += 1;
            gap = 0;
            if speech >= SUSTAINED_FRAMES {
                return Some(quiet_at_release);
            }
        } else {
            gap += 1;
            if gap > SYLLABLE_GAP_FRAMES {
                return None;
            }
        }
    }
    None // the take began before enough speech was heard
}

/// Samples in one frame at this rate: 960 at 48kHz, 320 at 16kHz.
fn frame_len(rate: u32) -> usize {
    (rate as u64 * FRAME_MS / 1000).max(1) as usize
}

/// One frame's loudness in dBFS. The frame's mean comes off first,
/// because some mics add a DC offset (a constant push away from zero) that
/// would otherwise read as loud forever. What's left is the frame's
/// average power, the mean of its squared samples, on the decibel scale:
/// 10·log10(power).
fn level_dbfs(frame: &[f32]) -> f32 {
    /// 10·log10(1e-12) = -120dB, where DIGITAL_SILENCE_DBFS sits.
    const SILENT_POWER: f32 = 1e-12;
    let n = frame.len().max(1) as f32;
    let mean = frame.iter().sum::<f32>() / n;
    let power = frame.iter().map(|s| (s - mean) * (s - mean)).sum::<f32>() / n;
    // Written as "not above" so a NaN from a broken driver lands here too.
    if !(power > SILENT_POWER) {
        return DIGITAL_SILENCE_DBFS;
    }
    10.0 * power.log10()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    /// Windows' usual mic rate.
    const RATE: u32 = 48_000;

    /// Room hiss around -59 dBFS, scaled by `gain`. It's an integer hash
    /// of the sample index (lowbias32), so every run hears the same room.
    fn hiss(i: usize, gain: f32) -> f32 {
        let mut x = i as u32;
        x ^= x >> 16;
        x = x.wrapping_mul(0x7feb_352d);
        x ^= x >> 15;
        x = x.wrapping_mul(0x846c_a68b);
        x ^= x >> 16;
        (x as f32 / u32::MAX as f32 - 0.5) * 0.004 * gain
    }

    /// A 180Hz voice at about -13 dBFS while `ms` is in [from, to). This
    /// is hex's test voice.
    fn voice(ms: f64, from: f64, to: f64) -> f32 {
        if (from..to).contains(&ms) {
            (0.3 * (2.0 * PI * 180.0 * ms / 1000.0).sin()) as f32
        } else {
            0.0
        }
    }

    /// A key's clack: a loud, sharp 3kHz burst while `ms` is in [from, to).
    fn click(ms: f64, from: f64, to: f64) -> f32 {
        if (from..to).contains(&ms) {
            (0.8 * (2.0 * PI * 3000.0 * ms / 1000.0).sin()) as f32
        } else {
            0.0
        }
    }

    /// A take at `rate`: `before` ms recorded before key-up and `after`
    /// ms after it. `sound(ms)` is what the speaker adds at each moment,
    /// in ms from key-up (negative = before the release), over hiss
    /// scaled by `room`. Returns the samples and key-up's index.
    fn take(
        rate: u32,
        before: u64,
        after: u64,
        room: f32,
        sound: impl Fn(f64) -> f32,
    ) -> (Vec<f32>, usize) {
        let key_up = (before * rate as u64 / 1000) as usize;
        let len = ((before + after) * rate as u64 / 1000) as usize;
        let samples = (0..len)
            .map(|i| {
                let ms = (i as f64 - key_up as f64) * 1000.0 / rate as f64;
                sound(ms) + hiss(i, room)
            })
            .collect();
        (samples, key_up)
    }

    /// The usual take: held 3s, recorded 1s past key-up (more than any
    /// tail needs), at 48kHz in a quiet room.
    fn dictate(sound: impl Fn(f64) -> f32) -> (Vec<f32>, usize) {
        take(RATE, 3_000, 1_000, 1.0, sound)
    }

    /// Replays a take into decide() the way capture::stop does. The first
    /// call sees the audio up to `stop_at` ms after key-up (when the stop
    /// command arrives); each later poll sees 10ms more. Returns the
    /// verdict, and how much audio past key-up it had heard when it
    /// settled.
    fn settle(rate: u32, samples: &[f32], key_up: usize, stop_at: u64) -> (Tail, u64) {
        let threshold = speech_threshold(&samples[..key_up], rate).expect("the room has a floor");
        let mut heard = stop_at;
        loop {
            let end = key_up + (heard * rate as u64 / 1000) as usize;
            assert!(
                end <= samples.len(),
                "the take ran out while still listening"
            );
            match decide(&samples[..end], rate, key_up, threshold) {
                Tail::Listen => heard += 10,
                settled => return (settled, heard),
            }
        }
    }

    const QUIET: Tail = Tail::Close {
        ms: 0,
        why: Why::QuietAtRelease,
    };

    #[test]
    fn quiet_release_closes_at_once() {
        // The common case: finished speaking 300ms before letting go.
        let (s, up) = dictate(|ms| voice(ms, -2_000.0, -300.0));
        let (tail, heard) = settle(RATE, &s, up, 20);
        assert_eq!(tail, QUIET);
        assert_eq!(
            heard, 20,
            "a quiet release must not listen past the stop's arrival"
        );
    }

    #[test]
    fn quiet_release_needs_no_audio_after_key_up() {
        // The gap proof. capture::stop decides BEFORE it ever sleeps. If
        // the answer is Close with zero samples past key-up, then a quiet
        // release can't wait, whatever the dispatch timing.
        let (s, up) = take(RATE, 3_000, 0, 1.0, |ms| voice(ms, -2_000.0, -300.0));
        let threshold = speech_threshold(&s[..up], RATE).unwrap();
        assert_eq!(decide(&s[..up], RATE, up, threshold), QUIET);
        // The contrast: with speech running into the release, the same
        // first look says to keep listening.
        let (s, up) = take(RATE, 3_000, 0, 1.0, |ms| voice(ms, -2_000.0, 250.0));
        let threshold = speech_threshold(&s[..up], RATE).unwrap();
        assert_eq!(decide(&s[..up], RATE, up, threshold), Tail::Listen);
    }

    #[test]
    fn a_release_soon_after_the_last_word_is_still_quiet() {
        // 80ms of quiet before key-up is already more than a syllable gap:
        // you'd finished.
        let (s, up) = dictate(|ms| voice(ms, -2_000.0, -80.0));
        assert_eq!(settle(RATE, &s, up, 20), (QUIET, 20));
    }

    #[test]
    fn speech_through_the_release_listens_until_160ms_of_quiet() {
        // Still talking when the key came up; silent from +250ms. 250
        // lands mid-frame (240-260). That frame still holds 10ms of voice,
        // so quiet starts at 260: 260 + 160 = 420. That's "about
        // 250 + 160" at 20ms resolution.
        let (s, up) = dictate(|ms| voice(ms, -2_000.0, 250.0));
        let (tail, heard) = settle(RATE, &s, up, 20);
        assert_eq!(
            tail,
            Tail::Close {
                ms: 420,
                why: Why::SpeechEnded
            }
        );
        assert_eq!(
            heard, 420,
            "closes on the poll where the 160ms of quiet arrives"
        );
    }

    #[test]
    fn speech_that_never_stops_is_capped_at_600ms() {
        let (s, up) = dictate(|ms| voice(ms, -2_000.0, 10_000.0));
        let (tail, heard) = settle(RATE, &s, up, 20);
        assert_eq!(
            tail,
            Tail::Close {
                ms: MAX_TAIL_MS,
                why: Why::Capped
            }
        );
        assert_eq!(heard, MAX_TAIL_MS);
    }

    #[test]
    fn the_tail_is_the_end_of_speech_plus_160ms_up_to_600() {
        for end in [0u64, 100, 200, 300, 400, 440, 460, 500] {
            let (s, up) = dictate(|ms| voice(ms, -2_000.0, end as f64));
            let (tail, _) = settle(RATE, &s, up, 20);
            let expected = if end + 160 <= MAX_TAIL_MS {
                Tail::Close {
                    ms: end + 160,
                    why: Why::SpeechEnded,
                }
            } else {
                Tail::Close {
                    ms: MAX_TAIL_MS,
                    why: Why::Capped,
                }
            };
            assert_eq!(tail, expected, "speech until +{end}ms");
        }
    }

    #[test]
    fn a_click_at_release_is_not_speech() {
        // The key's own clack, right as it comes up, after a finished
        // sentence.
        let (s, up) = dictate(|ms| voice(ms, -2_000.0, -400.0) + click(ms, -12.0, 0.0));
        assert_eq!(settle(RATE, &s, up, 20), (QUIET, 20));
    }

    #[test]
    fn a_linux_finish_tap_is_two_clicks_not_speech() {
        // Linux ends a take with one tap of left Ctrl: a clack going down
        // ~100ms before the stamp, and another coming up at it.
        let (s, up) = dictate(|ms| {
            voice(ms, -2_000.0, -500.0) + click(ms, -110.0, -100.0) + click(ms, -10.0, 0.0)
        });
        assert_eq!(settle(RATE, &s, up, 20), (QUIET, 20));
    }

    #[test]
    fn time_spent_reaching_the_stop_counts_toward_the_tail() {
        // Speech ran 100ms past key-up, and the stop took 300ms to arrive
        // (a busy webview). The tail ended at 100 + 160 = 260. That audio
        // is already in the buffer, so the stop listens no further.
        let (s, up) = dictate(|ms| voice(ms, -2_000.0, 100.0));
        let (tail, heard) = settle(RATE, &s, up, 300);
        assert_eq!(
            tail,
            Tail::Close {
                ms: 260,
                why: Why::SpeechEnded
            }
        );
        assert_eq!(heard, 300);
    }

    #[test]
    fn a_stop_arriving_after_the_cap_closes_at_once() {
        let (s, up) = dictate(|ms| voice(ms, -2_000.0, 10_000.0));
        let (tail, heard) = settle(RATE, &s, up, 700);
        assert_eq!(
            tail,
            Tail::Close {
                ms: MAX_TAIL_MS,
                why: Why::Capped
            }
        );
        assert_eq!(heard, 700);
    }

    #[test]
    fn a_syllable_gap_at_release_still_counts_as_talking() {
        // Let go in the 40ms hush between two syllables of the last word.
        let (s, up) = dictate(|ms| voice(ms, -2_000.0, -30.0) + voice(ms, 10.0, 200.0));
        let (tail, _) = settle(RATE, &s, up, 20);
        assert_eq!(
            tail,
            Tail::Close {
                ms: 360,
                why: Why::SpeechEnded
            }
        );
    }

    #[test]
    fn a_real_pause_at_release_counts_as_finished() {
        // The known edge. Let go in a full pause (100ms+) BEFORE the last
        // word, and that word hadn't started at key-up. That looks exactly
        // like having finished, so it's cut, as it was before this file.
        // Catching it would take a fixed tail on every take.
        let (s, up) = dictate(|ms| voice(ms, -2_000.0, -100.0) + voice(ms, 50.0, 400.0));
        assert_eq!(settle(RATE, &s, up, 20), (QUIET, 20));
    }

    #[test]
    fn a_loud_room_is_not_speech_but_speech_over_it_is() {
        // A fan at about -29 dBFS: the floor rises with it.
        let fan = 30.0;
        let (s, up) = take(RATE, 3_000, 1_000, fan, |ms| voice(ms, -2_000.0, -400.0));
        assert_eq!(settle(RATE, &s, up, 20), (QUIET, 20));
        let (s, up) = take(RATE, 3_000, 1_000, fan, |ms| voice(ms, -2_000.0, 200.0));
        let (tail, _) = settle(RATE, &s, up, 20);
        assert_eq!(
            tail,
            Tail::Close {
                ms: 360,
                why: Why::SpeechEnded
            }
        );
    }

    #[test]
    fn works_at_any_mic_rate() {
        for rate in [16_000, 44_100, 48_000] {
            let (s, up) = take(rate, 3_000, 1_000, 1.0, |ms| voice(ms, -2_000.0, 250.0));
            let (tail, _) = settle(rate, &s, up, 20);
            assert_eq!(
                tail,
                Tail::Close {
                    ms: 420,
                    why: Why::SpeechEnded
                },
                "at {rate}Hz"
            );
        }
    }

    #[test]
    fn the_floor_is_the_room_not_the_voice() {
        // Two of three seconds are voice, yet the floor is the hiss
        // (~-59 dBFS), so speech starts 12dB above that.
        let (s, up) = take(RATE, 3_000, 0, 1.0, |ms| voice(ms, -2_000.0, 0.0));
        let threshold = speech_threshold(&s[..up], RATE).unwrap();
        assert!((-48.0..-45.5).contains(&threshold), "{threshold}");
    }

    #[test]
    fn a_near_silent_mic_still_needs_minus_55_dbfs() {
        // Hiss at about -85 dBFS: 12dB over that is still inaudible fizz.
        let (s, up) = take(RATE, 1_000, 0, 0.05, |_| 0.0);
        assert_eq!(speech_threshold(&s[..up], RATE), Some(MIN_SPEECH_DBFS));
    }

    #[test]
    fn digital_silence_says_nothing_about_the_room() {
        assert_eq!(speech_threshold(&[], RATE), None);
        assert_eq!(speech_threshold(&vec![0.0; 48_000], RATE), None);
        // A constant DC offset is no sound either: the mean comes off first.
        assert_eq!(speech_threshold(&vec![0.25; 48_000], RATE), None);
    }

    #[test]
    fn key_up_is_placed_by_one_wall_clock() {
        assert_eq!(since_key_up(10_000, 10_018), 18);
        assert_eq!(since_key_up(0, 10_018), 0, "no stamp: key-up is now");
        assert_eq!(
            since_key_up(10_000, 9_990),
            0,
            "clock stepped back: key-up is now"
        );
        // 18ms at 48kHz is 864 samples back from the newest.
        assert_eq!(key_up_index(48_000, RATE, 18), 48_000 - 864);
        // A stamp older than the take itself (a tap that beat the mic
        // open): key-up sits at sample 0, with nothing before it to call
        // speech.
        assert_eq!(key_up_index(4_800, RATE, 500), 0);
    }
}

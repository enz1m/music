//! A track's waveform: the loudness envelope the now-playing seek bar draws.
//!
//! [`extract`] decodes the file with GStreamer at a low mono rate and keeps
//! the peak of every short block, then folds the blocks into [`BARS`] bars
//! scaled to the loudest. It runs off the UI thread (see
//! `MusicApp::request_waveform`); a file GStreamer cannot decode has none,
//! and the seek bar draws even bars instead.

use std::path::Path;

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

/// How many bars the seek bar draws.
pub const BARS: usize = 96;

/// The decode rate: enough for a loudness envelope, cheap to decode to.
const RATE: u32 = 4_000;

/// Samples per block (a quarter of a second at [`RATE`]).
const BLOCK: usize = 1_000;

/// The longest a decode may take before it is abandoned.
const DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

/// The waveform of the audio file at `path`, as [`BARS`] heights in
/// `0.0..=1.0`, or `None` when it cannot be decoded.
#[must_use]
pub fn extract(path: &Path) -> Option<Vec<f32>> {
    gst::init().ok()?;
    let pipeline = gst::Pipeline::new();
    let source = gst::ElementFactory::make("filesrc")
        .property("location", path.to_str()?)
        .build()
        .ok()?;
    let decode = gst::ElementFactory::make("decodebin").build().ok()?;
    let convert = gst::ElementFactory::make("audioconvert").build().ok()?;
    let resample = gst::ElementFactory::make("audioresample").build().ok()?;
    let sink = gst_app::AppSink::builder()
        .caps(
            &gst::Caps::builder("audio/x-raw")
                .field("format", "F32LE")
                .field("channels", 1i32)
                .field("rate", RATE as i32)
                .field("layout", "interleaved")
                .build(),
        )
        .sync(false)
        .build();
    pipeline
        .add_many([&source, &decode, &convert, &resample, sink.upcast_ref()])
        .ok()?;
    source.link(&decode).ok()?;
    gst::Element::link_many([&convert, &resample, sink.upcast_ref()]).ok()?;
    // decodebin's audio pad appears once it has typed the stream.
    let convert_weak = convert.downgrade();
    decode.connect_pad_added(move |_, pad| {
        let Some(convert) = convert_weak.upgrade() else {
            return;
        };
        let is_audio = pad
            .current_caps()
            .and_then(|caps| caps.structure(0).map(|s| s.name().starts_with("audio/")))
            .unwrap_or(false);
        if let Some(sink_pad) = convert.static_pad("sink")
            && is_audio
            && !sink_pad.is_linked()
        {
            let _ = pad.link(&sink_pad);
        }
    });

    pipeline.set_state(gst::State::Playing).ok()?;
    let peaks = pull_block_peaks(&sink);
    let _ = pipeline.set_state(gst::State::Null);
    fold(&peaks?)
}

/// The peak of every [`BLOCK`] samples until end of stream, or `None` on a
/// decode error or the deadline.
fn pull_block_peaks(sink: &gst_app::AppSink) -> Option<Vec<f32>> {
    let started = std::time::Instant::now();
    let mut peaks = Vec::new();
    let mut block_peak = 0.0_f32;
    let mut in_block = 0;
    loop {
        if started.elapsed() > DEADLINE {
            return None;
        }
        let Some(sample) = sink.try_pull_sample(gst::ClockTime::from_mseconds(500)) else {
            if sink.is_eos() {
                break;
            }
            // A pipeline that errored stops producing without reaching EOS.
            let errored = sink
                .bus()
                .and_then(|bus| bus.pop_filtered(&[gst::MessageType::Error]))
                .is_some();
            if errored {
                return None;
            }
            continue;
        };
        let Some(buffer) = sample.buffer() else {
            continue;
        };
        let Ok(map) = buffer.map_readable() else {
            continue;
        };
        for chunk in map.as_slice().as_chunks::<4>().0 {
            let value = f32::from_le_bytes(*chunk).abs();
            if value.is_finite() {
                block_peak = block_peak.max(value);
            }
            in_block += 1;
            if in_block == BLOCK {
                peaks.push(block_peak);
                block_peak = 0.0;
                in_block = 0;
            }
        }
    }
    if in_block > 0 {
        peaks.push(block_peak);
    }
    (!peaks.is_empty()).then_some(peaks)
}

/// Fold block peaks into [`BARS`] bars scaled to the loudest, or `None` for
/// silence.
fn fold(blocks: &[f32]) -> Option<Vec<f32>> {
    let bars: Vec<f32> = (0..BARS)
        .map(|bar| {
            let start = bar * blocks.len() / BARS;
            let end = ((bar + 1) * blocks.len() / BARS)
                .max(start + 1)
                .min(blocks.len());
            blocks
                .get(start..end)
                .map_or(0.0, |span| span.iter().copied().fold(0.0, f32::max))
        })
        .collect();
    let loudest = bars.iter().copied().fold(0.0, f32::max);
    (loudest > 0.0).then(|| bars.into_iter().map(|bar| bar / loudest).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audible_waveforms_are_normalized_and_silence_is_absent() {
        for blocks in [
            vec![0.5, 1.0],
            (0..BARS * 2).map(|i| (i % 4) as f32 * 0.1).collect(),
        ] {
            let bars = fold(&blocks).expect("audible");
            assert_eq!(bars.len(), BARS);
            assert!(bars.iter().all(|bar| (0.0..=1.0).contains(bar)));
            assert!(bars.iter().any(|bar| (*bar - 1.0).abs() < f32::EPSILON));
        }
        assert_eq!(fold(&[0.0; 10]), None);
    }
}

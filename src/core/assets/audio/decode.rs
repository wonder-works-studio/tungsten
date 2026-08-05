use anyhow::{Context, Result, anyhow, bail};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{CODEC_TYPE_NULL, DecoderOptions};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// Decoded PCM audio: interleaved 32-bit float samples.
pub struct DecodedAudio {
    /// Interleaved samples in the range `[-1.0, 1.0]`.
    pub samples: Vec<f32>,
    /// Number of channels.
    pub channels: u32,
    /// Sample rate in Hz.
    pub sample_rate: u32,
}

impl DecodedAudio {
    /// Number of frames (samples per channel).
    #[allow(dead_code)]
    pub fn frames(&self) -> usize {
        if self.channels == 0 {
            0
        } else {
            self.samples.len() / self.channels as usize
        }
    }
}

/// Decode any supported audio container (WAV, MP3, M4A/AAC, OGG, FLAC)
/// to interleaved f32 PCM via symphonia.
///
/// `ext` is the source file extension, used as a probe hint.
pub fn decode(data: &[u8], ext: &str) -> Result<DecodedAudio> {
    let cursor = std::io::Cursor::new(data.to_vec());
    let mss = MediaSourceStream::new(Box::new(cursor), Default::default());

    let mut hint = Hint::new();
    if !ext.is_empty() {
        hint.with_extension(ext);
    }

    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .context("Failed to probe audio container format")?;

    let mut format = probed.format;

    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| anyhow!("No decodable audio track found"))?;
    let track_id = track.id;

    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .context("Failed to create audio decoder")?;

    let mut samples: Vec<f32> = Vec::new();
    let mut channels: u32 = 0;
    let mut sample_rate: u32 = 0;
    let mut sample_buf: Option<SampleBuffer<f32>> = None;

    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            // End of stream.
            Err(SymphoniaError::IoError(e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(SymphoniaError::ResetRequired) => break,
            Err(e) => return Err(e).context("Failed to read audio packet"),
        };

        if packet.track_id() != track_id {
            continue;
        }

        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            // Recoverable decode errors: skip the packet.
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(e) => return Err(e).context("Failed to decode audio packet"),
        };

        let spec = *decoded.spec();
        if channels == 0 {
            channels = spec.channels.count() as u32;
            sample_rate = spec.rate;
        }

        let buf = sample_buf.get_or_insert_with(|| {
            SampleBuffer::<f32>::new(decoded.capacity() as u64, spec)
        });
        if buf.capacity() < decoded.capacity() * spec.channels.count() {
            *buf = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
        }
        buf.copy_interleaved_ref(decoded);
        samples.extend_from_slice(buf.samples());
    }

    if channels == 0 || sample_rate == 0 || samples.is_empty() {
        bail!("Audio stream contained no decodable samples");
    }

    Ok(DecodedAudio {
        samples,
        channels,
        sample_rate,
    })
}

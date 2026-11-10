//! Audio in the frames an AirPlay receiver decodes as Apple Lossless: its
//! "escape" frames carry the samples as they are, so no encoder is needed
//! and what the receiver plays is exactly what was decoded here.

/// Sample frames (one sample of each channel) in a packet.
pub const FRAMES_PER_PACKET: usize = 352;
pub const CHANNELS: usize = 2;
/// Samples in one packet.
pub const SAMPLES_PER_PACKET: usize = FRAMES_PER_PACKET * CHANNELS;

/// The `a=fmtp` line's parameters that tell the receiver the frames are of
/// this size, 16-bit stereo at 44.1 kHz.
pub const FORMAT_PARAMETERS: &str = "352 0 16 40 10 14 2 255 0 0 44100";

/// Channel element of a pair, which stereo is.
const CHANNEL_PAIR: u64 = 1;
/// Marks the end of the frame.
const END: u64 = 7;
/// Header bits after the element's tag: unused, then partial frame (0),
/// shifted bytes (0) and escape (1) as ALAC's decoder reads them.
const UNUSED_BITS: u32 = 12;
const FLAGS_ESCAPE: u64 = 0b0001;

/// The ALAC frame of one packet of interleaved `samples`.
///
/// # Panics
/// When `samples` are not [`SAMPLES_PER_PACKET`]: the caller pads the last
/// packet of a track with silence.
pub fn escape_frame(samples: &[i16]) -> Vec<u8> {
    assert_eq!(
        samples.len(),
        SAMPLES_PER_PACKET,
        "a whole packet of samples"
    );
    let mut bits = BitWriter::default();
    bits.write(CHANNEL_PAIR, 3);
    bits.write(0, 4);
    bits.write(0, UNUSED_BITS);
    bits.write(FLAGS_ESCAPE, 4);
    for &sample in samples {
        bits.write(u64::from(sample as u16), 16);
    }
    bits.write(END, 3);
    bits.finish()
}

/// Bits written most significant first, as ALAC reads them.
#[derive(Default)]
struct BitWriter {
    bytes: Vec<u8>,
    /// Bits of the last byte in use.
    used: u32,
}

impl BitWriter {
    fn write(&mut self, value: u64, count: u32) {
        for shift in (0..count).rev() {
            if self.used == 0 {
                self.bytes.push(0);
            }
            let bit = ((value >> shift) & 1) as u8;
            *self.bytes.last_mut().expect("a byte was just pushed") |= bit << (7 - self.used);
            self.used = (self.used + 1) % 8;
        }
    }

    fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reads `count` bits from bit `at` of `bytes`.
    fn bits(bytes: &[u8], at: usize, count: usize) -> u64 {
        (at..at + count).fold(0, |value, bit| {
            (value << 1) | u64::from(bytes[bit / 8] >> (7 - bit % 8) & 1)
        })
    }

    fn ramp() -> Vec<i16> {
        (0..SAMPLES_PER_PACKET as i32)
            .map(|n| (n * 97 - 30_000) as i16)
            .collect()
    }

    #[test]
    fn the_frame_header_says_a_stereo_pair_of_uncompressed_samples() {
        let frame = escape_frame(&ramp());
        assert_eq!(bits(&frame, 0, 3), 1, "a channel pair");
        assert_eq!(bits(&frame, 3, 4), 0, "the first element");
        assert_eq!(bits(&frame, 7, 12), 0, "unused");
        assert_eq!(bits(&frame, 19, 4), 0b0001, "whole, unshifted, escaped");
    }

    #[test]
    fn the_samples_follow_the_header_as_they_are_big_endian_and_the_frame_ends_with_its_marker() {
        let samples = ramp();
        let frame = escape_frame(&samples);
        let header = 23;
        let decoded: Vec<i16> = (0..SAMPLES_PER_PACKET)
            .map(|n| bits(&frame, header + n * 16, 16) as u16 as i16)
            .collect();
        assert_eq!(decoded, samples);
        let end = header + SAMPLES_PER_PACKET * 16;
        assert_eq!(bits(&frame, end, 3), 7);
        assert_eq!(frame.len(), (end + 3).div_ceil(8));
    }

    #[test]
    #[should_panic(expected = "whole packet")]
    fn a_part_of_a_packet_is_refused() {
        escape_frame(&[0; 10]);
    }
}

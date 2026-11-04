//! Which plate a station logo sits on. A logo on see-through pixels gets
//! dark ink on a light plate and white ink on a dark one. A logo that
//! carries its own background, opaque to its edges, gets the plate of that
//! background, so a white card never sits on the dark plate.

use slint::Image;

type Pixel = [u8; 4];

/// Pixels looked at for the ink, at most; a logo has no more detail than
/// this needs.
const SAMPLES_AT_MOST: usize = 4096;
/// The average brightness, 0 to 255, from which ink or a background counts
/// as light. It is where a dark and a light plate contrast equally with it.
const LIGHT_FROM: f32 = 128.0;
/// The alpha from which a pixel counts as opaque.
const OPAQUE_FROM: u8 = 250;
const BYTES_PER_PIXEL: usize = 4;

/// Whether the logo's RGBA pixels, `width` across, want the dark plate. A
/// logo with no visible pixels has no ink and keeps the light plate.
pub fn wants_dark_plate(rgba: &[u8], width: usize) -> bool {
    let pixels = rgba.as_chunks::<BYTES_PER_PIXEL>().0;
    match opaque_edge(pixels, width) {
        Some(edge) => !is_light(edge.iter()),
        None => {
            let step = pixels.len().div_ceil(SAMPLES_AT_MOST).max(1);
            is_light(pixels.iter().step_by(step))
        }
    }
}

pub fn of_image(image: &Image) -> bool {
    let width = image.size().width as usize;
    image
        .to_rgba8()
        .is_some_and(|pixels| wants_dark_plate(pixels.as_bytes(), width))
}

/// The pixels along the logo's four edges, when every one is opaque.
fn opaque_edge(pixels: &[Pixel], width: usize) -> Option<Vec<Pixel>> {
    if width == 0 {
        return None;
    }
    let rows: Vec<&[Pixel]> = pixels.chunks_exact(width).collect();
    let (first, last) = (rows.first()?, rows.last()?);
    let sides = rows.iter().flat_map(|row| [row[0], row[width - 1]]);
    let edge: Vec<Pixel> = first
        .iter()
        .chain(last.iter())
        .copied()
        .chain(sides)
        .collect();
    edge.iter()
        .all(|pixel| pixel[3] >= OPAQUE_FROM)
        .then_some(edge)
}

/// Whether the pixels' brightness, weighted by how visible each is, is light.
fn is_light<'a>(pixels: impl Iterator<Item = &'a Pixel>) -> bool {
    let (mut brightness, mut weight) = (0.0_f32, 0.0_f32);
    for pixel in pixels {
        let [red, green, blue, alpha] = pixel.map(f32::from);
        brightness += alpha * (0.2126 * red + 0.7152 * green + 0.0722 * blue);
        weight += alpha;
    }
    weight > 0.0 && brightness / weight >= LIGHT_FROM
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOTHING: Pixel = [0, 0, 0, 0];
    const BLACK: Pixel = [0, 0, 0, 255];
    const WHITE: Pixel = [255, 255, 255, 255];

    /// A `size` square picture whose middle `inner` pixels across are
    /// painted by `inside`, and the rest by `outside`.
    fn picture(
        size: usize,
        inner: usize,
        inside: impl Fn(usize) -> Pixel,
        outside: Pixel,
    ) -> Vec<u8> {
        let margin = (size - inner) / 2;
        let middle = margin..margin + inner;
        let mut painted = 0;
        let mut rgba = Vec::new();
        for (row, column) in (0..size).flat_map(|row| (0..size).map(move |column| (row, column))) {
            let pixel = if middle.contains(&row) && middle.contains(&column) {
                painted += 1;
                inside(painted - 1)
            } else {
                outside
            };
            rgba.extend(pixel);
        }
        rgba
    }

    fn on_nothing(ink: Pixel) -> bool {
        wants_dark_plate(&picture(10, 6, |_| ink, NOTHING), 10)
    }

    #[test]
    fn dark_ink_gets_the_light_plate_and_white_ink_the_dark_one() {
        assert!(!on_nothing([20, 20, 20, 255]));
        assert!(on_nothing([250, 250, 250, 255]));
    }

    #[test]
    fn colours_count_by_their_brightness() {
        assert!(!on_nothing([200, 0, 0, 255]));
        assert!(on_nothing([0, 255, 0, 255]));
    }

    #[test]
    fn mixed_ink_goes_by_which_there_is_more_of() {
        // The middle is 36 pixels: 22 of one ink, 14 of the other.
        let mostly = |first: Pixel, second: Pixel| {
            wants_dark_plate(
                &picture(10, 6, |n| if n < 22 { first } else { second }, NOTHING),
                10,
            )
        };
        assert!(!mostly(BLACK, WHITE));
        assert!(mostly(WHITE, BLACK));
    }

    #[test]
    fn see_through_pixels_do_not_count_whatever_their_colour() {
        let white_but_unseen = [255, 255, 255, 0];
        assert!(!wants_dark_plate(
            &picture(10, 6, |_| BLACK, white_but_unseen),
            10
        ));
        let black_but_unseen = [0, 0, 0, 0];
        assert!(wants_dark_plate(
            &picture(10, 6, |_| WHITE, black_but_unseen),
            10
        ));
    }

    #[test]
    fn faint_pixels_weigh_less_than_solid_ones() {
        let faint_black = [0, 0, 0, 20];
        let some_white = |n: usize| if n < 10 { WHITE } else { faint_black };
        assert!(wants_dark_plate(
            &picture(10, 6, some_white, faint_black),
            10
        ));
    }

    #[test]
    fn a_logo_with_nothing_visible_keeps_the_light_plate() {
        assert!(!wants_dark_plate(&picture(10, 6, |_| NOTHING, NOTHING), 10));
        assert!(!wants_dark_plate(&[], 0));
    }

    #[test]
    fn a_big_logo_is_judged_by_a_sample_of_it() {
        let mostly_dark = |n: usize| if n.is_multiple_of(1000) { WHITE } else { BLACK };
        assert!(!wants_dark_plate(
            &picture(1000, 800, mostly_dark, NOTHING),
            1000
        ));
        assert!(wants_dark_plate(
            &picture(1000, 800, |_| WHITE, NOTHING),
            1000
        ));
    }

    #[test]
    fn a_white_card_gets_the_light_plate_and_a_dark_card_the_dark_one() {
        let dark_mark = picture(10, 6, |_| BLACK, WHITE);
        assert!(!wants_dark_plate(&dark_mark, 10));
        let white_mark = picture(10, 6, |_| WHITE, BLACK);
        assert!(wants_dark_plate(&white_mark, 10));
    }

    #[test]
    fn a_card_is_judged_by_its_background_not_its_mark() {
        // A mark covering most of the card, close to the card's own tone.
        let grey = [150, 150, 150, 255];
        assert!(!wants_dark_plate(&picture(10, 8, |_| grey, WHITE), 10));
        let dark_grey = [100, 100, 100, 255];
        assert!(wants_dark_plate(&picture(10, 8, |_| dark_grey, BLACK), 10));
    }

    #[test]
    fn a_card_with_one_see_through_corner_is_judged_by_its_ink() {
        let mut rgba = picture(10, 6, |_| BLACK, WHITE);
        rgba[..BYTES_PER_PIXEL].copy_from_slice(&NOTHING);
        assert!(wants_dark_plate(&rgba, 10));
    }

    #[test]
    fn a_card_with_one_see_through_pixel_on_a_side_is_judged_by_its_ink() {
        let mut rgba = picture(10, 6, |_| BLACK, WHITE);
        let side = 4 * 10 * BYTES_PER_PIXEL;
        rgba[side..side + BYTES_PER_PIXEL].copy_from_slice(&NOTHING);
        assert!(wants_dark_plate(&rgba, 10));
    }

    #[test]
    fn an_edge_that_is_nearly_opaque_does_not_make_a_card() {
        let nearly = [255, 255, 255, 249];
        assert!(wants_dark_plate(&picture(10, 6, |_| BLACK, nearly), 10));
        let card = [255, 255, 255, 250];
        assert!(!wants_dark_plate(&picture(10, 6, |_| WHITE, card), 10));
    }
}

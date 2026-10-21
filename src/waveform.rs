use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

pub const HUD_WIDTH: u32 = 360;
pub const HUD_HEIGHT: u32 = 78;
pub const WAVEFORM_HEIGHT: u32 = 58;
pub const WAVEFORM_BOTTOM_PADDING: u32 = 14;
pub const WAVEFORM_LEVEL_FLOOR: f32 = 0.10;
pub const WAVEFORM_LEVEL_CEILING: f32 = 0.62;
pub const LEVEL_HISTORY: usize = 180;
const PROCESSING_CYCLE_SECONDS: f32 = 1.25;
const PROCESSING_WAVE_COUNT: f32 = 1.75;

pub type SharedLevels = Arc<Mutex<VecDeque<f32>>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WaveformColor {
    r: u8,
    g: u8,
    b: u8,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WaveformStyle {
    pub gradient_start: WaveformColor,
    pub gradient_end: WaveformColor,
    pub rounded_background: bool,
    pub speech_gain: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WaveformMode {
    Listening,
    Processing { elapsed_seconds: f32 },
}

impl WaveformColor {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    fn mix(self, other: Self, amount: f32) -> Self {
        let amount = amount.clamp(0.0, 1.0);
        Self {
            r: mix_channel(self.r, other.r, amount),
            g: mix_channel(self.g, other.g, amount),
            b: mix_channel(self.b, other.b, amount),
        }
    }

    fn to_argb(self) -> u32 {
        0xff00_0000 | (u32::from(self.r) << 16) | (u32::from(self.g) << 8) | u32::from(self.b)
    }
}

pub fn shared_levels() -> SharedLevels {
    Arc::new(Mutex::new(VecDeque::with_capacity(LEVEL_HISTORY)))
}

pub fn push_level(levels: &SharedLevels, level: f32) {
    if let Ok(mut levels) = levels.lock() {
        let previous = levels.back().copied().unwrap_or(0.0);
        let smoothed = previous * 0.72 + level.clamp(0.0, 1.0) * 0.28;
        if levels.len() >= LEVEL_HISTORY {
            levels.pop_front();
        }
        levels.push_back(smoothed);
    }
}

pub fn snapshot(levels: &SharedLevels) -> Vec<f32> {
    levels
        .lock()
        .map(|levels| levels.iter().copied().collect())
        .unwrap_or_default()
}

pub fn parse_hex_color(value: &str, fallback: WaveformColor) -> WaveformColor {
    let value = value.trim().trim_matches('"').trim_start_matches('#');
    if value.len() != 6 {
        return fallback;
    }

    let Ok(parsed) = u32::from_str_radix(value, 16) else {
        return fallback;
    };

    WaveformColor::new(
        ((parsed >> 16) & 0xff) as u8,
        ((parsed >> 8) & 0xff) as u8,
        (parsed & 0xff) as u8,
    )
}

pub fn draw_waveform(
    buffer: &mut [u32],
    width: u32,
    height: u32,
    levels: &[f32],
    response: f32,
    mode: WaveformMode,
    style: WaveformStyle,
) {
    draw_waveform_with_scale(buffer, width, height, 1.0, levels, response, mode, style);
}

/// Render logical waveform dimensions into a physical-pixel backing buffer.
/// macOS Retina windows have twice as many pixels per point; scaling only the
/// window would otherwise halve the apparent bar heights, widths and spacing.
/// Speech gain affects only the listening excursion above the silent baseline.
pub fn draw_waveform_with_scale(
    buffer: &mut [u32],
    width: u32,
    height: u32,
    scale_factor: f64,
    levels: &[f32],
    response: f32,
    mode: WaveformMode,
    style: WaveformStyle,
) {
    let scale_factor = if scale_factor.is_finite() && scale_factor > 0.0 {
        scale_factor
    } else {
        1.0
    };
    let pixels = |points: u32| (f64::from(points) * scale_factor).round().max(1.0) as u32;
    let width = width.max(1);
    let height = height.max(1);
    buffer.fill(0);

    if style.rounded_background {
        draw_rounded_background(buffer, width, height, pixels(18));
    } else {
        buffer.fill(0xff00_0000);
    }

    let waveform_height = pixels(WAVEFORM_HEIGHT);
    let minimum_bar_height = pixels(4);
    let waveform_bottom = height
        .saturating_sub(pixels(WAVEFORM_BOTTOM_PADDING))
        .max(1);
    let waveform_top = waveform_bottom.saturating_sub(waveform_height);
    let center = waveform_top + (waveform_bottom.saturating_sub(waveform_top) / 2);

    let left = pixels(42);
    let right = width.saturating_sub(pixels(42));
    let bar_width = pixels(3);
    let gap = pixels(5);
    let stride = bar_width + gap;
    let drawable_width = right.saturating_sub(left).max(1);
    let bar_count = (drawable_width / stride).max(1);

    for bar_index in 0..bar_count {
        let x = left + bar_index * stride;
        let progress = bar_index as f32 / bar_count.saturating_sub(1).max(1) as f32;
        let color = style
            .gradient_start
            .mix(style.gradient_end, progress)
            .to_argb();
        let level = match mode {
            WaveformMode::Listening => {
                let raw_level = if levels.is_empty() {
                    0.0
                } else {
                    // Each bar represents a complete interval of history. Keep
                    // its peak so short syllables cannot fall between sampled
                    // points as more audio fills the fixed-width HUD.
                    let start = bar_index as usize * levels.len() / bar_count as usize;
                    let end = ((bar_index as usize + 1) * levels.len() / bar_count as usize)
                        .max(start + 1)
                        .min(levels.len());
                    let peak = levels[start..end].iter().copied().fold(0.0_f32, f32::max);
                    (peak.sqrt() * response).clamp(0.0, 1.0)
                };
                // Position is decorative, not an audio measurement: identical
                // levels must have identical heights everywhere in the HUD.
                (shape_waveform_level(raw_level) * style.speech_gain).clamp(0.0, 1.0)
            }
            WaveformMode::Processing { elapsed_seconds } => {
                processing_ripple(progress, elapsed_seconds)
            }
        };
        let bar_height = (minimum_bar_height as f32
            + level * waveform_height.saturating_sub(minimum_bar_height) as f32)
            .round() as u32;
        let y = center.saturating_sub(bar_height / 2);
        draw_waveform_bar(buffer, width, x, y, bar_width, bar_height, color);
    }
}

fn processing_ripple(progress: f32, elapsed_seconds: f32) -> f32 {
    let phase = (elapsed_seconds.max(0.0) / PROCESSING_CYCLE_SECONDS).fract();
    let angle = std::f32::consts::TAU * (PROCESSING_WAVE_COUNT * progress - phase);
    let wave = (angle.sin() + (angle * 2.0 + 0.55).sin() * 0.16) / 1.16;
    let normalized = (0.5 + wave * 0.5).clamp(0.0, 1.0);
    0.14 + normalized.powf(1.25) * 0.78
}

fn shape_waveform_level(level: f32) -> f32 {
    let normalized = ((level - WAVEFORM_LEVEL_FLOOR)
        / (WAVEFORM_LEVEL_CEILING - WAVEFORM_LEVEL_FLOOR))
        .clamp(0.0, 1.0);
    normalized * normalized * (3.0 - 2.0 * normalized)
}

fn draw_rounded_background(buffer: &mut [u32], width: u32, height: u32, radius: u32) {
    let radius = radius.min(width / 2).min(height / 2);
    let radius_squared = i64::from(radius) * i64::from(radius);
    for y in 0..height {
        for x in 0..width {
            let corner_x = if x < radius {
                radius.saturating_sub(x)
            } else if x >= width.saturating_sub(radius) {
                x.saturating_sub(width.saturating_sub(radius).saturating_sub(1))
            } else {
                0
            };
            let corner_y = if y < radius {
                radius.saturating_sub(y)
            } else if y >= height.saturating_sub(radius) {
                y.saturating_sub(height.saturating_sub(radius).saturating_sub(1))
            } else {
                0
            };
            if corner_x == 0
                || corner_y == 0
                || i64::from(corner_x) * i64::from(corner_x)
                    + i64::from(corner_y) * i64::from(corner_y)
                    <= radius_squared
            {
                buffer[y as usize * width as usize + x as usize] = 0xf200_0000;
            }
        }
    }
}

fn draw_waveform_bar(buffer: &mut [u32], width: u32, x: u32, y: u32, w: u32, h: u32, color: u32) {
    if h <= 2 {
        draw_rect(buffer, width, x, y, w, h, color);
        return;
    }

    draw_rect(
        buffer,
        width,
        x + 1,
        y,
        w.saturating_sub(2).max(1),
        1,
        color,
    );
    draw_rect(buffer, width, x, y + 1, w, h.saturating_sub(2), color);
    draw_rect(
        buffer,
        width,
        x + 1,
        y + h.saturating_sub(1),
        w.saturating_sub(2).max(1),
        1,
        color,
    );
}

fn draw_rect(buffer: &mut [u32], width: u32, x: u32, y: u32, w: u32, h: u32, color: u32) {
    let buffer_width = width as usize;
    let buffer_len = buffer.len();
    for row in y..y.saturating_add(h) {
        let start = row as usize * buffer_width + x as usize;
        if start >= buffer_len {
            break;
        }
        let end = (start + w as usize).min(buffer_len);
        buffer[start..end].fill(color);
    }
}

fn mix_channel(start: u8, end: u8, amount: f32) -> u8 {
    (start as f32 + (end as f32 - start as f32) * amount).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_colors_with_fallback() {
        let fallback = WaveformColor::new(1, 2, 3);
        assert_eq!(
            parse_hex_color("#b58cff", fallback),
            WaveformColor::new(181, 140, 255)
        );
        assert_eq!(parse_hex_color("invalid", fallback), fallback);
    }

    #[test]
    fn history_is_bounded_and_smoothed() {
        let levels = shared_levels();
        for _ in 0..LEVEL_HISTORY + 10 {
            push_level(&levels, 1.0);
        }
        let snapshot = snapshot(&levels);
        assert_eq!(snapshot.len(), LEVEL_HISTORY);
        assert!(snapshot[0] > 0.0);
        assert!(snapshot.last().copied().unwrap_or_default() <= 1.0);
    }

    fn render_speech(levels: &[f32]) -> Vec<u32> {
        let mut buffer = vec![0; (HUD_WIDTH * HUD_HEIGHT) as usize];
        draw_waveform(
            &mut buffer,
            HUD_WIDTH,
            HUD_HEIGHT,
            levels,
            1.8,
            WaveformMode::Listening,
            WaveformStyle {
                gradient_start: WaveformColor::new(181, 140, 255),
                gradient_end: WaveformColor::new(215, 230, 255),
                rounded_background: false,
                speech_gain: 4.0,
            },
        );
        buffer
    }

    fn column_heights(buffer: &[u32]) -> Vec<usize> {
        (0..HUD_WIDTH as usize)
            .map(|x| {
                (0..HUD_HEIGHT as usize)
                    .filter(|y| buffer[y * HUD_WIDTH as usize + x] & 0x00ff_ffff != 0)
                    .count()
            })
            .collect()
    }

    #[test]
    fn speech_peak_height_survives_growing_history() {
        let mut short = vec![0.0; 12];
        short[8] = 0.08;
        let short_height = column_heights(&render_speech(&short))
            .into_iter()
            .max()
            .unwrap();
        assert!(short_height > 4);
        for peak_index in [0, 1, 8, 17, 89, 178, 179] {
            let mut full = vec![0.0; LEVEL_HISTORY];
            full[peak_index] = 0.08;
            let full_height = column_heights(&render_speech(&full))
                .into_iter()
                .max()
                .unwrap();
            assert_eq!(
                full_height, short_height,
                "peak at history index {peak_index}"
            );
        }
    }

    #[test]
    fn speech_height_does_not_depend_on_horizontal_position() {
        let heights = column_heights(&render_speech(&[0.08; LEVEL_HISTORY]));
        let bar_heights = heights
            .split(|height| *height == 0)
            .filter(|bar| !bar.is_empty())
            .map(|bar| bar.iter().copied().max().unwrap())
            .collect::<Vec<_>>();
        assert!(!bar_heights.is_empty());
        assert_eq!(bar_heights.iter().min(), bar_heights.iter().max());
    }

    #[test]
    fn renderer_draws_transparent_corners_and_colored_bars() {
        let mut buffer = vec![0_u32; (HUD_WIDTH * HUD_HEIGHT) as usize];
        draw_waveform(
            &mut buffer,
            HUD_WIDTH,
            HUD_HEIGHT,
            &[0.5; 32],
            1.8,
            WaveformMode::Listening,
            WaveformStyle {
                gradient_start: WaveformColor::new(181, 140, 255),
                gradient_end: WaveformColor::new(215, 230, 255),
                rounded_background: true,
                speech_gain: 1.0,
            },
        );
        assert_eq!(buffer[0], 0);
        assert!(buffer.contains(&0xf200_0000));
        assert!(buffer.iter().any(|pixel| (*pixel & 0x00ff_ffff) != 0));
    }

    #[test]
    fn retina_preserves_logical_waveform_size_for_quiet_speech_and_processing() {
        fn bounds(buffer: &[u32], width: u32) -> (u32, u32, u32, u32) {
            let mut bounds = (u32::MAX, u32::MAX, 0, 0);
            for (index, pixel) in buffer.iter().enumerate() {
                if pixel & 0x00ff_ffff != 0 {
                    let x = index as u32 % width;
                    let y = index as u32 / width;
                    bounds.0 = bounds.0.min(x);
                    bounds.1 = bounds.1.min(y);
                    bounds.2 = bounds.2.max(x);
                    bounds.3 = bounds.3.max(y);
                }
            }
            assert_ne!(bounds.0, u32::MAX);
            bounds
        }

        let style = WaveformStyle {
            gradient_start: WaveformColor::new(181, 140, 255),
            gradient_end: WaveformColor::new(215, 230, 255),
            rounded_background: false,
            speech_gain: 1.0,
        };
        for mode in [
            WaveformMode::Listening,
            WaveformMode::Processing {
                elapsed_seconds: 0.4,
            },
        ] {
            for level in [0.0, 0.015, 0.08, 0.5] {
                let mut standard = vec![0; (HUD_WIDTH * HUD_HEIGHT) as usize];
                let mut retina = vec![0; (HUD_WIDTH * HUD_HEIGHT * 4) as usize];
                draw_waveform(
                    &mut standard,
                    HUD_WIDTH,
                    HUD_HEIGHT,
                    &[level; 32],
                    1.8,
                    mode,
                    style,
                );
                draw_waveform_with_scale(
                    &mut retina,
                    HUD_WIDTH * 2,
                    HUD_HEIGHT * 2,
                    2.0,
                    &[level; 32],
                    1.8,
                    mode,
                    style,
                );
                let standard = bounds(&standard, HUD_WIDTH);
                let retina = bounds(&retina, HUD_WIDTH * 2);
                assert_eq!(retina.0, standard.0 * 2);
                assert_eq!(retina.2 + 1, (standard.2 + 1) * 2);
                assert!(retina.1.abs_diff(standard.1 * 2) <= 1);
                assert!((retina.3 + 1).abs_diff((standard.3 + 1) * 2) <= 1);
            }
        }
    }

    #[test]
    fn speech_gain_enlarges_speech_but_preserves_silence_processing_and_height_limit() {
        let width = HUD_WIDTH * 2;
        let height = HUD_HEIGHT * 2;
        let render = |level: f32, gain, mode| {
            let mut buffer = vec![0; (width * height) as usize];
            draw_waveform_with_scale(
                &mut buffer,
                width,
                height,
                2.0,
                &[level; 32],
                1.8,
                mode,
                WaveformStyle {
                    gradient_start: WaveformColor::new(181, 140, 255),
                    gradient_end: WaveformColor::new(215, 230, 255),
                    rounded_background: false,
                    speech_gain: gain,
                },
            );
            buffer
        };
        let tallest_bar = |buffer: &[u32]| {
            (0..width as usize)
                .map(|x| {
                    (0..height as usize)
                        .filter(|y| buffer[y * width as usize + x] & 0x00ff_ffff != 0)
                        .count()
                })
                .max()
                .unwrap()
        };
        let silence = render(0.0, 1.0, WaveformMode::Listening);
        assert_eq!(silence, render(0.0, 1.5, WaveformMode::Listening));
        let baseline = tallest_bar(&silence);
        for level in [0.015, 0.025] {
            let old = tallest_bar(&render(level, 1.0, WaveformMode::Listening)) - baseline;
            let boosted = tallest_bar(&render(level, 1.5, WaveformMode::Listening)) - baseline;
            assert!(boosted > old);
            assert!((boosted as f32 - old as f32 * 1.5).abs() <= 1.5);
        }
        let processing = WaveformMode::Processing {
            elapsed_seconds: 0.4,
        };
        assert_eq!(render(0.08, 1.0, processing), render(0.08, 1.5, processing));
        assert!(
            tallest_bar(&render(1.0, 1.5, WaveformMode::Listening)) <= WAVEFORM_HEIGHT as usize * 2
        );
    }

    #[test]
    fn processing_ripple_moves_from_left_to_right() {
        let elapsed = 0.35;
        let delta = 0.2;
        let progress = 0.65;
        let distance = delta / PROCESSING_CYCLE_SECONDS / PROCESSING_WAVE_COUNT;
        let later = processing_ripple(progress, elapsed + delta);
        let earlier_to_the_left = processing_ripple(progress - distance, elapsed);
        assert!((later - earlier_to_the_left).abs() < 0.0001);
    }

    #[test]
    fn processing_ripple_uses_the_full_width() {
        for index in 0..100 {
            assert!(processing_ripple(index as f32 / 99.0, 0.4) >= 0.14);
        }
    }

    #[test]
    fn empty_listening_state_stays_at_rest() {
        let mut buffer = vec![0_u32; (HUD_WIDTH * HUD_HEIGHT) as usize];
        draw_waveform(
            &mut buffer,
            HUD_WIDTH,
            HUD_HEIGHT,
            &[],
            1.8,
            WaveformMode::Listening,
            WaveformStyle {
                gradient_start: WaveformColor::new(181, 140, 255),
                gradient_end: WaveformColor::new(215, 230, 255),
                rounded_background: true,
                speech_gain: 1.0,
            },
        );
        let colored_pixels = buffer
            .iter()
            .filter(|pixel| (**pixel & 0x00ff_ffff) != 0)
            .count();
        assert!(colored_pixels < 2_000);
    }
}

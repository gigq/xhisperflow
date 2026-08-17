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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WaveformStyle {
    pub gradient_start: WaveformColor,
    pub gradient_end: WaveformColor,
    pub rounded_background: bool,
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
    let width = width.max(1);
    let height = height.max(1);
    buffer.fill(0);

    if style.rounded_background {
        draw_rounded_background(buffer, width, height, 18);
    } else {
        buffer.fill(0xff00_0000);
    }

    let waveform_bottom = height.saturating_sub(WAVEFORM_BOTTOM_PADDING).max(1);
    let waveform_top = waveform_bottom.saturating_sub(WAVEFORM_HEIGHT);
    let center = waveform_top + (waveform_bottom.saturating_sub(waveform_top) / 2);

    let left = 42_u32;
    let right = width.saturating_sub(42);
    let bar_width = 3_u32;
    let gap = 5_u32;
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
        let (level, taper) = match mode {
            WaveformMode::Listening => {
                let raw_level = if levels.is_empty() {
                    0.0
                } else {
                    let idx = (bar_index as usize * levels.len() / bar_count as usize)
                        .min(levels.len().saturating_sub(1));
                    (levels[idx].sqrt() * response).clamp(0.0, 1.0)
                };
                let distance_from_center = ((progress - 0.5).abs() * 2.0).clamp(0.0, 1.0);
                (
                    shape_waveform_level(raw_level),
                    1.0 - distance_from_center * 0.62,
                )
            }
            WaveformMode::Processing { elapsed_seconds } => {
                (processing_ripple(progress, elapsed_seconds), 1.0)
            }
        };
        let bar_height =
            (4.0 + level * taper * (WAVEFORM_HEIGHT.saturating_sub(4)) as f32).round() as u32;
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
            },
        );
        assert_eq!(buffer[0], 0);
        assert!(buffer.contains(&0xf200_0000));
        assert!(buffer.iter().any(|pixel| (*pixel & 0x00ff_ffff) != 0));
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
            },
        );
        let colored_pixels = buffer
            .iter()
            .filter(|pixel| (**pixel & 0x00ff_ffff) != 0)
            .count();
        assert!(colored_pixels < 2_000);
    }
}

//! Desktop clock text and its shadows, flattened into one brush per line.

use anyhow::Result;
use windows::Win32::Graphics::{
    Direct2D::{
        CLSID_D2D1Shadow, Common::D2D1_COMPOSITE_MODE_SOURCE_OVER, D2D1_INTERPOLATION_MODE_LINEAR,
        D2D1_PROPERTY_TYPE_FLOAT, D2D1_PROPERTY_TYPE_VECTOR4,
        D2D1_SHADOW_PROP_BLUR_STANDARD_DEVIATION, D2D1_SHADOW_PROP_COLOR,
        D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
    },
    DirectWrite::{
        DWRITE_FONT_FEATURE, DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES, DWRITE_TEXT_RANGE,
        IDWriteTextLayout1,
    },
};

use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClockLine {
    Date,
    Time,
    Weather,
}

impl ClockLine {
    pub const fn height(self) -> f32 {
        match self {
            Self::Date => 34.5,
            Self::Time => 184.0 * 1.02,
            Self::Weather => 30.0,
        }
    }

    fn font_size(self) -> f32 {
        match self {
            Self::Date => 23.0,
            Self::Time => 184.0,
            Self::Weather => 20.0,
        }
    }

    pub const fn margin(self) -> f32 {
        match self {
            Self::Time => 76.0,
            Self::Date | Self::Weather => 36.0,
        }
    }

    fn shadows(self) -> &'static [(f32, f32, f32)] {
        // (vertical offset, CSS blur radius, opacity). Longer shadows are
        // painted first so the closer contours stay defined.
        match self {
            Self::Time => &[(0.0, 2.0, 0.4), (8.0, 44.0, 0.42), (2.0, 5.0, 0.28)],
            _ => &[
                (0.0, 22.0, 0.25),
                (0.0, 12.0, 0.45),
                (1.0, 4.0, 0.6),
                (0.0, 3.0, 0.7),
                (0.0, 1.5, 0.9),
            ],
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClockSpan {
    Normal,
    Strong,
    Muted,
    Symbol,
}

impl Gfx {
    pub(crate) fn clock_line(
        &self,
        line: ClockLine,
        spans: &[(&str, ClockSpan)],
        scale: f32,
        max_width: f32,
    ) -> Result<LabelBrush> {
        let margin = line.margin() * scale;
        let line_height = line.height() * scale;
        let mut width = 0.0;
        let mut layouts = Vec::with_capacity(spans.len());
        for &(text, style) in spans {
            let gap = if width > 0.0 { 12.0 * scale } else { 0.0 };
            let available = (max_width - 2.0 * margin - width - gap).floor();
            if available < 2.0 {
                break;
            }
            width += gap;
            let symbol = style == ClockSpan::Symbol;
            let size = if symbol { 22.0 } else { line.font_size() } * scale;
            let family = if symbol {
                w!("Segoe UI Symbol")
            } else if muelle_shell::system::is_windows_11() {
                w!("Segoe UI Variable Display")
            } else {
                w!("Segoe UI")
            };
            let weight = if line == ClockLine::Time || style == ClockSpan::Strong {
                DWRITE_FONT_WEIGHT_SEMI_BOLD
            } else {
                DWRITE_FONT_WEIGHT_MEDIUM
            };
            let utf16 = text.encode_utf16().collect::<Vec<_>>();
            // SAFETY: These layouts are used only on the graphics thread.
            unsafe {
                let format = self.dwrite.CreateTextFormat(
                    family,
                    None,
                    weight,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    size,
                    w!(""),
                )?;
                format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
                let layout =
                    self.dwrite
                        .CreateTextLayout(&utf16, &format, available, size * 2.0)?;
                if line == ClockLine::Time {
                    let range = DWRITE_TEXT_RANGE {
                        startPosition: 0,
                        length: utf16.len() as u32,
                    };
                    layout.cast::<IDWriteTextLayout1>()?.SetCharacterSpacing(
                        -2.5 * scale,
                        -2.5 * scale,
                        0.0,
                        range,
                    )?;
                    let typography = self.dwrite.CreateTypography()?;
                    typography.AddFontFeature(DWRITE_FONT_FEATURE {
                        nameTag: DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES,
                        parameter: 1,
                    })?;
                    layout.SetTypography(&typography, range)?;
                } else {
                    let ellipsis = self.dwrite.CreateEllipsisTrimmingSign(&format)?;
                    layout.SetTrimming(
                        &DWRITE_TRIMMING {
                            granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                            ..Default::default()
                        },
                        &ellipsis,
                    )?;
                }
                let mut metrics = DWRITE_TEXT_METRICS::default();
                layout.GetMetrics(&mut metrics)?;
                let span_width = if symbol {
                    (22.0 * scale).min(available)
                } else {
                    metrics.width.min(available)
                };
                let opacity = if style == ClockSpan::Muted { 0.85 } else { 1.0 };
                layouts.push((
                    layout,
                    Vector2 {
                        X: margin + width + (span_width - metrics.width) * 0.5,
                        Y: margin + (line_height - metrics.height) * 0.5,
                    },
                    opacity,
                ));
                width += span_width;
            }
        }

        let width = (width + 2.0 * margin).ceil();
        let height = (line_height + 2.0 * margin).ceil();
        let brush = self.draw(width as u32, height as u32, |dc| {
            // SAFETY: The destination and transform are restored before the
            // list is finalized and before Gfx ends the surface draw.
            unsafe {
                let target = dc.GetTarget()?;
                let mut transform = Matrix3x2::identity();
                dc.GetTransform(&mut transform);
                let antialias = dc.GetTextAntialiasMode();
                let text = dc.CreateCommandList()?;
                let white = dc.CreateSolidColorBrush(&WHITE, None)?;
                dc.SetTarget(&text);
                dc.SetTransform(&Matrix3x2::identity());
                dc.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
                for (layout, position, opacity) in &layouts {
                    white.SetOpacity(*opacity);
                    dc.DrawTextLayout(*position, layout, &white, D2D1_DRAW_TEXT_OPTIONS_NONE);
                }
                dc.SetTarget(&target);
                dc.SetTransform(&transform);
                dc.SetTextAntialiasMode(antialias);
                text.Close()?;

                for &(dy, blur, alpha) in line.shadows() {
                    let shadow = dc.CreateEffect(&CLSID_D2D1Shadow)?;
                    shadow.SetInput(0, &text, true);
                    shadow.SetValue(
                        D2D1_SHADOW_PROP_BLUR_STANDARD_DEVIATION.0 as u32,
                        D2D1_PROPERTY_TYPE_FLOAT,
                        &(blur * scale * 0.5).to_ne_bytes(),
                    )?;
                    let mut black = [0; 16];
                    black[12..].copy_from_slice(&alpha.to_ne_bytes());
                    shadow.SetValue(
                        D2D1_SHADOW_PROP_COLOR.0 as u32,
                        D2D1_PROPERTY_TYPE_VECTOR4,
                        &black,
                    )?;
                    let offset = Vector2 {
                        X: 0.0,
                        Y: dy * scale,
                    };
                    dc.DrawImage(
                        &shadow.GetOutput()?,
                        Some(&offset),
                        None,
                        D2D1_INTERPOLATION_MODE_LINEAR,
                        D2D1_COMPOSITE_MODE_SOURCE_OVER,
                    );
                }
                dc.DrawImage(
                    &text,
                    None,
                    None,
                    D2D1_INTERPOLATION_MODE_LINEAR,
                    D2D1_COMPOSITE_MODE_SOURCE_OVER,
                );
            }
            Ok(())
        })?;
        Ok(LabelBrush {
            brush,
            width,
            height,
        })
    }
}

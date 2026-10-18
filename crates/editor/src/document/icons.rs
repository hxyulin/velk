//! Small stroked chrome icons, painted in the element's text color.
use rxui::astrelis::{LineCap, LineDraw, PaintSession, Rect, Stroke};
use rxui::{CustomElement, Element, ElementInfo, UiError};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Icon {
    Files,
    Search,
    Terminal,
    Settings,
    Close,
}
impl Icon {
    /// A best guess from a plugin panel's title; plugins send no icons.
    pub fn for_panel(title: &str) -> Option<Self> {
        let title = title.to_lowercase();
        [
            ("file", Self::Files),
            ("explor", Self::Files),
            ("search", Self::Search),
            ("find", Self::Search),
            ("term", Self::Terminal),
        ]
        .into_iter()
        .find_map(|(word, icon)| title.contains(word).then_some(icon))
    }
    pub fn element(self, size: f32) -> Element {
        rxui::custom(self).size(size, size).flex_shrink(0.)
    }
}
impl CustomElement for Icon {
    type State = ();
    fn paint(
        &self,
        _: &(),
        element: &ElementInfo<'_>,
        paint: &mut PaintSession<'_, '_>,
    ) -> Result<(), UiError> {
        // Coordinates use a 24-unit grid, like common icon sets.
        let b = element.content_bounds;
        let scale = b.width.min(b.height) / 24.;
        let at = |x: f32, y: f32| [b.x + x * scale, b.y + y * scale];
        let color = element.color;
        let width = (1.7 * scale).max(1.);
        let lines =
            |paint: &mut PaintSession<'_, '_>, points: &[(f32, f32)]| -> Result<(), UiError> {
                for pair in points.windows(2) {
                    paint.draw_line(LineDraw {
                        width,
                        cap: LineCap::Round,
                        ..LineDraw::new(at(pair[0].0, pair[0].1), at(pair[1].0, pair[1].1), color)
                    })?;
                }
                Ok(())
            };
        match self {
            Self::Files => {
                lines(
                    paint,
                    &[
                        (14., 3.),
                        (6., 3.),
                        (6., 21.),
                        (18., 21.),
                        (18., 7.),
                        (14., 3.),
                    ],
                )?;
                lines(paint, &[(14., 3.), (14., 7.), (18., 7.)])?;
            }
            Self::Search => {
                lines(paint, &[(15.6, 15.6), (20., 20.)])?;
                circle(paint, at(11., 11.), 6.5 * scale, width, color)?;
            }
            Self::Terminal => {
                lines(paint, &[(5., 7.), (10., 12.), (5., 17.)])?;
                lines(paint, &[(12., 17.), (19., 17.)])?;
            }

            Self::Settings => {
                for i in 0..8 {
                    let (sin, cos) = (i as f32 * std::f32::consts::FRAC_PI_4).sin_cos();
                    lines(
                        paint,
                        &[
                            (12. + 7. * cos, 12. + 7. * sin),
                            (12. + 9.5 * cos, 12. + 9.5 * sin),
                        ],
                    )?;
                }
                circle(paint, at(12., 12.), 7. * scale, width, color)?;
                circle(paint, at(12., 12.), 2.8 * scale, width, color)?;
            }
            Self::Close => {
                lines(paint, &[(7., 7.), (17., 17.)])?;
                lines(paint, &[(17., 7.), (7., 17.)])?;
            }
        }
        Ok(())
    }
}
fn circle(
    paint: &mut PaintSession<'_, '_>,
    center: [f32; 2],
    radius: f32,
    width: f32,
    color: [f32; 4],
) -> Result<(), UiError> {
    Ok(paint.stroke_ellipse(
        Rect::new(
            center[0] - radius,
            center[1] - radius,
            radius * 2.,
            radius * 2.,
        ),
        Stroke::new(width),
        color,
    )?)
}

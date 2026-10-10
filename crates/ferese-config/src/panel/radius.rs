use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Corner radii, clockwise from top-left, in logical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CornerRadii(pub [f32; 4]);

impl CornerRadii {
    pub fn uniform(radius: f32) -> Self {
        Self([radius; 4])
    }

    pub fn validate(self) -> Result<(), String> {
        if self.0.iter().all(|value| value.is_finite() && *value >= 0.) {
            Ok(())
        } else {
            Err("corner radii must be finite, nonnegative pixel values".into())
        }
    }

    /// A panel touching the top edge must cover its top corners.
    pub fn at_top_edge(mut self, touching: bool) -> Self {
        if touching {
            self.0[0] = 0.;
            self.0[1] = 0.;
        }
        self
    }

    pub fn max(self) -> f32 {
        self.0.into_iter().fold(0., f32::max)
    }

    /// Keep every corner within half the shorter surface dimension.
    pub fn clamped(self, width: f32, height: f32) -> Self {
        let limit = width.min(height).max(0.) / 2.;
        Self(self.0.map(|value| value.min(limit)))
    }
}

impl std::str::FromStr for CornerRadii {
    type Err = String;

    fn from_str(source: &str) -> Result<Self, Self::Err> {
        let values: Vec<f32> = source
            .split_whitespace()
            .map(|part| {
                let number = if part == "0" {
                    part
                } else {
                    part.strip_suffix("px").ok_or("use pixel values such as 4px")?
                };
                number.parse::<f32>().map_err(|_| "invalid corner radius")
            })
            .collect::<Result<_, _>>()
            .map_err(str::to_owned)?;
        let radii = match values.as_slice() {
            [a] => [*a; 4],
            [a, b] => [*a, *b, *a, *b],
            [a, b, c] => [*a, *b, *c, *b],
            [a, b, c, d] => [*a, *b, *c, *d],
            _ => return Err("enter one to four pixel values, clockwise from top-left".into()),
        };
        let result = Self(radii);
        result.validate()?;
        Ok(result)
    }
}

impl std::fmt::Display for CornerRadii {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let [a, b, c, d] = self.0;
        if a == b && b == c && c == d {
            write!(f, "{a}px")
        } else if a == c && b == d {
            write!(f, "{a}px {b}px")
        } else if b == d {
            write!(f, "{a}px {b}px {c}px")
        } else {
            write!(f, "{a}px {b}px {c}px {d}px")
        }
    }
}

impl Serialize for CornerRadii {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for CornerRadii {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn css_shorthand_expands_clockwise_and_round_trips() {
        for (source, expected) in [
            ("4px", [4.; 4]),
            ("4px 8px", [4., 8., 4., 8.]),
            ("4px 8px 12px", [4., 8., 12., 8.]),
            ("4px 8px 12px 16px", [4., 8., 12., 16.]),
            ("0 2.5px", [0., 2.5, 0., 2.5]),
        ] {
            let radii: CornerRadii = source.parse().unwrap();
            assert_eq!(radii.0, expected);
            let value = serde_json::to_value(radii).unwrap();
            assert_eq!(serde_json::from_value::<CornerRadii>(value).unwrap(), radii);
        }
    }

    #[test]
    fn docking_squares_only_the_top_corners_without_changing_the_override() {
        let radii = CornerRadii([4., 8., 12., 16.]);
        assert_eq!(radii.at_top_edge(true).0, [0., 0., 12., 16.]);
        assert_eq!(radii.at_top_edge(false), radii);
        assert_eq!(radii.0, [4., 8., 12., 16.]);
    }

    #[test]
    fn invalid_values_are_rejected_and_large_radii_clamp_to_surface_size() {
        for source in [
            "",
            "4",
            "4%",
            "-4px",
            "NaNpx",
            "infpx",
            "4px / 8px",
            "1px 2px 3px 4px 5px",
        ] {
            assert!(source.parse::<CornerRadii>().is_err(), "{source}");
        }
        assert_eq!(CornerRadii([4., 40., 0., 18.]).clamped(100., 28.).0, [4., 14., 0., 14.]);
    }
}

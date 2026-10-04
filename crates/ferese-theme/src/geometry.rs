/// Radius of a circular inset. Squircle insets retain an `Outline` and its distance offset.
pub fn inner_radius(outer: f32, padding: f32) -> f32 {
    (outer - padding).max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inset_corners_preserve_the_center_and_saturate() {
        assert_eq!(inner_radius(14.0, 4.0), 10.0);
        assert_eq!(inner_radius(7.5, 2.25), 5.25);
        assert_eq!(inner_radius(14.0, 0.0), 14.0);
        assert_eq!(inner_radius(14.0, 14.0), 0.0);
        assert_eq!(inner_radius(14.0, 20.0), 0.0);
        assert_eq!(inner_radius(0.0, 4.0), 0.0);
    }
}

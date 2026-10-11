use crate::recording::Crop;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Region {
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) width: i32,
    pub(crate) height: i32,
}

impl Region {
    pub(crate) fn parse(value: &str) -> Result<Self, String> {
        let (position, size) = value.trim().split_once(' ').ok_or("Invalid region selection")?;
        let (x, y) = position.split_once(',').ok_or("Invalid region position")?;
        let (width, height) = size.split_once('x').ok_or("Invalid region size")?;
        let number = |value: &str| value.parse::<i32>().map_err(|_| "Invalid region coordinate".to_owned());
        let region = Self {
            x: number(x)?,
            y: number(y)?,
            width: number(width)?,
            height: number(height)?,
        };

        if region.width <= 0
            || region.height <= 0
            || region.x.checked_add(region.width).is_none()
            || region.y.checked_add(region.height).is_none()
        {
            return Err("Invalid region bounds".into());
        }

        Ok(region)
    }

    pub(crate) fn crop(self, output: Self, physical: (u32, u32)) -> Result<Crop, String> {
        let x = i64::from(self.x) - i64::from(output.x);
        let y = i64::from(self.y) - i64::from(output.y);
        let end_x = x + i64::from(self.width);
        let end_y = y + i64::from(self.height);

        if self.width <= 0
            || self.height <= 0
            || output.width <= 0
            || output.height <= 0
            || x < 0
            || y < 0
            || end_x > i64::from(output.width)
            || end_y > i64::from(output.height)
            || physical.0 == 0
            || physical.1 == 0
            || physical.0 > i32::MAX as u32
            || physical.1 > i32::MAX as u32
        {
            return Err("Select a region entirely inside the chosen display".into());
        }
        // Round inward so pixels outside the selected logical rectangle never
        // enter the recording, including at fractional output scales.
        let near = |value: i64, pixels: u32, logical: i32| {
            (value * i64::from(pixels) + i64::from(logical) - 1) / i64::from(logical)
        };
        let far = |value: i64, pixels: u32, logical: i32| value * i64::from(pixels) / i64::from(logical);
        let left = near(x, physical.0, output.width);
        let top = near(y, physical.1, output.height);
        let end_x = far(end_x, physical.0, output.width);
        let end_y = far(end_y, physical.1, output.height);

        if end_x <= left || end_y <= top {
            return Err("The selected region is smaller than one output pixel".into());
        }

        Ok(Crop {
            left: left as i32,
            top: top as i32,
            right: (i64::from(physical.0) - end_x) as i32,
            bottom: (i64::from(physical.1) - end_y) as i32,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn region_mapping_handles_negative_origins_and_fractional_scales() {
        let output = Region::parse("-1920,20 1920x1080").unwrap();
        let selected = Region::parse("-1919,21 100x50").unwrap();
        assert_eq!(
            selected.crop(output, (2400, 1350)).unwrap(),
            Crop {
                left: 2,
                top: 2,
                right: 2274,
                bottom: 1287
            }
        );
        assert_eq!(
            output.crop(output, (2880, 1620)).unwrap(),
            Crop {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0
            }
        );
        assert!(
            Region::parse("-1921,20 100x50")
                .unwrap()
                .crop(output, (2400, 1350))
                .is_err()
        );
        assert!(
            Region::parse("-100,20 200x50")
                .unwrap()
                .crop(output, (2400, 1350))
                .is_err()
        );
    }

    #[test]
    fn region_selection_rejects_empty_overflow_and_malformed_bounds() {
        for value in ["", "1,2 0x20", "1,2 -1x20", "2147483647,0 2x20", "1,2 20x30 extra"] {
            assert!(Region::parse(value).is_err(), "{value}");
        }
        assert!(
            Crop {
                left: -1,
                top: 0,
                right: 0,
                bottom: 0
            }
            .validate()
            .is_err()
        );
    }
}

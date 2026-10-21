//! Gamma ramps shared by display backends and the gamma-control protocol.
use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;

// Keep client-controlled allocation bounded even if a driver reports an invalid size.
const MAX_GAMMA_SIZE: usize = 65_536;

#[derive(Clone, PartialEq)]
pub(crate) struct Ramp(Vec<u16>);

impl Ramp {
    pub(crate) fn zeroed(size: usize) -> io::Result<Self> {
        if size == 0 || size > MAX_GAMMA_SIZE {
            return Err(io::ErrorKind::Unsupported.into());
        }
        Ok(Self(vec![0; size * 3]))
    }

    pub(crate) fn size(&self) -> usize {
        self.0.len() / 3
    }

    pub(crate) fn channels_mut(&mut self) -> (&mut [u16], &mut [u16], &mut [u16]) {
        let size = self.size();
        let (red, rest) = self.0.split_at_mut(size);
        let (green, blue) = rest.split_at_mut(size);
        (red, green, blue)
    }

    pub(crate) fn at_temperature(&self, temperature: u16) -> Self {
        let mut ramp = self.clone();
        for (channel, gain) in ramp
            .0
            .chunks_exact_mut(self.size())
            .zip(temperature_gains(temperature))
        {
            for value in channel {
                *value = (f64::from(*value) * gain).round() as u16;
            }
        }
        ramp
    }

    pub(crate) fn interpolate(&self, target: &Self, progress: f64) -> Self {
        assert_eq!(self.size(), target.size());
        let progress = progress.clamp(0., 1.);
        Self(
            self.0
                .iter()
                .zip(&target.0)
                .map(|(&from, &to)| {
                    (f64::from(from) + (f64::from(to) - f64::from(from)) * progress).round() as u16
                })
                .collect(),
        )
    }

    pub(crate) fn read(file: File, size: usize) -> io::Result<Self> {
        let metadata = file.metadata()?;
        let length = size
            .checked_mul(6)
            .filter(|_| size > 0 && size <= MAX_GAMMA_SIZE);
        let Some(length) = length else {
            return Err(io::ErrorKind::InvalidData.into());
        };
        // pread preserves the caller's offset and avoids mmap's truncation SIGBUS.
        // Only fixed-size files are accepted, not pipes or device streams.
        if !metadata.is_file() || metadata.len() != length as u64 {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let mut bytes = vec![0; length];
        file.read_exact_at(&mut bytes, 0)?;
        Ok(Self(
            bytes
                .chunks_exact(2)
                .map(|v| u16::from_ne_bytes([v[0], v[1]]))
                .collect(),
        ))
    }

    pub(crate) fn channels(&self) -> (&[u16], &[u16], &[u16]) {
        let size = self.0.len() / 3;
        (
            &self.0[..size],
            &self.0[size..2 * size],
            &self.0[2 * size..],
        )
    }
}

fn temperature_gains(kelvin: u16) -> [f64; 3] {
    // Tanner Helland's temperature approximation, normalized to neutral at 6500 K.
    // https://tannerhelland.com/2012/09/18/convert-temperature-rgb-algorithm-code.html
    let green = |t: f64| (99.4708025861 * t.ln() - 161.1195681661).clamp(0., 255.);
    let blue = |t: f64| {
        if t <= 19. {
            0.
        } else {
            (138.5177312231 * (t - 10.).ln() - 305.0447927307).clamp(0., 255.)
        }
    };
    let temperature = f64::from(kelvin.clamp(1000, 6500)) / 100.;
    [
        1.,
        green(temperature) / green(65.),
        blue(temperature) / blue(65.),
    ]
}

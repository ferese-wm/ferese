use std::path::Path;

use image::{ColorType, ImageDecoder};
use memmap2::MmapMut;

pub(super) type Pixels = image::ImageBuffer<image::Rgba<u8>, MmapMut>;

const DECODE_LIMIT: u64 = 256 * 1024 * 1024;

pub(super) fn load(path: &Path) -> Result<Pixels, String> {
    let result = decode(path);

    // Decoders can leave large freed scratch buffers in glibc arenas. Reclaim
    // them here, after decode has dropped its temporaries, on the worker only.
    // The returned pixels have their own mapping and are unaffected by trimming.
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    unsafe {
        libc::malloc_trim(0);
    }

    result
}

fn decode(path: &Path) -> Result<Pixels, String> {
    let mut reader = image::ImageReader::open(path)
        .map_err(|error| error.to_string())?
        .with_guessed_format()
        .map_err(|error| error.to_string())?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(DECODE_LIMIT);
    reader.limits(limits);
    let decoder = reader.into_decoder().map_err(|error| error.to_string())?;
    let (width, height) = decoder.dimensions();
    let bytes = rgba_bytes(width, height)?;

    // Give both the retained pixels and RGB conversion scratch their own
    // mappings. Dropping either releases its pages even when another thread
    // owns the image, rather than leaving them in the decoder thread's arena.
    match decoder.color_type() {
        ColorType::Rgba8 => {
            let mut data = MmapMut::map_anon(bytes).map_err(|error| error.to_string())?;
            decoder.read_image(&mut data).map_err(|error| error.to_string())?;
            mapped_image(width, height, data)
        }

        ColorType::Rgb8 => {
            let mut rgb = MmapMut::map_anon(bytes / 4 * 3).map_err(|error| error.to_string())?;
            decoder.read_image(&mut rgb).map_err(|error| error.to_string())?;
            let mut rgba = MmapMut::map_anon(bytes).map_err(|error| error.to_string())?;
            for (source, target) in rgb.chunks_exact(3).zip(rgba.chunks_exact_mut(4)) {
                target[..3].copy_from_slice(source);
                target[3] = 255;
            }

            mapped_image(width, height, rgba)
        }

        _ => {
            let decoded = image::DynamicImage::from_decoder(decoder).map_err(|error| error.to_string())?;
            from_rgba(decoded.into_rgba8())
        }
    }
}

fn rgba_bytes(width: u32, height: u32) -> Result<usize, String> {
    let pixels = u64::from(width) * u64::from(height);
    if pixels == 0 || pixels > DECODE_LIMIT / 4 {
        return Err("wallpaper must be nonempty and within the 256 MiB decode limit".into());
    }

    Ok(pixels as usize * 4)
}

pub(super) fn from_rgba(image: image::RgbaImage) -> Result<Pixels, String> {
    let (width, height) = image.dimensions();
    let mut data = MmapMut::map_anon(rgba_bytes(width, height)?).map_err(|error| error.to_string())?;
    data.copy_from_slice(image.as_raw());
    mapped_image(width, height, data)
}

fn mapped_image(width: u32, height: u32, data: MmapMut) -> Result<Pixels, String> {
    Pixels::from_raw(width, height, data).ok_or_else(|| "invalid wallpaper pixel dimensions".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mapped_pixels_match_existing_decoder_for_png_jpeg_and_webp() {
        use image::{DynamicImage, ImageBuffer, ImageFormat, Luma, LumaA, Rgb, Rgba};
        let directory = tempfile::tempdir().unwrap();
        let images = [
            DynamicImage::ImageRgb8(ImageBuffer::from_pixel(7, 5, Rgb([31, 81, 159]))),
            DynamicImage::ImageRgba8(ImageBuffer::from_pixel(7, 5, Rgba([31, 81, 159, 127]))),
            DynamicImage::ImageLuma8(ImageBuffer::from_pixel(7, 5, Luma([81]))),
            DynamicImage::ImageLumaA8(ImageBuffer::from_pixel(7, 5, LumaA([81, 127]))),
            DynamicImage::ImageRgb16(ImageBuffer::from_pixel(7, 5, Rgb([8000, 21000, 41000]))),
            DynamicImage::ImageRgba16(ImageBuffer::from_pixel(7, 5, Rgba([8000, 21000, 41000, 32000]))),
        ];
        for (index, image) in images.iter().enumerate() {
            let path = directory.path().join(format!("image-{index}.png"));
            image.save(&path).unwrap();
            let expected = image::open(&path).unwrap().into_rgba8();
            let actual = load(&path).unwrap();
            assert_eq!(actual.dimensions(), expected.dimensions());
            assert_eq!(&actual.as_raw()[..], expected.as_raw());
        }

        for format in [ImageFormat::Jpeg, ImageFormat::WebP] {
            let path = directory.path().join("without-extension");
            images[0].save_with_format(&path, format).unwrap();
            let expected = image::ImageReader::open(&path)
                .unwrap()
                .with_guessed_format()
                .unwrap()
                .decode()
                .unwrap()
                .into_rgba8();
            let actual = load(&path).unwrap();
            assert_eq!(&actual.as_raw()[..], expected.as_raw());
        }
    }

    #[test]
    fn empty_and_oversized_dimensions_are_rejected_without_overflow() {
        assert!(rgba_bytes(0, 1).is_err());
        assert!(rgba_bytes(u32::MAX, u32::MAX).is_err());
        assert!(rgba_bytes(8192, 8193).is_err());
        assert_eq!(rgba_bytes(8192, 8192).unwrap(), DECODE_LIMIT as usize);
    }

    #[test]
    fn corrupt_file_does_not_publish_pixels() {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), b"not an image").unwrap();
        assert!(load(file.path()).is_err());
    }

    #[test]
    #[ignore = "isolated process RSS regression; run in release with --ignored"]
    fn repeated_wallpaper_loads_release_resident_memory() {
        const CHILD: &str = "FERESE_WALLPAPER_MEMORY_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "wallpaper::pixels::tests::repeated_wallpaper_loads_release_resident_memory",
                    "--ignored",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            println!("{}", String::from_utf8_lossy(&output.stdout));
            assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
            return;
        }

        fn rss_kib() -> usize {
            std::fs::read_to_string("/proc/self/status")
                .unwrap()
                .lines()
                .find(|line| line.starts_with("VmRSS:"))
                .unwrap()
                .split_whitespace()
                .nth(1)
                .unwrap()
                .parse()
                .unwrap()
        }

        let paths = [
            Path::new(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../assets/wallpapers/ferese-wallpaper-dark.jpg"
            )),
            Path::new(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../assets/wallpapers/ferese-wallpaper-light.png"
            )),
        ];
        drop(std::thread::spawn(move || load(paths[0]).unwrap()).join().unwrap());
        let baseline = rss_kib();
        let mut current = None;
        for index in 0..20 {
            // Transfer ownership out of the decoder thread, as WallpaperState does.
            let path = paths[index % paths.len()];
            current = Some(std::thread::spawn(move || load(path).unwrap()).join().unwrap());
        }

        let with_wallpaper = rss_kib();
        drop(current);
        let released = rss_kib();
        println!("RSS KiB: baseline={baseline}, wallpaper={with_wallpaper}, released={released}");
        assert!(
            released <= baseline + 16 * 1024,
            "repeated decoder threads retained {} KiB after all pixels were dropped",
            released.saturating_sub(baseline)
        );
    }
}

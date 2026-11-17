use std::{env, fs, path::PathBuf};

fn make_icon(png_path: &str, output: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    let source = image::open(png_path)?.into_rgba8();
    let side = source.width().min(source.height());
    let square = image::imageops::crop_imm(
        &source,
        (source.width() - side) / 2,
        (source.height() - side) / 2,
        side,
        side,
    )
    .to_image();
    let sizes = [16u32, 24, 32, 48, 64, 128, 256];
    let mut png_images = Vec::with_capacity(sizes.len());

    for size in sizes {
        let rgba =
            image::imageops::resize(&square, size, size, image::imageops::FilterType::Lanczos3)
                .into_raw();

        let mut image = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut image, size, size);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder.set_compression(png::Compression::High);
            let mut writer = encoder.write_header()?;
            writer.write_image_data(&rgba)?;
        }
        png_images.push((size, image));
    }

    let mut ico = Vec::new();
    ico.extend_from_slice(&0u16.to_le_bytes()); // reserved
    ico.extend_from_slice(&1u16.to_le_bytes()); // icon resource
    ico.extend_from_slice(&(png_images.len() as u16).to_le_bytes());
    let directory_size = 6 + png_images.len() * 16;
    let mut offset = directory_size as u32;
    for (size, image) in &png_images {
        ico.push(if *size == 256 { 0 } else { *size as u8 });
        ico.push(if *size == 256 { 0 } else { *size as u8 });
        ico.push(0); // palette size: true color
        ico.push(0); // reserved
        ico.extend_from_slice(&1u16.to_le_bytes());
        ico.extend_from_slice(&32u16.to_le_bytes());
        ico.extend_from_slice(&(image.len() as u32).to_le_bytes());
        ico.extend_from_slice(&offset.to_le_bytes());
        offset += image.len() as u32;
    }
    for (_, image) in png_images {
        ico.extend_from_slice(&image);
    }
    fs::write(output, ico)?;
    Ok(())
}

fn main() {
    println!("cargo:rerun-if-changed=assets/jkeeper.png");
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is set by Cargo"));
    let icon = out_dir.join("jkeeper.ico");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        make_icon("assets/jkeeper.png", &icon).expect("failed to generate jkeeper.ico");
    }
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .windows_attributes(tauri_build::WindowsAttributes::new().window_icon_path(icon)),
    )
    .expect("failed to run tauri-build");
}

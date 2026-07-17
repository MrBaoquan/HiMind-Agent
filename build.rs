use std::fs;
use std::path::Path;

fn main() {
    let icon_path = Path::new("icons/icon.ico");
    if !icon_path.exists() {
        fs::create_dir_all("icons").expect("failed to create icons dir");
        fs::write(icon_path, generate_ico()).expect("failed to write icon.ico");
    }
    tauri_build::build();
}

fn generate_ico() -> Vec<u8> {
    let w: u32 = 32;
    let h: u32 = 32;
    let pixel_bytes = (w * h * 4) as usize;
    let bmp_header_size: usize = 40;
    let and_mask_row = ((w + 31) / 32 * 4) as usize;
    let and_mask_size = and_mask_row * h as usize;
    let image_data_size = bmp_header_size + pixel_bytes + and_mask_size;
    let data_offset: u32 = 6 + 16;

    let mut buf = Vec::with_capacity(data_offset as usize + image_data_size);

    // ICO header
    buf.extend_from_slice(&[0, 0]); // reserved
    buf.extend_from_slice(&[1, 0]); // type = icon
    buf.extend_from_slice(&[1, 0]); // count = 1

    // directory entry
    buf.push(w as u8); // width
    buf.push(h as u8); // height
    buf.push(0); // color count
    buf.push(0); // reserved
    buf.extend_from_slice(&[1, 0]); // planes
    buf.extend_from_slice(&[32, 0]); // bpp
    buf.extend_from_slice(&(image_data_size as u32).to_le_bytes());
    buf.extend_from_slice(&data_offset.to_le_bytes());

    // BMP info header
    buf.extend_from_slice(&40u32.to_le_bytes());
    buf.extend_from_slice(&(w as i32).to_le_bytes());
    buf.extend_from_slice(&((h * 2) as i32).to_le_bytes()); // ICO doubles height
    buf.extend_from_slice(&[1, 0]); // planes
    buf.extend_from_slice(&[32, 0]); // bpp
    buf.extend_from_slice(&[0; 24]); // rest of header (zeroed)

    // pixel data (BGRA, bottom-up)
    for y_row in 0..h {
        let y = h - 1 - y_row; // bottom-up
        for x in 0..w {
            let inside = x > 4 && x < 27 && y > 4 && y < 27;
            let accent = x > 9 && x < 23 && y > 9 && y < 23;
            let (b, g, r, a) = if accent {
                (235u8, 99u8, 37u8, 255u8)
            } else if inside {
                (178u8, 145u8, 8u8, 255u8)
            } else {
                (0u8, 0u8, 0u8, 0u8)
            };
            buf.extend_from_slice(&[b, g, r, a]);
        }
    }

    // AND mask (all zeros for 32-bit with alpha)
    buf.resize(buf.len() + and_mask_size, 0);

    buf
}

//! 一次性生成托盘「未读」图标：把 `icons/32x32.png` 右下角叠一颗
//! 带白色衬环的红点，输出 `icons/tray-unread.png`。
//!
//! 产物提交入库（`tray.rs` 用 `include_image!` 编译期内嵌），日常构建
//! 不依赖本 example。红点在深/浅两种任务栏背景上都要可辨认，故衬一环白边。
//!
//! 运行：`cargo run --example gen_tray_unread`（于 src-tauri/ 目录）

/// 圆点中心（右下角）。32px 图在托盘缩到 16px 显示，直径 12px 的红心 → 约 6px。
const DOT_CX: f32 = 23.5;
const DOT_CY: f32 = 23.5;
/// 白色衬环外半径（红心半径须小于它，差值部分即白边）。
const RING_R: f32 = 8.0;
/// 红心半径。
const DOT_R: f32 = 6.0;
/// #ef4444，与产品红色提示一致。
const RED: [f32; 3] = [239.0, 68.0, 68.0];
const WHITE: [f32; 3] = [255.0, 255.0, 255.0];
/// 每像素 4x4 超采样，让圆边有抗锯齿。
const SS: u32 = 4;

/// 像素 (x, y) 被以 (DOT_CX, DOT_CY) 为圆心、半径 r 的圆覆盖的比例（0~1 抗锯齿）。
fn coverage(x: u32, y: u32, r: f32) -> f32 {
    let mut hit = 0u32;
    for sy in 0..SS {
        for sx in 0..SS {
            let px = x as f32 + (sx as f32 + 0.5) / SS as f32;
            let py = y as f32 + (sy as f32 + 0.5) / SS as f32;
            let dx = px - DOT_CX;
            let dy = py - DOT_CY;
            if dx * dx + dy * dy <= r * r {
                hit += 1;
            }
        }
    }
    hit as f32 / (SS * SS) as f32
}

/// straight-alpha 的 over 合成：不透明色 `layer` 以覆盖率 `a` 叠在
/// 当前像素（rgb/alpha 各分量）上，返回合成后的 (rgb, alpha)。
fn over(rgb: [f32; 3], a_b: f32, layer: [f32; 3], a: f32) -> ([f32; 3], f32) {
    let a_out = a + a_b * (1.0 - a);
    let mut out = [0.0f32; 3];
    if a_out > 0.0 {
        for c in 0..3 {
            out[c] = (layer[c] * a + rgb[c] * a_b * (1.0 - a)) / a_out;
        }
    }
    (out, a_out)
}

fn main() {
    let src = std::fs::read("icons/32x32.png").expect("读取 icons/32x32.png 失败（须在 src-tauri/ 下运行）");
    let decoder = png::Decoder::new(std::io::Cursor::new(&src));
    let mut reader = decoder.read_info().expect("解析 PNG 失败");
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).expect("解码 PNG 失败");
    assert_eq!(
        info.color_type,
        png::ColorType::Rgba,
        "底图应为 RGBA（当前 {:?}）",
        info.color_type
    );
    let (w, h) = (info.width, info.height);
    let mut px = buf[..info.buffer_size()].to_vec();

    for y in 0..h {
        for x in 0..w {
            let a_ring = coverage(x, y, RING_R);
            if a_ring <= 0.0 {
                continue;
            }
            let a_dot = coverage(x, y, DOT_R);
            let i = ((y * w + x) * 4) as usize;
            let base = [px[i] as f32, px[i + 1] as f32, px[i + 2] as f32];
            let a_base = px[i + 3] as f32 / 255.0;

            let (rgb1, a1) = over(base, a_base, WHITE, a_ring);
            let (rgb2, a2) = over(rgb1, a1, RED, a_dot);

            for c in 0..3 {
                px[i + c] = rgb2[c].round().clamp(0.0, 255.0) as u8;
            }
            px[i + 3] = (a2 * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    }

    let out_path = "icons/tray-unread.png";
    let file = std::fs::File::create(out_path).expect("创建输出文件失败");
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .expect("写 PNG 头失败")
        .write_image_data(&px)
        .expect("写 PNG 数据失败");
    println!("已生成 {out_path}（{w}x{h}）");
}

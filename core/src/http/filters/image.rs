//! nginx `ngx_http_image_filter_module`: resize / crop / rotate JPEG, PNG, GIF
//! and WebP responses, or report their size as JSON. Built with the
//! `image-filter` feature.

use axum::body::Body;
use axum::http::{header, HeaderValue, Response, StatusCode};
use axum::response::IntoResponse;
use image::{DynamicImage, ImageFormat};
use std::io::Cursor;

use crate::config::{ImageFilterConfig, ImageOp};

fn fit(w: u32, h: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    // `-` (0) means "any" in nginx.
    let rw = if max_w == 0 {
        f64::MAX
    } else {
        max_w as f64 / w as f64
    };
    let rh = if max_h == 0 {
        f64::MAX
    } else {
        max_h as f64 / h as f64
    };
    let r = rw.min(rh).min(1.0);
    (
        ((w as f64) * r).round().max(1.0) as u32,
        ((h as f64) * r).round().max(1.0) as u32,
    )
}

pub fn transform(
    cfg: &ImageFilterConfig,
    bytes: &[u8],
) -> Result<(Vec<u8>, &'static str), StatusCode> {
    let format = image::guess_format(bytes).map_err(|_| StatusCode::UNSUPPORTED_MEDIA_TYPE)?;
    let mime = match format {
        ImageFormat::Jpeg => "image/jpeg",
        ImageFormat::Png => "image/png",
        ImageFormat::Gif => "image/gif",
        ImageFormat::WebP => "image/webp",
        _ => return Err(StatusCode::UNSUPPORTED_MEDIA_TYPE),
    };
    if cfg.op == ImageOp::Test {
        return Ok((bytes.to_vec(), mime));
    }
    let img = image::load_from_memory_with_format(bytes, format)
        .map_err(|_| StatusCode::UNSUPPORTED_MEDIA_TYPE)?;
    if cfg.op == ImageOp::Size {
        let json = format!(
            "{{ \"img\" : {{ \"width\": {}, \"height\": {}, \"type\": \"{}\" }} }}\n",
            img.width(),
            img.height(),
            mime.trim_start_matches("image/")
        );
        return Ok((json.into_bytes(), "application/json"));
    }
    let mut img: DynamicImage = match cfg.op {
        ImageOp::Resize => {
            let (w, h) = fit(img.width(), img.height(), cfg.width, cfg.height);
            if (w, h) == (img.width(), img.height()) {
                img
            } else {
                img.resize_exact(w, h, image::imageops::FilterType::Triangle)
            }
        }
        ImageOp::Crop => {
            // Scale to cover, then cut the centre.
            let (tw, th) = (
                if cfg.width == 0 {
                    img.width()
                } else {
                    cfg.width.min(img.width())
                },
                if cfg.height == 0 {
                    img.height()
                } else {
                    cfg.height.min(img.height())
                },
            );
            let r = (tw as f64 / img.width() as f64)
                .max(th as f64 / img.height() as f64)
                .min(1.0);
            let scaled = if r < 1.0 {
                img.resize_exact(
                    ((img.width() as f64) * r).ceil() as u32,
                    ((img.height() as f64) * r).ceil() as u32,
                    image::imageops::FilterType::Triangle,
                )
            } else {
                img
            };
            let x = (scaled.width().saturating_sub(tw)) / 2;
            let y = (scaled.height().saturating_sub(th)) / 2;
            scaled.crop_imm(x, y, tw, th)
        }
        _ => img,
    };
    img = match cfg.rotate {
        90 => img.rotate90(),
        180 => img.rotate180(),
        270 => img.rotate270(),
        _ => img,
    };
    let mut out = Cursor::new(Vec::new());
    let r = if format == ImageFormat::Jpeg {
        let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(
            &mut out,
            cfg.jpeg_quality.clamp(1, 100),
        );
        img.to_rgb8().write_with_encoder(enc)
    } else {
        img.write_to(&mut out, format)
    };
    r.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok((out.into_inner(), mime))
}

pub async fn apply(cfg: &ImageFilterConfig, resp: Response<Body>) -> Response<Body> {
    if !resp.status().is_success() {
        return resp;
    }
    let (mut parts, body) = resp.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, cfg.buffer_bytes as usize).await else {
        return StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response();
    };
    // Heavy work off the async workers.
    let cfg2 = cfg.clone();
    let res = tokio::task::spawn_blocking(move || transform(&cfg2, &bytes)).await;
    match res {
        Ok(Ok((out, mime))) => {
            for h in [
                header::ETAG,
                header::LAST_MODIFIED,
                header::ACCEPT_RANGES,
                header::CONTENT_ENCODING,
            ] {
                parts.headers.remove(h);
            }
            parts
                .headers
                .insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
            parts
                .headers
                .insert(header::CONTENT_LENGTH, HeaderValue::from(out.len()));
            Response::from_parts(parts, Body::from(out))
        }
        Ok(Err(code)) => code.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let img = DynamicImage::new_rgb8(w, h);
        let mut out = Cursor::new(Vec::new());
        img.write_to(&mut out, ImageFormat::Png).unwrap();
        out.into_inner()
    }

    fn cfg(op: ImageOp, w: u32, h: u32) -> ImageFilterConfig {
        ImageFilterConfig {
            op,
            width: w,
            height: h,
            ..Default::default()
        }
    }

    #[test]
    fn resize_crop_size_rotate() {
        let src = png(400, 200);
        let (out, _) = transform(&cfg(ImageOp::Resize, 100, 100), &src).unwrap();
        let i = image::load_from_memory(&out).unwrap();
        assert_eq!((i.width(), i.height()), (100, 50));
        let (out, _) = transform(&cfg(ImageOp::Crop, 100, 100), &src).unwrap();
        let i = image::load_from_memory(&out).unwrap();
        assert_eq!((i.width(), i.height()), (100, 100));
        let (out, mime) = transform(&cfg(ImageOp::Size, 0, 0), &src).unwrap();
        assert_eq!(mime, "application/json");
        assert!(String::from_utf8(out).unwrap().contains("\"width\": 400"));
        let mut c = cfg(ImageOp::Rotate, 0, 0);
        c.rotate = 90;
        let (out, _) = transform(&c, &src).unwrap();
        let i = image::load_from_memory(&out).unwrap();
        assert_eq!((i.width(), i.height()), (200, 400));
        assert_eq!(
            transform(&cfg(ImageOp::Test, 0, 0), b"not an image").unwrap_err(),
            415
        );
    }
}

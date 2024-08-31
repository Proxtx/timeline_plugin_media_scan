use raqote::{DrawTarget, Source, SolidSource, DrawOptions, Image};
use std::pin::Pin;
use shared::timeline::types::api::CompressedEvent;
use rand::Rng;
use image::Pixel;
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct SignedMedia {
    path: String,
    signature: String
}

pub struct PluginRenderer {}

impl crate::renderer::PluginRenderer for PluginRenderer {
    async fn new() -> PluginRenderer {
        PluginRenderer {}
    }

    fn render(&self, dimensions: (i32, i32), event: &CompressedEvent) -> Pin<Box<dyn std::future::Future<Output = Result<Vec<u32>, String>> + Send>> {
        let data = event.data.clone();

        Box::pin(async move {
            let mut target = DrawTarget::new(dimensions.0, dimensions.1);
            let path = match serde_json::from_str::<SignedMedia>(&data) {
                Ok(v) => v,
                Err(e) => {
                    return Err(format!("Unable to read CompressedEvent: {}", e))
                }
            }.path;
            let mut img = match image::ImageReader::open(std::path::PathBuf::from(path)) {
                Ok(v) => match v.decode() {
                    Ok(v) => v,
                    Err(e) => return Err(format!("Unable to decode Image: {}", e))
                }
                Err(e) => {
                    return Err(format!("Unable to open image path: {}", e))
                }
            };

            img = img.resize_to_fill(dimensions.0 as u32, dimensions.1 as u32, image::imageops::FilterType::Lanczos3);
            img = img.thumbnail_exact(dimensions.0 as u32, dimensions.1 as u32);

            let width = img.width();
            let height = img.height();

            // im too proud to delete this code
            /*
            let width_multi = width / dimensions.0 as u32;
            let height_multi = height / dimensions.1 as u32;
            
            let width_respective_height = dimensions.1 as u32 * width_multi;
            let height_respective_width = dimensions.0 as u32 * height_multi;

            let cut_multi = if width_respective_height <= height {
                width_multi
            }
            else {
                height_multi
            };

            let (cut_width, cut_height) = (dimensions.0 as u32 * cut_multi, dimensions.1 as u32 * cut_multi);

            image = image.crop(
                (width - cut_width) / 2, (height - cut_height) / 2, cut_width, cut_height
            ); */
            
            let rgba = img.into_rgba8();
            let pixels = rgba.pixels().map(|v| {
                let c = v.channels();
                ((c[3] as u32) << 24) | ((c[0] as u32) << 16) | ((c[1] as u32) << 8) | (c[2] as u32)
            }).collect::<Vec<u32>>();

            let image = raqote::Image {
                width: width as i32,
                height: height as i32,
                data: &pixels
            };

            target.draw_image_at(0., 0., &image, &raqote::DrawOptions::new());

            //target.fill_rect(0., 0., dimensions.0 as f32, dimensions.1 as f32, &Source::Solid(SolidSource::from_unpremultiplied_argb(255, rng.gen(), 0, 0)), &DrawOptions::new());
            Ok(target.into_vec())
        })
    }
}

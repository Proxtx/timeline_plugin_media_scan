use raqote::{DrawTarget, Source, SolidSource, DrawOptions};
use std::pin::Pin;
use shared::timeline::types::api::CompressedEvent;

pub struct PluginRenderer {}

impl crate::renderer::PluginRenderer for PluginRenderer {
    async fn new() -> PluginRenderer {
        PluginRenderer {}
    }

    fn render(&self, target: &mut DrawTarget, _event: &CompressedEvent) -> Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>> {
        Box::pin(async {
            target.fill_rect(0., 0., 50., 50., &Source::Solid(SolidSource::from_unpremultiplied_argb(255, 0, 180, 0)), &DrawOptions::new());
            Ok(())
        })
    }
}

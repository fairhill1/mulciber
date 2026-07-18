//! Renders the same four instance batches as `mulciber-instanced-scene` through wgpu and winit.

mod gpu;
mod scene;

use std::error::Error;
use std::sync::Arc;
use std::time::Instant;

use gpu::Gpu;
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::{Window, WindowId};

fn main() -> Result<(), Box<dyn Error>> {
    let event_loop = EventLoop::new()?;
    let mut app = App {
        window: None,
        gpu: None,
        started: Instant::now(),
        failure: None,
    };
    event_loop.run_app(&mut app)?;
    if let Some(failure) = app.failure {
        return Err(failure);
    }
    Ok(())
}

struct App {
    window: Option<Arc<Window>>,
    gpu: Option<Gpu>,
    started: Instant,
    failure: Option<Box<dyn Error>>,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attributes = Window::default_attributes()
            .with_title("wgpu — 100-object instanced scene")
            .with_inner_size(LogicalSize::new(1100, 700));
        match event_loop.create_window(attributes) {
            Ok(window) => {
                let window = Arc::new(window);
                match pollster::block_on(Gpu::new(Arc::clone(&window))) {
                    Ok(gpu) => {
                        println!(
                            "backend: wgpu ({:?}), samples: {}, scene objects: 100, instance batches: 4",
                            gpu.backend(),
                            if gpu.sample_count() == 4 { "Four" } else { "One" }
                        );
                        self.gpu = Some(gpu);
                        self.window = Some(window);
                    }
                    Err(error) => {
                        self.failure = Some(error);
                        event_loop.exit();
                    }
                }
            }
            Err(error) => {
                self.failure = Some(Box::new(error));
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let (Some(window), Some(gpu)) = (self.window.as_ref(), self.gpu.as_mut()) else {
            return;
        };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => gpu.resize(size.width, size.height),
            WindowEvent::RedrawRequested => {
                if let Err(error) = gpu.render(self.started.elapsed().as_secs_f32()) {
                    self.failure = Some(error);
                    event_loop.exit();
                    return;
                }
                window.request_redraw();
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }
}

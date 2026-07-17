//! Renders a spinning textured cube through Mulciber's target-selected native backend.

use std::env;
use std::error::Error;
use std::time::Instant;

use mulciber::{
    ClearColor, DeviceRequest, FrameAcquire, OpenedGraphics, ShaderArtifact, TexturedDraw, Vertex,
};
use mulciber_platform::{
    Application, LogicalSize, PumpStatus, Window, WindowDescriptor, WindowEvent, WindowMetrics,
};

const SHADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/cube.shaderbin"));

struct Options {
    frame_limit: Option<u64>,
    abandon_once: bool,
    force_one_sample: bool,
}

#[allow(clippy::cast_precision_loss, clippy::too_many_lines)]
fn main() -> Result<(), Box<dyn Error>> {
    let options = options()?;
    let mut application = Application::new()?;
    let window = application.create_window(&WindowDescriptor::new(
        "Mulciber — same-source textured cube",
        LogicalSize::new(960, 540),
    ))?;
    let initial_metrics = wait_for_initial_metrics(&mut application, &window)?;
    let request = DeviceRequest {
        preferred_sample_count: if options.force_one_sample {
            mulciber::SampleCount::One
        } else {
            mulciber::SampleCount::Four
        },
    };
    let mut graphics = OpenedGraphics::open(window.surface_target(), initial_metrics, request)?;
    println!(
        "backend: {}, samples: {:?}",
        graphics.selection.backend(),
        graphics.selection.sample_count()
    );

    let mesh = graphics.device.create_mesh(&CUBE_VERTICES, &CUBE_INDICES)?;
    let checkerboard = checkerboard();
    let texture = graphics
        .device
        .create_rgba8_srgb_texture(8, 8, &checkerboard)?;
    let shader = ShaderArtifact::new(SHADER)?;
    let pipeline = graphics.device.create_textured_pipeline(shader)?;
    let mut targets = graphics
        .device
        .create_render_targets(graphics.surface.info()?)?;
    let clear = ClearColor::new(0.025, 0.035, 0.055, 1.0).expect("constant is normalized");
    let started = Instant::now();
    let mut presented = 0_u64;
    let mut abandon_pending = options.abandon_once;
    let mut failure: Option<Box<dyn Error>> = None;

    loop {
        let status = application.pump_events(&window, |event| {
            if failure.is_some() {
                return;
            }
            let WindowEvent::RedrawRequested(metrics) = event else {
                return;
            };
            let result: Result<(), Box<dyn Error>> = (|| {
                match graphics.surface.acquire(metrics)? {
                    FrameAcquire::Ready(frame) if abandon_pending => {
                        frame.abandon()?;
                        targets = graphics
                            .device
                            .create_render_targets(graphics.surface.info()?)?;
                        abandon_pending = false;
                    }
                    FrameAcquire::Ready(frame) => {
                        let info = frame.surface_info();
                        let aspect = info.extent().width() as f32 / info.extent().height() as f32;
                        graphics.queue.draw_textured_and_present(
                            frame,
                            TexturedDraw {
                                mesh: &mesh,
                                texture: &texture,
                                pipeline: &pipeline,
                                targets: &targets,
                                model_view_projection: cube_transform(
                                    started.elapsed().as_secs_f32(),
                                    aspect,
                                ),
                                clear,
                            },
                        )?;
                        presented += 1;
                    }
                    FrameAcquire::Unavailable(_) => {}
                    FrameAcquire::Reconfigured(info) => {
                        targets = graphics.device.create_render_targets(info)?;
                        println!(
                            "surface generation {} configured at {}x{}",
                            info.generation().get(),
                            info.extent().width(),
                            info.extent().height()
                        );
                    }
                }
                Ok(())
            })();
            if let Err(error) = result {
                failure = Some(error);
            }
        })?;
        if let Some(error) = failure.take() {
            return Err(error);
        }
        if status == PumpStatus::Exit
            || options
                .frame_limit
                .is_some_and(|frame_limit| presented >= frame_limit)
        {
            break;
        }
    }

    if abandon_pending {
        return Err("requested abandonment never acquired a frame".into());
    }
    graphics.shutdown()?;
    println!("presented {presented} textured cube frame(s)");
    Ok(())
}

fn wait_for_initial_metrics(
    application: &mut Application,
    window: &Window,
) -> Result<WindowMetrics, Box<dyn Error>> {
    loop {
        if let Some(metrics) = window.rendering_metrics() {
            return Ok(metrics);
        }
        let mut initial_metrics = None;
        let status = application.pump_events(window, |event| {
            if let WindowEvent::RenderingResumed(metrics) | WindowEvent::RedrawRequested(metrics) =
                event
            {
                initial_metrics = Some(metrics);
            }
        })?;
        if let Some(metrics) = initial_metrics {
            return Ok(metrics);
        }
        if status == PumpStatus::Exit {
            return Err("window closed before drawable metrics became available".into());
        }
    }
}

fn checkerboard() -> [u8; 8 * 8 * 4] {
    let mut texels = [0_u8; 8 * 8 * 4];
    for y in 0..8 {
        for x in 0..8 {
            let bright = (x / 2 + y / 2) % 2 == 0;
            let color = if bright {
                [245, 170, 45, 255]
            } else {
                [35, 95, 210, 255]
            };
            let offset = (y * 8 + x) * 4;
            texels[offset..offset + 4].copy_from_slice(&color);
        }
    }
    texels
}

fn cube_transform(seconds: f32, aspect: f32) -> [[f32; 4]; 4] {
    let model = multiply(rotation_y(seconds * 0.85), rotation_x(seconds * 0.47));
    let view = translation(0.0, 0.0, -4.0);
    let projection = perspective(55_f32.to_radians(), aspect, 0.1, 100.0);
    multiply(projection, multiply(view, model))
}

fn multiply(left: [[f32; 4]; 4], right: [[f32; 4]; 4]) -> [[f32; 4]; 4] {
    let mut result = [[0.0; 4]; 4];
    for column in 0..4 {
        for row in 0..4 {
            result[column][row] = (0..4)
                .map(|index| left[index][row] * right[column][index])
                .sum();
        }
    }
    result
}

fn rotation_x(angle: f32) -> [[f32; 4]; 4] {
    let (sin, cos) = angle.sin_cos();
    [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, cos, sin, 0.0],
        [0.0, -sin, cos, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

fn rotation_y(angle: f32) -> [[f32; 4]; 4] {
    let (sin, cos) = angle.sin_cos();
    [
        [cos, 0.0, -sin, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [sin, 0.0, cos, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

const fn translation(x: f32, y: f32, z: f32) -> [[f32; 4]; 4] {
    [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [x, y, z, 1.0],
    ]
}

fn perspective(fov_y: f32, aspect: f32, near: f32, far: f32) -> [[f32; 4]; 4] {
    let focal = 1.0 / (fov_y * 0.5).tan();
    [
        [focal / aspect, 0.0, 0.0, 0.0],
        [0.0, focal, 0.0, 0.0],
        [0.0, 0.0, far / (near - far), -1.0],
        [0.0, 0.0, far * near / (near - far), 0.0],
    ]
}

fn options() -> Result<Options, Box<dyn Error>> {
    let mut frame_limit = None;
    let mut abandon_once = false;
    let mut force_one_sample = false;
    let mut arguments = env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--frames" => {
                let value = arguments.next().ok_or("--frames requires a value")?;
                let value = value.parse::<u64>()?;
                if value == 0 {
                    return Err("--frames must be greater than zero".into());
                }
                frame_limit = Some(value);
            }
            "--abandon-acquired-frame-once" => abandon_once = true,
            "--force-one-sample" => force_one_sample = true,
            _ => return Err(format!("unknown argument: {argument}").into()),
        }
    }
    Ok(Options {
        frame_limit,
        abandon_once,
        force_one_sample,
    })
}

const CUBE_VERTICES: [Vertex; 24] = cube_vertices();

const fn vertex(position: [f32; 3], color: [f32; 3], uv: [f32; 2]) -> Vertex {
    Vertex {
        position,
        color,
        uv,
    }
}

const fn cube_vertices() -> [Vertex; 24] {
    let n = -1.0;
    let p = 1.0;
    [
        vertex([n, n, p], [1.0, 0.45, 0.35], [0.0, 1.0]),
        vertex([p, n, p], [1.0, 0.45, 0.35], [1.0, 1.0]),
        vertex([p, p, p], [1.0, 0.45, 0.35], [1.0, 0.0]),
        vertex([n, p, p], [1.0, 0.45, 0.35], [0.0, 0.0]),
        vertex([p, n, n], [0.35, 0.75, 1.0], [0.0, 1.0]),
        vertex([n, n, n], [0.35, 0.75, 1.0], [1.0, 1.0]),
        vertex([n, p, n], [0.35, 0.75, 1.0], [1.0, 0.0]),
        vertex([p, p, n], [0.35, 0.75, 1.0], [0.0, 0.0]),
        vertex([n, n, n], [0.45, 1.0, 0.55], [0.0, 1.0]),
        vertex([n, n, p], [0.45, 1.0, 0.55], [1.0, 1.0]),
        vertex([n, p, p], [0.45, 1.0, 0.55], [1.0, 0.0]),
        vertex([n, p, n], [0.45, 1.0, 0.55], [0.0, 0.0]),
        vertex([p, n, p], [0.95, 0.85, 0.35], [0.0, 1.0]),
        vertex([p, n, n], [0.95, 0.85, 0.35], [1.0, 1.0]),
        vertex([p, p, n], [0.95, 0.85, 0.35], [1.0, 0.0]),
        vertex([p, p, p], [0.95, 0.85, 0.35], [0.0, 0.0]),
        vertex([n, p, p], [0.85, 0.45, 1.0], [0.0, 1.0]),
        vertex([p, p, p], [0.85, 0.45, 1.0], [1.0, 1.0]),
        vertex([p, p, n], [0.85, 0.45, 1.0], [1.0, 0.0]),
        vertex([n, p, n], [0.85, 0.45, 1.0], [0.0, 0.0]),
        vertex([n, n, n], [0.35, 0.95, 0.9], [0.0, 1.0]),
        vertex([p, n, n], [0.35, 0.95, 0.9], [1.0, 1.0]),
        vertex([p, n, p], [0.35, 0.95, 0.9], [1.0, 0.0]),
        vertex([n, n, p], [0.35, 0.95, 0.9], [0.0, 0.0]),
    ]
}

const CUBE_INDICES: [u16; 36] = [
    0, 1, 2, 0, 2, 3, 4, 5, 6, 4, 6, 7, 8, 9, 10, 8, 10, 11, 12, 13, 14, 12, 14, 15, 16, 17, 18,
    16, 18, 19, 20, 21, 22, 20, 22, 23,
];

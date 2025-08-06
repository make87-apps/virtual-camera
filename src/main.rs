use ffmpeg_next as ffmpeg;
use make87::encodings::{Encoder, ProtobufEncoder};
use make87::interfaces::zenoh::ZenohInterface;
use make87::config::{load_config_from_default_env};
use make87_messages::image::uncompressed::ImageRawAny;
use make87_messages::image::uncompressed::image_raw_any::Image;
use make87_messages::image::uncompressed::ImageYuv420;
use make87_messages::core::Header;
use std::error::Error;
use make87::models::ApplicationConfig;
use make87_messages::google::protobuf::Timestamp;
use serde_json::Value;
use tokio::time::{sleep, Duration, Instant};
use uuid::Uuid;

#[derive(Clone)]
struct CachedFrame {
    width: u32,
    height: u32,
    data: Vec<u8>,
    pts: i64,
    time_offset: f64, // Time offset from start of video
}

fn get_config_value<'a>(cfg: &'a ApplicationConfig, key: &str) -> Option<&'a Value> {
    cfg.config.get(key)
}

fn resolve_video_url(video_source: &Value) -> Result<String, Box<dyn Error + Send + Sync>> {
    let source_type = video_source.get("type")
        .and_then(|v| v.as_str())
        .ok_or("Missing or invalid video source type")?;

    match source_type {
        "predefined" => {
            let selection = video_source.get("selection")
                .and_then(|v| v.as_str())
                .ok_or("Missing selection for predefined video")?;

            let url = match selection {
                "city_walk" => "https://make87-files.nyc3.digitaloceanspaces.com/example-apps/virtual-camera/city_walk.mp4",
                "highway_traffic" => "https://make87-assets.nyc3.digitaloceanspaces.com/videos/highway_traffic.mp4",
                "drone_flight" => "https://make87-files.nyc3.digitaloceanspaces.com/example-apps/virtual-camera/drone_flight.mp4",
                _ => return Err(format!("Unknown predefined video selection: {}", selection).into()),
            };

            log::info!("Using predefined video '{}': {}", selection, url);
            Ok(url.to_string())
        },
        "custom_url" => {
            let url = video_source.get("url")
                .and_then(|v| v.as_str())
                .ok_or("Missing URL for custom video source")?;

            log::info!("Using custom video URL: {}", url);
            Ok(url.to_string())
        },
        _ => Err(format!("Unknown video source type: {}", source_type).into()),
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    env_logger::init();

    // Load configuration
    let config = load_config_from_default_env()?;
    let video_source = get_config_value(&config, "video_source")
        .ok_or("Missing 'video_source' configuration")?;

    // Resolve video URL
    let url = resolve_video_url(video_source)?;

    // Generate entity path with short random UUID
    let short_uuid = Uuid::new_v4().to_string()[..8].to_string();
    let entity_path = format!("/virtual_camera_{}", short_uuid);

    // 2. Set up Zenoh publisher
    let zenoh_interface = ZenohInterface::from_default_env("zenoh")?;
    let session = zenoh_interface.get_session().await?;
    let publisher = zenoh_interface.get_publisher(&session, "raw_frames").await?;
    let message_encoder = ProtobufEncoder::<ImageRawAny>::new();

    // Base header template
    let base_header = Header {
        timestamp: None,
        reference_id: 0,
        entity_path: entity_path.clone(),
    };

    // Initialize FFmpeg
    ffmpeg::init().unwrap();

    log::info!("Starting initial video decode and caching...");

    // PHASE 1: Decode entire video once and cache all frames
    let cached_frames = {
        let mut ictx = ffmpeg::format::input(&url)?;
        let input = ictx
            .streams()
            .best(ffmpeg::media::Type::Video)
            .expect("Could not find video stream");
        let video_stream_index = input.index();
        let codec_params = input.parameters();
        let mut decoder = ffmpeg::codec::context::Context::from_parameters(codec_params)?
            .decoder()
            .video()?;

        let time_base = input.time_base();
        let time_base_f64 = f64::from(time_base);

        let mut frames = Vec::new();
        let mut first_pts = None;

        for (stream, packet) in ictx.packets() {
            if stream.index() != video_stream_index {
                continue;
            }

            let mut decoded = ffmpeg::util::frame::video::Video::empty();
            decoder.send_packet(&packet)?;

            while decoder.receive_frame(&mut decoded).is_ok() {
                if decoded.width() == 0 || decoded.height() == 0 {
                    continue;
                }

                let pts = decoded.pts().unwrap_or(0);

                if first_pts.is_none() {
                    first_pts = Some(pts);
                }
                let first_pts_value = first_pts.unwrap();

                // Calculate time offset from start of video
                let time_offset = (pts - first_pts_value) as f64 * time_base_f64;

                // Extract YUV420 data
                let width = decoded.width() as u32;
                let height = decoded.height() as u32;

                let y_data = decoded.data(0).to_vec();
                let u_data = decoded.data(1).to_vec();
                let v_data = decoded.data(2).to_vec();

                let mut combined_data = Vec::new();
                combined_data.extend_from_slice(&y_data);
                combined_data.extend_from_slice(&u_data);
                combined_data.extend_from_slice(&v_data);

                frames.push(CachedFrame {
                    width,
                    height,
                    data: combined_data,
                    pts,
                    time_offset,
                });

                log::debug!("Cached frame {} at time {:.3}s", frames.len(), time_offset);
            }
        }

        log::info!("Cached {} frames, total duration: {:.3}s", frames.len(),
                   frames.last().map(|f| f.time_offset).unwrap_or(0.0));
        frames
    };

    if cached_frames.is_empty() {
        return Err("No frames could be decoded from video".into());
    }

    let video_duration = cached_frames.last().unwrap().time_offset;

    // PHASE 2: Replay cached frames in infinite loop
    let start_wallclock = Instant::now();
    let mut virtual_time_offset = 0.0;

    log::info!("Starting infinite playback loop...");

    loop {
        for frame in &cached_frames {
            let total_video_time = virtual_time_offset + frame.time_offset;

            // Create headers with current timestamp
            let mut header = base_header.clone();
            header.timestamp = Some(Timestamp::get_current_time());

            // Create YUV420 image message from cached data
            let yuv420_image = ImageYuv420 {
                header: Some(header.clone()),
                width: frame.width,
                height: frame.height,
                data: frame.data.clone(),
            };

            // Create ImageRawAny message
            let image_any = ImageRawAny {
                header: Some(header),
                image: Some(Image::Yuv420(yuv420_image)),
            };

            // Encode the message
            let message_encoded = message_encoder.encode(&image_any)?;

            // Wait for the right time to publish
            let elapsed_since_start = start_wallclock.elapsed().as_secs_f64();
            if total_video_time > elapsed_since_start {
                let wait_time = total_video_time - elapsed_since_start;
                sleep(Duration::from_secs_f64(wait_time)).await;
            }

            // Publish immediately when the time is right
            publisher.put(&message_encoded).await?;

            log::info!(
                "Published cached frame {}x{} PTS={} total_video_time={:.3}s (wallclock elapsed={:.3}s)",
                frame.width,
                frame.height,
                frame.pts,
                total_video_time,
                elapsed_since_start
            );
        }

        // Update virtual time offset for seamless looping
        virtual_time_offset += video_duration;
        log::info!("Loop completed. Restarting seamlessly... (virtual_time_offset: {:.3}s)", virtual_time_offset);
    }

}

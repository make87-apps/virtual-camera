use ffmpeg_next as ffmpeg;
use make87::config::load_config_from_default_env;
use make87::encodings::{Encoder, ProtobufEncoder};
use make87::interfaces::zenoh::ZenohInterface;
use make87::models::ApplicationConfig;
use make87_messages::core::Header;
use make87_messages::google::protobuf::Timestamp;
use make87_messages::image::uncompressed::image_raw_any::Image;
use make87_messages::image::uncompressed::ImageRawAny;
use make87_messages::image::uncompressed::ImageYuv420;
use serde_json::Value;
use std::error::Error;
use tokio::time::{sleep, Duration, Instant};
use uuid::Uuid;

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
                "city_walk_4k" => "https://make87-files.nyc3.digitaloceanspaces.com/example-apps/virtual-camera/city_walk.mp4",
                "city_walk_1080" => "https://make87-files.nyc3.digitaloceanspaces.com/example-apps/virtual-camera/city_walk_1080.mp4",
                "highway_traffic_4k" => "https://make87-assets.nyc3.digitaloceanspaces.com/videos/highway_traffic.mp4",
                "highway_traffic_1080" => "https://make87-assets.nyc3.digitaloceanspaces.com/videos/highway_traffic_1080.mp4",
                "drone_flight_4k" => "https://make87-files.nyc3.digitaloceanspaces.com/example-apps/virtual-camera/drone_flight.mp4",
                "drone_flight_1080" => "https://make87-files.nyc3.digitaloceanspaces.com/example-apps/virtual-camera/drone_flight_1080.mp4",
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

    // Start wallclock and timing variables
    let start_wallclock = Instant::now();
    let mut start_pts = None;
    let mut virtual_time_offset = 0.0; // Track cumulative video time for seamless loops

    // Stream, decode, and publish frames at wallclock speed - loop infinitely
    loop {
        // Reset for each loop iteration
        let ictx_result = ffmpeg::format::input(&url);
        let mut ictx = match ictx_result {
            Ok(ctx) => ctx,
            Err(e) => {
                log::warn!("Failed to open stream: {:?}. Retrying in 1 second...", e);
                sleep(Duration::from_secs(1)).await;
                continue;
            }
        };

        let input = match ictx.streams().best(ffmpeg::media::Type::Video) {
            Some(stream) => stream,
            None => {
                log::warn!("Could not find video stream. Retrying in 1 second...");
                sleep(Duration::from_secs(1)).await;
                continue;
            }
        };

        let video_stream_index = input.index();
        let codec_params = input.parameters();
        let decoder_result = ffmpeg::codec::context::Context::from_parameters(codec_params)
            .and_then(|ctx| ctx.decoder().video());
        let mut decoder = match decoder_result {
            Ok(dec) => dec,
            Err(e) => {
                log::warn!("Failed to create decoder: {:?}. Retrying in 1 second...", e);
                sleep(Duration::from_secs(1)).await;
                continue;
            }
        };

        let time_base = input.time_base();
        let time_base_f64 = f64::from(time_base);

        // Prepare scaler for YUV420p conversion (will be initialized lazily)
        let mut scaler: Option<ffmpeg::software::scaling::Context> = None;

        // Track the first PTS of this loop to calculate the offset
        let mut loop_start_pts = None;
        let mut last_pts = None;

        for (stream, packet) in ictx.packets() {
            if stream.index() != video_stream_index {
                continue;
            }

            let mut decoded = ffmpeg::util::frame::video::Video::empty();

            // Handle decoder errors gracefully - just break and restart stream
            if decoder.send_packet(&packet).is_err() {
                log::warn!("Packet decode error, restarting stream...");
                break;
            }

            while decoder.receive_frame(&mut decoded).is_ok() {
                if decoded.width() == 0 || decoded.height() == 0 {
                    continue;
                }

                // Always use a separate output_frame variable
                let mut output_frame = ffmpeg::util::frame::video::Video::empty();

                // Handle frame conversion errors - if it fails, skip this frame
                let conversion_result = if decoded.format() != ffmpeg::format::Pixel::YUV420P {
                    // Lazily initialize scaler
                    if scaler.is_none() {
                        match ffmpeg::software::scaling::Context::get(
                            decoded.format(),
                            decoded.width(),
                            decoded.height(),
                            ffmpeg::format::Pixel::YUV420P,
                            decoded.width(),
                            decoded.height(),
                            ffmpeg::software::scaling::flag::Flags::BILINEAR,
                        ) {
                            Ok(ctx) => scaler = Some(ctx),
                            Err(e) => {
                                log::warn!("Failed to create scaler, skipping frame: {:?}", e);
                                continue;
                            }
                        }
                    }
                    match scaler.as_mut().unwrap().run(&decoded, &mut output_frame) {
                        Ok(_) => {
                            // Preserve PTS from original frame
                            output_frame.set_pts(decoded.pts());
                            Ok(())
                        }
                        Err(e) => Err(e)
                    }
                } else {
                    // Copy decoded into output_frame without clone
                    std::mem::swap(&mut output_frame, &mut decoded);
                    Ok(())
                };

                // If conversion failed, skip this frame
                if conversion_result.is_err() {
                    log::warn!("Frame conversion failed, skipping frame: {:?}", conversion_result.unwrap_err());
                    continue;
                }

                let frame = &output_frame;

                let pts = frame.pts().unwrap_or(0);

                // Initialize timing on the very first frame
                if start_pts.is_none() {
                    start_pts = Some(pts);
                }

                // Track the first PTS of this loop iteration
                if loop_start_pts.is_none() {
                    loop_start_pts = Some(pts);
                }

                // Track the last PTS we processed
                last_pts = Some(pts);

                let loop_start_pts_value = loop_start_pts.unwrap();

                // Calculate frame time within this loop iteration
                let frame_time_in_loop = (pts - loop_start_pts_value) as f64 * time_base_f64;

                // Add the virtual time offset for seamless looping
                let total_video_time = virtual_time_offset + frame_time_in_loop;

                // Extract frame data and create message (do all heavy work first)
                let width = frame.width();
                let height = frame.height();

                // For YUV420P format, extract only the actual pixel data (not padding/stride)
                let y_stride = frame.stride(0);
                let u_stride = frame.stride(1);
                let v_stride = frame.stride(2);

                let y_data = frame.data(0);
                let u_data = frame.data(1);
                let v_data = frame.data(2);

                // Extract only the actual pixel data for each plane
                let mut combined_data = Vec::new();

                // Y plane: full resolution (width x height)
                for row in 0..height {
                    let start = row as usize * y_stride;
                    let end = start + width as usize;
                    combined_data.extend_from_slice(&y_data[start..end]);
                }

                // U plane: quarter resolution (width/2 x height/2)
                for row in 0..(height / 2) {
                    let start = row as usize * u_stride;
                    let end = start + (width / 2) as usize;
                    combined_data.extend_from_slice(&u_data[start..end]);
                }

                // V plane: quarter resolution (width/2 x height/2)
                for row in 0..(height / 2) {
                    let start = row as usize * v_stride;
                    let end = start + (width / 2) as usize;
                    combined_data.extend_from_slice(&v_data[start..end]);
                }

                // Create headers with current timestamp
                let mut header = base_header.clone();
                header.timestamp = Some(Timestamp::get_current_time());

                // Create YUV420 image message
                let yuv420_image = ImageYuv420 {
                    header: Some(header.clone()),
                    width,
                    height,
                    data: combined_data,
                };

                // Create ImageRawAny message
                let image_any = ImageRawAny {
                    header: Some(header),
                    image: Some(Image::Yuv420(yuv420_image)),
                };

                // Encode the message (do this before timing check)
                let message_encoded = match message_encoder.encode(&image_any) {
                    Ok(encoded) => encoded,
                    Err(e) => {
                        log::warn!("Failed to encode message, skipping frame: {:?}", e);
                        continue;
                    }
                };

                // NOW wait for the right time to publish
                let elapsed_since_start = start_wallclock.elapsed().as_secs_f64();
                if total_video_time > elapsed_since_start {
                    let wait_time = total_video_time - elapsed_since_start;
                    sleep(Duration::from_secs_f64(wait_time)).await;
                }

                // Publish immediately when the time is right
                if let Err(e) = publisher.put(&message_encoded).await {
                    log::warn!("Failed to publish frame: {:?}", e);
                    continue;
                }

                log::info!(
                    "Published YUV420 frame {}x{} PTS={} total_video_time={:.3}s (wallclock elapsed={:.3}s)",
                    width,
                    height,
                    pts,
                    total_video_time,
                    elapsed_since_start
                );
            }
        }

        // Calculate the actual duration of this loop iteration and preserve offset
        if let (Some(loop_start), Some(last)) = (loop_start_pts, last_pts) {
            let actual_loop_duration = (last - loop_start) as f64 * time_base_f64;
            virtual_time_offset += actual_loop_duration;
            log::info!("Loop completed. Actual duration: {:.3}s", actual_loop_duration);
        }

        log::info!("Stream ended, restarting video loop seamlessly... (virtual_time_offset: {:.3}s)", virtual_time_offset);
    }

}

use ffmpeg_next as ffmpeg;
use futures_util::StreamExt;
use make87::config::load_config_from_default_env;
use make87::encodings::{Encoder, ProtobufEncoder};
use make87::interfaces::zenoh::ZenohInterface;
use make87::models::ApplicationConfig;
use make87_messages::core::Header;
use make87_messages::google::protobuf::Timestamp;
use make87_messages::image::uncompressed::image_raw_any::Image;
use make87_messages::image::uncompressed::ImageRawAny;
use make87_messages::image::uncompressed::ImageYuv420;
use reqwest;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::error::Error;
use std::path::Path;
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::time::{sleep, Duration, Instant};
use uuid::Uuid;

fn get_config_value<'a>(cfg: &'a ApplicationConfig, key: &str) -> Option<&'a Value> {
    cfg.config.get(key)
}



fn get_url_hash(url: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(url.as_bytes());
    format!("{:x}", hasher.finalize())[..16].to_string()
}

async fn ensure_cache_dir() -> Result<(), Box<dyn Error + Send + Sync>> {
    fs::create_dir_all("cache").await?;
    Ok(())
}

async fn get_remote_file_size(url: &str) -> Result<Option<u64>, Box<dyn Error + Send + Sync>> {
    let client = reqwest::Client::new();
    let response = client.head(url).send().await?;
    
    if response.status().is_success() {
        Ok(response.headers()
            .get(reqwest::header::CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|s| s.parse().ok()))
    } else {
        Ok(None)
    }
}

async fn download_file(url: &str, file_path: &str) -> Result<(), Box<dyn Error + Send + Sync>> {
    log::info!("Downloading {} to {}", url, file_path);
    
    let client = reqwest::Client::new();
    let response = client.get(url).send().await?;
    
    if !response.status().is_success() {
        return Err(format!("Failed to download file: HTTP {}", response.status()).into());
    }
    
    let mut file = fs::File::create(file_path).await?;
    let mut stream = response.bytes_stream();
    
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        file.write_all(&chunk).await?;
    }
    
    file.flush().await?;
    log::info!("Downloaded {} successfully", file_path);
    Ok(())
}

async fn get_cached_file_path(url: &str) -> Result<String, Box<dyn Error + Send + Sync>> {
    ensure_cache_dir().await?;
    
    let url_hash = get_url_hash(url);
    let file_extension = Path::new(url)
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("mp4");
    let cache_file_path = format!("/cache/{}.{}", url_hash, file_extension);
    
    // Check if file exists and validate size
    if Path::new(&cache_file_path).exists() {
        let local_size = fs::metadata(&cache_file_path).await?.len();
        
        // Check remote file size
        match get_remote_file_size(url).await {
            Ok(Some(remote_size)) => {
                if local_size == remote_size {
                    log::info!("Using cached file: {} ({})", cache_file_path, url);
                    return Ok(cache_file_path);
                } else {
                    log::warn!("Cache file size mismatch. Local: {}, Remote: {}. Re-downloading.", local_size, remote_size);
                    fs::remove_file(&cache_file_path).await?;
                }
            },
            Ok(None) => {
                log::warn!("Could not get remote file size, using cached file anyway");
                return Ok(cache_file_path);
            },
            Err(e) => {
                log::warn!("Error checking remote file size: {}. Using cached file.", e);
                return Ok(cache_file_path);
            }
        }
    }
    
    // Download the file
    download_file(url, &cache_file_path).await?;
    Ok(cache_file_path)
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
                "highway_traffic_4k" => "https://make87-files.nyc3.digitaloceanspaces.com/example-apps/virtual-camera/highway_traffic.mp4",
                "highway_traffic_1080" => "https://make87-files.nyc3.digitaloceanspaces.com/example-apps/virtual-camera/highway_traffic_1080.mp4",
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

    // Download and cache the video file
    let local_file_path = get_cached_file_path(&url).await?;
    log::info!("Using local file: {}", local_file_path);

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
        let ictx_result = ffmpeg::format::input(&local_file_path);
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
        let mut packet_count = 0;
        let mut last_packet_time = Instant::now();

        for (stream, packet) in ictx.packets() {
            if stream.index() != video_stream_index {
                continue;
            }

            packet_count += 1;
            let current_time = Instant::now();

            // If we haven't received a video packet in 5 seconds, assume stream is stuck
            if current_time.duration_since(last_packet_time).as_secs() > 5 {
                log::warn!("No video packets received for 5 seconds, restarting stream...");
                break;
            }
            last_packet_time = current_time;

            // If we've processed too many packets without progress, break
            if packet_count > 1000 && loop_start_pts.is_none() {
                log::warn!("Processed {} packets without any valid frames, restarting stream...", packet_count);
                break;
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

                // Reset packet monitoring since we got a valid frame
                packet_count = 0;
                last_packet_time = current_time;

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

# Virtual Camera - make87 Application

A make87 application that provides a virtual camera stream by decoding MP4 video files and publishing raw YUV420 frames via Zenoh in real-time with seamless looping.

## What It Does

This application transforms any MP4 video into a continuous virtual camera feed that:

- **Caches video files locally** for reliable streaming (initial download may take time depending on file size and internet connection)
- **Decodes MP4 videos** using software decoding (FFmpeg)
- **Extracts YUV420 frames** without unnecessary color space conversions
- **Publishes frames** via Zenoh using the `ImageRawAny` message format
- **Maintains real-time playback** with precise frame timing
- **Loops infinitely** with seamless transitions between iterations
- **Generates unique entity paths** for each instance using random UUIDs

The virtual camera appears as a continuous, never-ending video stream to any make87 components subscribing to the published frames.

## Local Caching

The application automatically downloads and caches video files in a local `cache/` directory before streaming begins. This provides several benefits:

- **Reliable streaming** - No network interruptions or corruption during playback
- **Faster subsequent runs** - Cached files are reused until the remote file changes
- **Bandwidth efficiency** - Files are only downloaded when updated

⚠️ **Initial startup time**: The first run will include a download phase that depends on file size and internet connection speed. Large 4K videos may take several minutes to download on slower connections.

## Output

The application publishes to the Zenoh topic `raw_frames` with:

- **Message Type**: `make87_messages.image.uncompressed.ImageRawAny`
- **Format**: YUV420 (optimal for video without color conversion overhead)
- **Entity Path**: `/virtual_camera_{8-char-uuid}` (unique per instance)
- **Timing**: Real-time frame rate matching the source video (or bandwidth-limited)
- **Encoding**: Protocol Buffers (protobuf)

Each frame includes:
- Width and height dimensions
- YUV420 planar data (Y, U, V planes combined)
- Timestamp metadata
- Entity path for identification

## Configuration

The application supports two video source options through the make87 configuration system:

### Option 1: Predefined Videos

Select from curated video samples:

```yaml
video_source:
  type: predefined
  selection: city_walk  # or highway_traffic, drone_flight
```

### Option 2: Custom MP4 URL

Provide your own MP4 file via HTTP/HTTPS:

```yaml
video_source:
  type: custom_url
  url: "https://example.com/your-video.mp4"
```

## Predefined Videos

Three high-quality video samples are available for immediate use, each cropped to 40 seconds:

| Selection | Description | Duration | Source | License |
|-----------|-------------|----------|---------|---------|
| `city_walk` | Urban pedestrian activity | 40s (loops) | [YouTube](https://www.youtube.com/watch?v=7YIG8auXo28) | CC License |
| `highway_traffic` | Highway traffic footage | 40s (loops) | [YouTube](https://www.youtube.com/watch?v=TW3EH4cnFZo) | CC License |
| `drone_flight` | Aerial drone cinematography | 40s (loops) | [YouTube](https://www.youtube.com/watch?v=afG8UGtnRrg) | CC License |

*All predefined videos are sourced from Creative Commons licensed content, cropped to 40-second segments, and optimized for streaming performance (hevc encoded). The application automatically loops these videos infinitely.*

## Features

- **Seamless Looping**: Videos restart without timing gaps or visual discontinuities
- **Optimal Performance**: YUV420 format minimizes processing overhead
- **Precise Timing**: Frame scheduling accounts for encoding time to maintain accurate playback rate
- **Error Recovery**: Automatic restart on stream interruptions
- **Scalable**: Multiple instances can run simultaneously with unique entity paths
- **Memory Efficient**: ~33% less data than RGB formats

## Use Cases

- **Testing video processing pipelines** with consistent, repeatable input
- **Simulating camera feeds** without physical hardware
- **Providing reference video streams** for computer vision applications
- **Creating demo environments** with realistic video content
- **Load testing** video processing systems

The virtual camera integrates seamlessly into any make87 ecosystem requiring video input, appearing as a standard camera source to downstream components.
